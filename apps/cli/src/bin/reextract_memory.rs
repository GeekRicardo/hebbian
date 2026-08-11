//! 一次性运维：清空全部记忆并从所有历史对话重新抽取一遍（用户 2026-07-13 要求）。
//!
//! 策略 B（清空重来，已先 tar 备份到 ~/.hebbian/backups/）：
//!   1. 删掉 global + 每个 project 的记忆 .md 与 links.jsonl（importance/last_active 随 .md 一起没）
//!   2. 归零所有 session 的抽取游标（否则 extract 见游标已到末尾直接跳过）
//!   3. 按 session id 升序（≈时间顺序）逐个用指定模型链重抽——**顺序跑**：extract 内置去重靠
//!      「现有 L0 清单」作上下文，边抽边建，并发会让两个 session 互相看不到对方刚写的记忆、
//!      产生近似重复，故牺牲速度换去重质量。
//!
//! 模型链不读 settings.memory.models，改用命令行指定（本次 gpt-5.6-luna @ Sub2api），
//! 不动用户配置。走 agent_core 主抽取路径（`memory_extract::extract_with_models`），
//! 与后台自动抽取逐字节同逻辑。
//!
//! 跑法：`cargo run -p hebbian-cli --bin reextract_memory -- <provider_id> <model>`

use std::path::{Path, PathBuf};

use agent_core::storage::{memory, sessions};
use agent_core::storage::settings::MemoryModelRef;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,memory=info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let provider_id = args.next().expect("用法: reextract_memory <provider_id> <model> [test_session_id]");
    let model = args.next().expect("用法: reextract_memory <provider_id> <model> [test_session_id]");
    // 第三个参数：
    //   `--resume` = 补抽模式：不清空、不归零任何游标，直接遍历所有 session 调 extract。
    //                成功过的游标已推进 → extract 返回「无新消息」自动跳过（不重抽）；失败过的
    //                游标为空 → 重抽。用于某次全量因上游 5xx 中断后，只补跑失败的那批。
    //   其它非空值   = 单会话测试：不清空，只对这一个 session 重抽，验证模型链能抽出记忆。
    let third = args.next();
    let resume = third.as_deref() == Some("--resume");
    let test_session = if resume { None } else { third };
    let data_dir = dirs::home_dir().expect("无 home 目录").join(".hebbian");

    let models = vec![MemoryModelRef {
        provider_id: provider_id.clone(),
        model: model.clone(),
    }];

    eprintln!("[reextract] data_dir={} 模型={provider_id}/{model}", data_dir.display());

    if let Some(sid) = test_session {
        eprintln!("[reextract] === 单会话测试（不清空）：{sid} ===");
        let _ = memory::clear_cursor(&data_dir, &sid);
        match agent_core::memory_extract::extract_with_models(&data_dir, &sid, &models).await {
            Ok(Some(res)) => eprintln!(
                "[reextract] 测试成功：写入 {} 条，模型={}",
                res.written.len(),
                res.model
            ),
            Ok(None) => eprintln!("[reextract] 测试：无新消息可抽（该 session 可能为空）"),
            Err(e) => eprintln!("[reextract] 测试失败：{e}"),
        }
        return;
    }

    // ── 1. 清空全部记忆 .md + links（resume 补抽模式跳过：保留已抽成果）──
    if resume {
        eprintln!("[reextract] === 补抽模式：不清空、不归零游标，只补失败的（游标为空的）session ===");
    } else {
        let purged = purge_all_memory(&data_dir);
        eprintln!("[reextract] 已清空记忆文件：{purged} 个 .md/links 删除");
    }

    // ── 2. 列 session + 归零游标（resume 模式不归零：成功过的游标留在末尾 → extract 自动跳过）──
    let mut metas = sessions::list(&data_dir).unwrap_or_default();
    metas.sort_by(|a, b| a.id.cmp(&b.id)); // ≈时间顺序，去重上下文按时间自然生长
    if !resume {
        for m in &metas {
            let _ = memory::clear_cursor(&data_dir, &m.id);
        }
        eprintln!("[reextract] 游标归零：{} 个 session", metas.len());
    }

    // ── 3. 逐个重抽 ──
    let total = metas.len();
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut written_total = 0usize;
    for (i, m) in metas.iter().enumerate() {
        let n = i + 1;
        match agent_core::memory_extract::extract_with_models(&data_dir, &m.id, &models).await {
            Ok(Some(res)) => {
                ok += 1;
                written_total += res.written.len();
                eprintln!(
                    "[reextract] {n}/{total} {} 写入 {} 条（累计 {written_total}）",
                    m.id,
                    res.written.len()
                );
            }
            Ok(None) => {
                ok += 1;
                eprintln!("[reextract] {n}/{total} {} 跳过（无新消息）", m.id);
            }
            Err(e) => {
                failed += 1;
                eprintln!("[reextract] {n}/{total} {} 失败：{e}", m.id);
            }
        }
    }

    eprintln!(
        "[reextract] 完成：session {total}（成功 {ok} / 失败 {failed}），共写入 {written_total} 条记忆"
    );
}

/// 删掉 global + 所有 project 的记忆 .md 与 links.jsonl（保留目录本身 + .memory_log.jsonl 审计）。
/// 返回删除的文件数。
fn purge_all_memory(data_dir: &Path) -> usize {
    let mut roots: Vec<PathBuf> = vec![data_dir.join("memory")];
    if let Ok(entries) = std::fs::read_dir(data_dir.join("projects")) {
        for e in entries.flatten() {
            let mem = e.path().join("memory");
            if mem.is_dir() {
                roots.push(mem);
            }
        }
    }
    let mut deleted = 0usize;
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let is_md = p.extension().and_then(|s| s.to_str()) == Some("md");
            if is_md || name == "links.jsonl" {
                if std::fs::remove_file(&p).is_ok() {
                    deleted += 1;
                }
            }
        }
    }
    deleted
}
