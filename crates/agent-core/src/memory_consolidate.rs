//! 深睡整合（架构 §4.14 / §3.1）。
//!
//! 浅睡（`memory_extract`）只把零散事实抽出来落盘；深睡在用户**不等待**的时段把它们
//! 「想」成有结构的记忆网络。当前实现核心一趟：**联结建边**——让模型扫全部记忆，
//! 找出语义相关的两两关系，产出带权关联边落盘 `links.jsonl`，供激活扩散注入（批5）使用。
//!
//! 触发：① session 空闲 ≥ T 分钟（`WakeupScheduler` 的 idle 哨兵，实时）；② 回填脚本
//! 按历史两轮时间戳（离线）。两条路径共用 [`decide_sleep_depth`]——同一个空闲时长映射到
//! 同一套睡眠深度，不写两套逻辑。
//!
//! 睡得越久越深（呼应 sleep-time compute「睡得越久收益越大」）：空闲越长跑的整合越完整。

use std::path::Path;

use crate::storage::memory::{self, mem_log, mem_warn, MemoryL0, MemoryLink, MemoryScope};
use crate::storage::settings;
use crate::tools::memory_project_workdir;

/// 每个模型最多重试次数（与浅睡一致）。
const MAX_RETRIES: u32 = 3;
const CONSOLIDATE_MAX_TOKENS: u32 = 4096;
/// 单趟建边喂给模型的记忆条数上限——太多会超 token / 稀释注意力。超出则跳过（等记忆
/// 增长触发的下一次深睡分批，或回填脚本显式处理）。
const MAX_MEMORIES_PER_PASS: usize = 80;

/// 睡眠深度（架构 §3.1）：空闲时长决定跑几趟整合。趟是递增包含的——`Deep` 含 `Light`
/// 的全部，`Full` 含 `Deep` 的全部。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepDepth {
    /// 空闲不足 T：连续工作，不睡。
    None,
    /// ≥ T（默认 10min，喝杯水级）：去重整合 + tag 归一（当前最小实现：建边）。
    Light,
    /// ≥ 1h（午饭 / 会议级）：加联结建边。
    Deep,
    /// 跨天（睡了一觉级）：加升华 + 遗忘衰减。
    Full,
}

impl SleepDepth {
    pub fn label(self) -> &'static str {
        match self {
            SleepDepth::None => "none",
            SleepDepth::Light => "light",
            SleepDepth::Deep => "deep",
            SleepDepth::Full => "full",
        }
    }
}

/// 把空闲时长（分钟）映射到睡眠深度（架构 §3.1）。实时 idle 哨兵与离线回填共用此函数。
///
/// `idle_threshold_min`：触发深睡的最小空闲（来自设置，默认 10min）。低于它不睡。
/// 分档阈值：≥ threshold → Light；≥ 60min → Deep；≥ 8h(480min) → Full。
pub fn decide_sleep_depth(idle_minutes: f64, idle_threshold_min: f64) -> SleepDepth {
    if idle_minutes < idle_threshold_min {
        SleepDepth::None
    } else if idle_minutes < 60.0 {
        SleepDepth::Light
    } else if idle_minutes < 480.0 {
        SleepDepth::Deep
    } else {
        SleepDepth::Full
    }
}

/// 深睡整合入口（架构 §4.14）。算出睡眠深度 → 对 global 与当前 project 两个作用域各跑
/// 一次联结建边。整合作用在「该作用域全部记忆」上，与触发它的 session 无强绑定
/// （session_id 仅用于日志定位与拿 project workdir）。
pub async fn consolidate_for_session(
    data_dir: &Path,
    session_id: &str,
    idle_minutes: f64,
    idle_threshold_min: f64,
) {
    let depth = decide_sleep_depth(idle_minutes, idle_threshold_min);
    if depth == SleepDepth::None {
        mem_warn!(
            "Sleep",
            "idle 触发但空闲不足（{idle_minutes:.1}min < {idle_threshold_min:.0}min）跳过 session={session_id}"
        );
        return;
    }
    mem_log!(
        "Sleep",
        "深睡开始 session={session_id} 空闲={idle_minutes:.1}min 深度={}",
        depth.label()
    );

    let app_settings = settings::load(data_dir);
    if app_settings.memory.models.is_empty() {
        mem_warn!("Sleep", "无可用记忆模型，深睡跳过");
        return;
    }
    let models = &app_settings.memory.models;

    // 该 session 绑定的 project（若有）→ project 作用域；global 总是整合。
    let project_workdir = session_workdir(data_dir, session_id);

    let global_n = consolidate_scope(data_dir, None, MemoryScope::Global, models).await;
    let project_n = match project_workdir.as_deref() {
        Some(wd) => consolidate_scope(data_dir, Some(wd), MemoryScope::Project, models).await,
        None => 0,
    };

    mem_log!(
        "Sleep",
        "深睡完成 session={session_id} 建边 global={global_n} project={project_n}"
    );
}

