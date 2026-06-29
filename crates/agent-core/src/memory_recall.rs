//! 自动联想记忆激活器（架构 §4.14 / 批5）。
//!
//! 把现状「首条塞全量 L0 清单 + 指望模型自己 ReadMemory」改为「门控器自动判断该不该
//! 联想、该联想才掏成本」。核心是经济权衡——判断「要不要联想」必须比「联想」便宜得多，
//! 靠成本分层实现：
//!
//! - **第1级 零成本触发器**（每轮跑，纯本地 microsecond 级）：用户消息分词查倒排表，
//!   零命中直接不联想（挡掉绝大多数轮次）。
//! - **第2级 激活扩散**（仅 gate 放行时，本地图遍历，零模型）：命中记忆作种子，沿
//!   `links.jsonl`（深睡建的边）扩散点亮邻居，按激活强度分 L0/L1/L2 档注入。
//! - 第3级 LLM 精排：本批不做（gate 已能挡多数；模型精排留后置增强）。
//!
//! 倒排表不单独落盘——从 `list_l0` 现场建（几百条记忆 microsecond 级），零一致性风险、
//! 零额外文件（不过度设计）。

use std::collections::{HashMap, HashSet};

use crate::storage::memory::{self, MemoryL0, MemoryScope};

/// 注入档位：激活强度决定给模型看多细（架构 §4.14 L0/L1/L2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallLevel {
    /// 弱激活：只进清单（id + summary 一行）。
    L0,
    /// 中激活：附 L1 概览段。
    L1,
    /// 强激活：附 L2 全文详情。
    L2,
}

/// 一条被激活的记忆 + 其强度与注入档。
#[derive(Debug, Clone)]
pub struct ActivatedMemory {
    pub l0: MemoryL0,
    /// 激活强度 [0,1]：种子 = 命中分；邻居 = 种子强度 × 边权 × 衰减。
    pub strength: f32,
    pub level: RecallLevel,
    /// 是否种子（直接命中查询，非扩散点亮）——供日志 / 调试区分。
    pub is_seed: bool,
}

/// 激活参数（可调，给默认值）。
#[derive(Debug, Clone, Copy)]
pub struct RecallParams {
    /// 扩散一跳的衰减系数。
    pub spread_decay: f32,
    /// 强度 ≥ 此值注入 L2 详情。
    pub l2_threshold: f32,
    /// 强度 ≥ 此值注入 L1 概览（低于则只 L0）。
    pub l1_threshold: f32,
    /// 最多注入多少条（控 token）。
    pub max_inject: usize,
}

impl Default for RecallParams {
    fn default() -> Self {
        Self {
            spread_decay: 0.5,
            l2_threshold: 0.75,
            l1_threshold: 0.4,
            max_inject: 12,
        }
    }
}

/// 激活结果：注入用的记忆列表 + 本次的种子 id 集（供话题漂移检测对比）。
#[derive(Debug, Clone, Default)]
pub struct Activation {
    pub activated: Vec<ActivatedMemory>,
    /// 种子 id 集（直接命中查询的记忆）——上下轮对比判话题漂移。
    pub seed_ids: HashSet<String>,
    /// 种子命中用到的 query token 集——话题漂移用 token 重合度判定。
    pub query_tokens: HashSet<String>,
}

