//! Session 目录布局（架构 §4.9.1 / §6.1）。
//!
//! 每段对话一个目录：
//!
//! ```text
//! ~/.hebbian/sessions/<session_id>/
//! ├── session.jsonl
//! ├── meta.json
//! ├── tool_results/
//! ├── compactions/
//! ├── plans/
//! └── partial/
//!     └── <msg_id>.partial.jsonl
//! ```
//!
//! 当前阶段 `session.jsonl` 主体写入仍由 [`common::storage::sessions`] 处理；本模块负责
//! 提供新布局的路径计算 + 目录初始化 + meta.json + partial sidecar，配合 Recorder
//! 的流式中间态落盘 + 中断恢复（架构 §4.9.3 / §10.8）。

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use common::AppResult;

use super::lock;

/// session 根目录：`~/.hebbian/sessions/<id>/`。
pub fn session_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join("sessions").join(session_id)
}

pub fn session_jsonl_path(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("session.jsonl")
}

pub fn meta_path(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("meta.json")
}

pub fn partial_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("partial")
}

pub fn partial_path(data_dir: &Path, session_id: &str, msg_id: &str) -> PathBuf {
    partial_dir(data_dir, session_id).join(format!("{msg_id}.partial.jsonl"))
}

fn partial_live_path(data_dir: &Path, session_id: &str, msg_id: &str) -> PathBuf {
    partial_dir(data_dir, session_id).join(format!("{msg_id}.partial.jsonl.live"))
}

/// 架构 §4.12.3：后台 Bash 进程的输出日志目录。
/// 每个 BackgroundShell 在这里落一份 `<task_id>.log`，与内存 tail buffer 双轨。
pub fn bg_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("bg")
}

/// 确保 session 主体目录与所有子目录都存在。
pub fn ensure_session_dirs(data_dir: &Path, session_id: &str) -> AppResult<()> {
    let root = session_dir(data_dir, session_id);
    for sub in [
        root.clone(),
        root.join("tool_results"),
        root.join("compactions"),
        root.join("plans"),
        root.join("partial"),
        root.join("bg"),
    ] {
        std::fs::create_dir_all(&sub)?;
    }
    Ok(())
}

/// 架构 §4.9.3：`{yyyymmddHHmm}-{shortUuid}`。
///
/// 新 session 推荐使用本函数生成 id；老 session 走 uuid 的 v4 仍然可被识别——
/// 列表与加载按目录名当 id，不解析格式。
pub fn new_session_id() -> String {
    let now = Utc::now().format("%Y%m%d%H%M");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{now}-{}", &suffix[..8])
}

// ──────────────────────────────────────────────────────────────────────────
// meta.json
// ──────────────────────────────────────────────────────────────────────────

/// 写入 session/meta.json 的最小字段集（架构 §4.9.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDirMeta {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub agent: String,
    pub workdir: Option<PathBuf>,
    pub provider: String,
    pub model: String,
    /// 流式中断时间戳；首次落 partial 时不写，恢复时由
    /// [`recover_interrupted_partials`] 填上。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastInterruptedAt"
    )]
    pub last_interrupted_at: Option<i64>,
}

pub fn save_meta(data_dir: &Path, meta: &SessionDirMeta) -> AppResult<()> {
    ensure_session_dirs(data_dir, &meta.session_id)?;
    let path = meta_path(data_dir, &meta.session_id);
    let bytes = serde_json::to_vec_pretty(meta)?;
    lock::write_atomic(&path, &bytes)
}

pub fn load_meta(data_dir: &Path, session_id: &str) -> AppResult<Option<SessionDirMeta>> {
    let p = meta_path(data_dir, session_id);
    if !p.exists() {
        return Ok(None);
    }
    let bytes = lock::read_locked(&p)?;
    Ok(serde_json::from_slice(&bytes).ok())
}

// ──────────────────────────────────────────────────────────────────────────
// partial sidecar
// ──────────────────────────────────────────────────────────────────────────

/// partial 文件单行格式。`text` / `reasoning` / `tool_call` / `tool_result` 四类增量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PartialFragment {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        arguments_chunk: String,
    },
    /// 工具执行完成后的结果（架构 §4.9.3 中断恢复）。
    /// `ToolCallFinished` 到达时立刻落盘，保证强退后恢复时 tool call 有 result。
    ToolResult {
        index: u32,
        result: String,
        #[serde(default)]
        duration_ms: u64,
    },
}