/// 对一个作用域跑联结建边：列全部记忆 → 模型找关联 → 落盘 links.jsonl。返回边数。
async fn consolidate_scope(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
    models: &[settings::MemoryModelRef],
) -> usize {
    let mems = match memory::list_l0(data_dir, workdir, scope) {
        Ok(m) => m,
        Err(e) => {
            mem_warn!("Link", "{} 列记忆失败：{e}", scope.prefix());
            return 0;
        }
    };
    if mems.len() < 2 {
        return 0; // 不足两条无从建边
    }
    if mems.len() > MAX_MEMORIES_PER_PASS {
        mem_warn!(
            "Link",
            "{} 记忆 {} 条超单趟上限 {MAX_MEMORIES_PER_PASS}，本次跳过建边",
            scope.prefix(),
            mems.len()
        );
        return 0;
    }

    let prompt = build_link_prompt(&mems);
    let raw = match run_chain(data_dir, models, &prompt).await {
        Some(r) => r,
        None => {
            mem_warn!("Link", "{} 建边模型链全失败", scope.prefix());
            return 0;
        }
    };
    let valid_ids: std::collections::HashSet<&str> = mems.iter().map(|m| m.id.as_str()).collect();
    let links = parse_links(&raw, &valid_ids);
    if links.is_empty() {
        return 0;
    }
    if let Err(e) = memory::save_links(data_dir, workdir, scope, &links) {
        mem_warn!("Link", "{} 边落盘失败：{e}", scope.prefix());
        return 0;
    }
    links.len()
}

/// 简易 fallback 链：按 models 顺序、每个最多重试 MAX_RETRIES 次。复用浅睡的
/// `call_memory_model`，深睡传自己的 system + prompt。
async fn run_chain(
    data_dir: &Path,
    models: &[settings::MemoryModelRef],
    prompt: &str,
) -> Option<String> {
    for m in models {
        for attempt in 1..=MAX_RETRIES {
            match crate::memory_extract::call_memory_model(
                data_dir,
                &m.provider_id,
                &m.model,
                LINK_SYSTEM,
                prompt,
                CONSOLIDATE_MAX_TOKENS,
            )
            .await
            {
                Ok(text) => return Some(text),
                Err(e) => mem_warn!(
                    "Link",
                    "模型 {}/{} 第{attempt}次失败：{e}",
                    m.provider_id,
                    m.model
                ),
            }
        }
    }
    None
}

const LINK_SYSTEM: &str = "你是 Hebbian 深睡整合器的『联结』阶段。输入是一批记忆（id + 摘要 + 标签）。\
    找出语义上真正相关的两两记忆，建立带权关联边——比如「症状↔根因」「设计决策↔它解决的问题」\
    「同一子系统」。只输出 JSON 数组，每项 {\"from\":\"记忆id\",\"to\":\"记忆id\",\"weight\":0.x,\"why\":\"一句关联理由\"}。\
    weight∈[0,1]，越相关越高。只连真正相关的，宁缺毋滥；不相关的不连。只输出 JSON 数组。";

fn build_link_prompt(mems: &[MemoryL0]) -> String {
    let mut s = String::from("为下面这批记忆建立关联边：\n\n");
    for m in mems {
        let tags = m.tags.join(",");
        s.push_str(&format!("- id={} [{}] {}\n", m.id, tags, m.summary));
    }
    s.push_str("\n只输出 JSON 数组。");
    s
}