/// 激活入口：对一个查询（当前 user message）在 global + 可选 project 两作用域上激活。
///
/// 纯本地、零模型调用。无记忆 / 零命中 → 返回空 activation（gate：不联想）。
pub fn activate(
    data_dir: &std::path::Path,
    project_workdir: Option<&std::path::Path>,
    query: &str,
    params: &RecallParams,
) -> Activation {
    // 汇集两作用域的记忆 + 边。
    let mut mems: Vec<MemoryL0> = memory::list_l0(data_dir, None, MemoryScope::Global).unwrap_or_default();
    let mut links = memory::load_links(data_dir, None, MemoryScope::Global).unwrap_or_default();
    if let Some(wd) = project_workdir {
        if let Ok(mut v) = memory::list_l0(data_dir, Some(wd), MemoryScope::Project) {
            mems.append(&mut v);
        }
        if let Ok(mut l) = memory::load_links(data_dir, Some(wd), MemoryScope::Project) {
            links.append(&mut l);
        }
    }
    if mems.is_empty() {
        return Activation::default();
    }

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return Activation::default();
    }

    // ── 第1级：倒排表种子激活 ──
    // 现场建倒排：token → [(mem_idx, weight)]。记忆侧 token 来自 summary+tags+category。
    let by_id: HashMap<&str, usize> = mems.iter().enumerate().map(|(i, m)| (m.id.as_str(), i)).collect();
    let mut seed_strength: HashMap<usize, f32> = HashMap::new();
    for (i, m) in mems.iter().enumerate() {
        let mtokens = memory_tokens(m);
        if mtokens.is_empty() {
            continue;
        }
        // 命中分 = query 与记忆 token 交集大小 / query token 数（占比，[0,1]）。
        let hits = query_tokens.iter().filter(|t| mtokens.contains(*t)).count();
        if hits > 0 {
            let score = (hits as f32 / query_tokens.len() as f32).min(1.0);
            seed_strength.insert(i, score);
        }
    }
    if seed_strength.is_empty() {
        // gate：零命中 → 不联想（挡掉绝大多数轮次）。
        return Activation {
            activated: Vec::new(),
            seed_ids: HashSet::new(),
            query_tokens,
        };
    }

    let seed_ids: HashSet<String> = seed_strength
        .keys()
        .map(|&i| mems[i].id.clone())
        .collect();

    // ── 第2级：沿 links 扩散点亮邻居 ──
    // 邻接表（无向）：id → [(邻居 id, 边权)]。
    let mut adj: HashMap<&str, Vec<(&str, f32)>> = HashMap::new();
    for l in &links {
        adj.entry(l.from.as_str()).or_default().push((l.to.as_str(), l.weight));
        adj.entry(l.to.as_str()).or_default().push((l.from.as_str(), l.weight));
    }
    // 一跳扩散：邻居强度 = 种子强度 × 边权 × 衰减。多种子点亮同一邻居取 max。
    let mut strength: HashMap<usize, f32> = seed_strength.clone();
    for (&si, &sval) in &seed_strength {
        let sid = mems[si].id.as_str();
        if let Some(neighbors) = adj.get(sid) {
            for &(nid, w) in neighbors {
                if let Some(&ni) = by_id.get(nid) {
                    let spread = sval * w * params.spread_decay;
                    let e = strength.entry(ni).or_insert(0.0);
                    if spread > *e {
                        *e = spread;
                    }
                }
            }
        }
    }

    // ── 分级 + 排序 + 截断 ──
    let mut activated: Vec<ActivatedMemory> = strength
        .into_iter()
        .map(|(i, s)| {
            let level = if s >= params.l2_threshold {
                RecallLevel::L2
            } else if s >= params.l1_threshold {
                RecallLevel::L1
            } else {
                RecallLevel::L0
            };
            ActivatedMemory {
                l0: mems[i].clone(),
                strength: s,
                level,
                is_seed: seed_strength.contains_key(&i),
            }
        })
        .collect();
    // 强度降序；同强度按 id 稳定排序，保证可复现。
    activated.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.l0.id.cmp(&b.l0.id))
    });
    activated.truncate(params.max_inject);

    Activation {
        activated,
        seed_ids,
        query_tokens,
    }
}

/// 话题漂移检测（架构 §3.1 / 批5）：当前 query token 集与上轮的重合度低于阈值 → 漂移。
/// 重合度 = |交集| / |当前 query token|（当前话题有多少落在上轮里）。
pub fn topic_drifted(prev_tokens: &HashSet<String>, cur_tokens: &HashSet<String>, threshold: f32) -> bool {
    if cur_tokens.is_empty() {
        return true;
    }
    if prev_tokens.is_empty() {
        return true; // 没有上轮 → 当漂移（首轮）
    }
    let overlap = cur_tokens.iter().filter(|t| prev_tokens.contains(*t)).count();
    let ratio = overlap as f32 / cur_tokens.len() as f32;
    ratio < threshold
}

/// 一条记忆的 token 集：summary + tags + category 合并分词。
fn memory_tokens(m: &MemoryL0) -> HashSet<String> {
    let mut s = tokenize(&m.summary);
    for t in &m.tags {
        s.extend(tokenize(t));
    }
    s.extend(tokenize(&m.category));
    s
}