pub fn append_partial(
    data_dir: &Path,
    session_id: &str,
    msg_id: &str,
    fragment: &PartialFragment,
) -> AppResult<()> {
    let dir = partial_dir(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;
    let path = partial_path(data_dir, session_id, msg_id);
    let line = serde_json::to_string(fragment)?;
    lock::append_jsonl(&path, &line)
}

/// partial 活性锁（架构 §4.9.3 恢复边界）。
///
/// 流式写入方在整个 run 期间排他持有 `<msg_id>.partial.jsonl.live`；
/// [`recover_interrupted_partials`] 据此区分「死进程残留」与「活 run 正在写」——
/// try-lock 拿不到就跳过，不折叠不删除。进程被 SIGKILL / 崩溃时锁由 OS 自动释放，
/// 崩溃残留照常恢复。锁文件本身不删（与 `.lock` 同类的零字节哨兵），
/// 由 [`delete_partial`] 统一清理。
pub struct PartialLiveGuard {
    _file: std::fs::File,
}

impl PartialLiveGuard {
    pub fn acquire(data_dir: &Path, session_id: &str, msg_id: &str) -> AppResult<Self> {
        let dir = partial_dir(data_dir, session_id);
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(partial_live_path(data_dir, session_id, msg_id))?;
        fs2::FileExt::lock_exclusive(&file)
            .map_err(|e| common::AppError::msg(format!("acquire partial live lock: {e}")))?;
        Ok(Self { _file: file })
    }

    /// 非阻塞抢锁：抢到返回 `Some`（持锁直到 drop），抢不到（写者还活、或别的恢复者正在
    /// 折叠这条死 partial）返回 `None`。用于中断恢复折盘——保证「同一死 partial 跨进程只被
    /// 一个恢复者折叠一次」，避免两 surface 并发打开同一崩溃 session 时把 Interrupted 段
    /// 重复折两份进 jsonl（§7.8.5）。
    pub fn try_acquire(data_dir: &Path, session_id: &str, msg_id: &str) -> Option<Self> {
        let dir = partial_dir(data_dir, session_id);
        std::fs::create_dir_all(&dir).ok()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(partial_live_path(data_dir, session_id, msg_id))
            .ok()?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Some(Self { _file: file }),
            Err(_) => None,
        }
    }
}