/// 容错解析模型返回的边数组。过滤：端点必须是真实存在的记忆 id、weight 落在 (0,1]、
/// 非自环。`updated_at` 统一打当前时间。
fn parse_links(raw: &str, valid_ids: &std::collections::HashSet<&str>) -> Vec<MemoryLink> {
    let start = raw.find('[');
    let end = raw.rfind(']');
    let json = match (start, end) {
        (Some(s), Some(e)) if e > s => &raw[s..=e],
        _ => return Vec::new(),
    };
    #[derive(serde::Deserialize)]
    struct RawLink {
        from: String,
        to: String,
        weight: f32,
        #[serde(default)]
        why: String,
    }
    let parsed: Vec<RawLink> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            mem_warn!("Link", "边 JSON 解析失败：{e}");
            return Vec::new();
        }
    };
    let now = chrono::Utc::now().to_rfc3339();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for l in parsed {
        if l.from == l.to
            || !valid_ids.contains(l.from.as_str())
            || !valid_ids.contains(l.to.as_str())
            || !(l.weight > 0.0 && l.weight <= 1.0)
        {
            continue;
        }
        // 无向去重：(a,b) 与 (b,a) 视为同一条，保留先出现的。
        let key = if l.from < l.to {
            (l.from.clone(), l.to.clone())
        } else {
            (l.to.clone(), l.from.clone())
        };
        if !seen.insert(key) {
            continue;
        }
        let _ = l.why; // why 暂不落盘（links.jsonl 不含理由字段，保持精简）；日志可见
        out.push(MemoryLink {
            from: l.from,
            to: l.to,
            weight: l.weight,
            updated_at: now.clone(),
        });
    }
    out
}

/// 拿 session 绑定的 project workdir（用于 project 作用域整合）。无绑定 → None。
fn session_workdir(data_dir: &Path, session_id: &str) -> Option<std::path::PathBuf> {
    let session = crate::storage::sessions::load(data_dir, session_id).ok()?;
    session
        .workdir
        .as_deref()
        .and_then(memory_project_workdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_depth_thresholds() {
        let t = 10.0;
        assert_eq!(decide_sleep_depth(3.0, t), SleepDepth::None, "连续工作不睡");
        assert_eq!(decide_sleep_depth(10.0, t), SleepDepth::Light, "刚到 T → light");
        assert_eq!(decide_sleep_depth(45.0, t), SleepDepth::Light);
        assert_eq!(decide_sleep_depth(60.0, t), SleepDepth::Deep, "1h → deep");
        assert_eq!(decide_sleep_depth(300.0, t), SleepDepth::Deep);
        assert_eq!(decide_sleep_depth(480.0, t), SleepDepth::Full, "8h → full");
        assert_eq!(decide_sleep_depth(2000.0, t), SleepDepth::Full, "跨天 → full");
    }

    #[test]
    fn threshold_zero_means_always_light_above_zero() {
        assert_eq!(decide_sleep_depth(0.0, 0.0), SleepDepth::Light);
    }

    #[test]
    fn parse_links_filters_invalid() {
        let ids: std::collections::HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let raw = r#"[
            {"from":"a","to":"b","weight":0.8,"why":"相关"},
            {"from":"a","to":"a","weight":0.5,"why":"自环应弃"},
            {"from":"a","to":"zzz","weight":0.5,"why":"端点不存在应弃"},
            {"from":"b","to":"c","weight":1.5,"why":"权重越界应弃"},
            {"from":"b","to":"a","weight":0.9,"why":"与首条无向重复应弃"}
        ]"#;
        let links = parse_links(raw, &ids);
        assert_eq!(links.len(), 1, "只应保留 a-b 一条有效边");
        assert_eq!(links[0].from, "a");
        assert_eq!(links[0].to, "b");
    }

    #[test]
    fn parse_links_empty_on_garbage() {
        let ids: std::collections::HashSet<&str> = ["a"].into_iter().collect();
        assert!(parse_links("没有 JSON", &ids).is_empty());
        assert!(parse_links("[]", &ids).is_empty());
    }
}