/// 中英文混合分词（够召回不求精，不引外部分词器）：
/// - ASCII 字母 / 数字连续段 → 一个 token（小写），长度 ≥ 2 才留（滤掉单字母噪声）。
/// - 连续 CJK 字 → 相邻二字 bigram（「记忆系统」→ 记忆/忆系/系统），单字也留。
/// - 其余字符作分隔。
pub fn tokenize(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    let flush_ascii = |buf: &mut String, out: &mut HashSet<String>| {
        if buf.len() >= 2 {
            out.insert(buf.to_lowercase());
        }
        buf.clear();
    };
    let flush_cjk = |buf: &mut Vec<char>, out: &mut HashSet<String>| {
        if buf.len() == 1 {
            out.insert(buf[0].to_string());
        } else {
            for w in buf.windows(2) {
                out.insert(w.iter().collect());
            }
        }
        buf.clear();
    };

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

fn is_cjk(ch: char) -> bool {
    let c = ch as u32;
    (0x4E00..=0x9FFF).contains(&c)   // CJK 统一表意
        || (0x3400..=0x4DBF).contains(&c) // 扩展 A
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_ascii_and_cjk() {
        let t = tokenize("修复 partial sidecar 的 drop 问题");
        assert!(t.contains("partial"), "ASCII 词");
        assert!(t.contains("sidecar"));
        assert!(t.contains("drop"));
        assert!(t.contains("修复") || t.contains("复"), "CJK bigram");
    }

    #[test]
    fn tokenize_drops_single_ascii_letter() {
        let t = tokenize("a bb ccc");
        assert!(!t.contains("a"), "单字母滤掉");
        assert!(t.contains("bb"));
        assert!(t.contains("ccc"));
    }

    #[test]
    fn tokenize_cjk_bigram() {
        let t = tokenize("记忆系统");
        assert!(t.contains("记忆"));
        assert!(t.contains("忆系"));
        assert!(t.contains("系统"));
    }

    #[test]
    fn topic_drift_detection() {
        let prev: HashSet<String> = ["架构", "session", "记忆"].iter().map(|s| s.to_string()).collect();
        // 高重合 → 不漂移
        let same: HashSet<String> = ["架构", "session"].iter().map(|s| s.to_string()).collect();
        assert!(!topic_drifted(&prev, &same, 0.5));
        // 低重合 → 漂移
        let diff: HashSet<String> = ["天气", "吃饭", "电影"].iter().map(|s| s.to_string()).collect();
        assert!(topic_drifted(&prev, &diff, 0.5));
        // 首轮（prev 空）→ 漂移
        assert!(topic_drifted(&HashSet::new(), &same, 0.5));
    }

    /// 端到端：写两条记忆 + 一条边，验证种子命中 + 沿边扩散点亮邻居（激活器核心属性）。
    #[test]
    fn activate_seeds_then_spreads_along_links() {
        use crate::storage::memory::{save_links, write, MemoryKind, MemoryLink};

        let dd = std::env::temp_dir().join(format!("heb-recall-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dd).unwrap();

        // A：直接命中查询「partial sidecar」；B：与 A 有强边，但本身不含查询词。
        write(&dd, None, MemoryScope::Global, "a-partial", MemoryKind::Stable,
            "bugfix", &["partial".into()], "partial sidecar 落盘用临时文件原子替换", "## 详情\nA 正文").unwrap();
        write(&dd, None, MemoryScope::Global, "b-buf", MemoryKind::Episode,
            "bug", &["drop".into()], "BufWriter 在进程被杀时 Drop 不跑导致丢数据", "## 详情\nB 正文").unwrap();
        // A—B 强边（深睡建的「症状↔根因」）。
        save_links(&dd, None, MemoryScope::Global, &[MemoryLink {
            from: "global/a-partial".into(),
            to: "global/b-buf".into(),
            weight: 0.9,
            updated_at: "2026-06-23T00:00:00Z".into(),
        }]).unwrap();

        let act = activate(&dd, None, "partial sidecar 怎么实现的", &RecallParams::default());

        // A 应是种子（直接命中 partial/sidecar）。
        assert!(act.seed_ids.contains("global/a-partial"), "A 应命中为种子");
        let ids: Vec<&str> = act.activated.iter().map(|a| a.l0.id.as_str()).collect();
        assert!(ids.contains(&"global/a-partial"), "A 应被激活");
        // B 不含查询词，但应被 A 沿边扩散点亮——这正是「联想」。
        assert!(ids.contains(&"global/b-buf"), "B 应被 A 沿 links 扩散点亮（联想）");
        let b = act.activated.iter().find(|a| a.l0.id == "global/b-buf").unwrap();
        assert!(!b.is_seed, "B 是扩散点亮的，非种子");
        assert!(b.strength < 1.0 && b.strength > 0.0, "B 强度被边权×衰减压低");
    }

    /// gate：查询与任何记忆都不沾边 → 零激活（挡掉无关轮次）。
    #[test]
    fn activate_zero_hit_returns_empty() {
        use crate::storage::memory::{write, MemoryKind};
        let dd = std::env::temp_dir().join(format!("heb-recall-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dd).unwrap();
        write(&dd, None, MemoryScope::Global, "a", MemoryKind::Stable,
            "c", &[], "数据库连接池配置", "正文").unwrap();
        let act = activate(&dd, None, "今天天气真好适合爬山", &RecallParams::default());
        assert!(act.activated.is_empty(), "无关查询应零激活");
    }
}