/// 写入方是否仍持有该 partial 的活性锁。open 失败按「不存活」处理——
/// 老 partial（修复前产生）没有 `.live` 文件，open 会新建后立刻拿到锁。
fn partial_writer_alive(data_dir: &Path, session_id: &str, msg_id: &str) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(partial_live_path(data_dir, session_id, msg_id))
    else {
        return false;
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// 清空 partial 内容但保留活性锁——drain 边界把已落盘段对应的中间态清零用。
pub fn clear_partial(data_dir: &Path, session_id: &str, msg_id: &str) -> AppResult<()> {
    let path = partial_path(data_dir, session_id, msg_id);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn delete_partial(data_dir: &Path, session_id: &str, msg_id: &str) -> AppResult<()> {
    let path = partial_path(data_dir, session_id, msg_id);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    // 哨兵文件一并清理：lock::append_jsonl 的 `.lock` 与活性锁 `.live`。
    // 先删 partial 再删哨兵——中间窗口里其他进程的恢复扫描看不到 partial，无害。
    for sentinel in [
        partial_live_path(data_dir, session_id, msg_id),
        PathBuf::from(format!("{}.lock", path.display())),
    ] {
        if sentinel.exists() {
            let _ = std::fs::remove_file(&sentinel);
        }
    }
    Ok(())
}

/// 中断恢复结果：每个 msg_id 累出文本 + reasoning + tool_call 串 + tool_result。
#[derive(Debug, Default, Clone)]
pub struct RecoveredPartial {
    pub msg_id: String,
    pub text: String,
    pub reasoning: String,
    /// 按 index 聚合的 tool_call arguments 累积字符串（name, arguments）。
    pub tool_calls: std::collections::BTreeMap<u32, (Option<String>, String)>,
    /// 按 index 聚合的 tool 执行结果（result, duration_ms）。
    pub tool_results: std::collections::BTreeMap<u32, (String, u64)>,
    /// 写入方是否仍存活（`.live` 锁仍被持有 = run 还在跑）。`true` = 活流式内容，
    /// 调用方应**只读出来渲染、不折盘、不标中断**（hebcore run 收尾会正式落盘，
    /// 折盘会重复两份）；`false` = 真·中断残留，按老路径折成 Interrupted message。
    pub alive: bool,
}

/// 扫描 partial 目录，把每个残留文件聚合并返回。返回后调用方负责把内容写到
/// `session.jsonl` 并删除 partial 文件（架构 §10.8 / §4.9.3）。
pub fn recover_interrupted_partials(
    data_dir: &Path,
    session_id: &str,
) -> AppResult<Vec<RecoveredPartial>> {
    let dir = partial_dir(data_dir, session_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // <msg_id>.partial.jsonl
        let Some(msg_id) = name.strip_suffix(".partial.jsonl") else {
            continue;
        };
        // 活性检测：写入方仍在流式写这个 partial（`.live` 锁被持有）= run 还在跑。
        // **不再跳过**——活的也读出来返回（标 alive），让 surface 加载会话时能渲染
        // 进行中的流式内容；调用方按 alive 区分处理：活的只渲染不折盘（hebcore run
        // 收尾会正式落盘，折盘会重复两份），死的才折成 Interrupted message 落盘。
        let alive = partial_writer_alive(data_dir, session_id, msg_id);
        let bytes = match lock::read_locked(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "读 partial 失败");
                continue;
            }
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut recovered = RecoveredPartial {
            msg_id: msg_id.to_string(),
            alive,
            ..Default::default()
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<PartialFragment>(line) {
                Ok(PartialFragment::Text { text }) => recovered.text.push_str(&text),
                Ok(PartialFragment::Reasoning { text }) => recovered.reasoning.push_str(&text),
                Ok(PartialFragment::ToolCall {
                    index,
                    name,
                    arguments_chunk,
                }) => {
                    let entry = recovered
                        .tool_calls
                        .entry(index)
                        .or_insert((None, String::new()));
                    if entry.0.is_none() {
                        entry.0 = name;
                    }
                    entry.1.push_str(&arguments_chunk);
                }
                Ok(PartialFragment::ToolResult {
                    index,
                    result,
                    duration_ms,
                }) => {
                    recovered
                        .tool_results
                        .entry(index)
                        .and_modify(|(r, d)| {
                            *r = result.clone();
                            *d = duration_ms;
                        })
                        .or_insert((result, duration_ms));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "解析 partial 行失败");
                }
            }
        }
        out.push(recovered);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-sd-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn new_session_id_has_expected_shape() {
        let id = new_session_id();
        // yyyymmddHHmm = 12 字符；- 1 字符；short uuid 8 字符
        assert_eq!(id.len(), 12 + 1 + 8, "id = {id}");
        assert!(id.chars().nth(12) == Some('-'));
    }

    #[test]
    fn partial_roundtrip_and_recovery() {
        let dir = tmp("partial");
        let sid = "20260511-abc12345";
        ensure_session_dirs(&dir, sid).unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::Text {
                text: "hello".into(),
            },
        )
        .unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::Text {
                text: " world".into(),
            },
        )
        .unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::ToolCall {
                index: 0,
                name: Some("Bash".into()),
                arguments_chunk: r#"{"command""#.into(),
            },
        )
        .unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::ToolCall {
                index: 0,
                name: None,
                arguments_chunk: r#":"ls"}"#.into(),
            },
        )
        .unwrap();

        let recovered = recover_interrupted_partials(&dir, sid).unwrap();
        assert_eq!(recovered.len(), 1);
        let r = &recovered[0];
        assert_eq!(r.msg_id, "msg1");
        assert_eq!(r.text, "hello world");
        let tc = r.tool_calls.get(&0).unwrap();
        assert_eq!(tc.0.as_deref(), Some("Bash"));
        assert_eq!(tc.1, r#"{"command":"ls"}"#);
    }

    /// 回归（架构 §7.8.5 步骤⑥）：活 run 的 partial 现在**返回但标 `alive=true`**——
    /// 不再无条件跳过，让 surface 加载时能读出流式内容渲染；上层据 alive 决定不折盘
    /// （见 sessions::recover_and_append_interrupted_partials / load_with_partial_recovery）。
    /// 写者退出后同一 partial 标 `alive=false`，按中断残留折叠。
    #[test]
    fn recover_skips_partial_while_writer_alive() {
        let dir = tmp("partial-live");
        let sid = "20260611-live1234";
        ensure_session_dirs(&dir, sid).unwrap();

        let guard = PartialLiveGuard::acquire(&dir, sid, "msg1").unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::Text {
                text: "streaming...".into(),
            },
        )
        .unwrap();

        // 写入方存活（锁被持有）：返回但标 alive=true，文件保持原样（上层不折盘）。
        let recovered = recover_interrupted_partials(&dir, sid).unwrap();
        assert_eq!(recovered.len(), 1, "活 partial 应被读出返回");
        assert!(recovered[0].alive, "活 partial 应标 alive=true");
        assert_eq!(recovered[0].text, "streaming...");
        assert!(partial_path(&dir, sid, "msg1").exists());

        // 写入方退出（锁释放，等价进程崩溃后 OS 释放）：标 alive=false，按残留恢复。
        drop(guard);
        let recovered = recover_interrupted_partials(&dir, sid).unwrap();
        assert_eq!(recovered.len(), 1);
        assert!(!recovered[0].alive, "写者退出后应标 alive=false");
        assert_eq!(recovered[0].text, "streaming...");
    }

    /// delete_partial 连同 `.live` / `.lock` 哨兵一起清理，不留目录垃圾。
    #[test]
    fn delete_partial_cleans_sentinels() {
        let dir = tmp("partial-clean");
        let sid = "20260611-clean123";
        ensure_session_dirs(&dir, sid).unwrap();
        let guard = PartialLiveGuard::acquire(&dir, sid, "msg1").unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::Text { text: "x".into() },
        )
        .unwrap();
        drop(guard);
        delete_partial(&dir, sid, "msg1").unwrap();
        let leftover: Vec<_> = std::fs::read_dir(partial_dir(&dir, sid))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftover.is_empty(), "残留哨兵: {leftover:?}");
    }
}
