//! models.dev 模型目录缓存（架构 §6.2 / §7 扩展）。
//!
//! 数据来源：<https://models.dev/models.json>，覆盖 180+ 模型的元数据（context/output 大小、
//! 输入/输出模态、reasoning 支持等）。前端用这些数据给模型卡片打标签，让 ModelPicker 的每行
//! 显示丰富的元信息，而不是光秃秃的 model id。
//!
//! ## 三层回退
//!
//! 1. 磁盘缓存：`~/.hebbian/models_catalog.json`（含 etag + 时间戳）
//! 2. 内置兜底：编译期 `include_bytes!` 的 `models_catalog_fallback.json`
//!
//! ## 更新策略
//!
//! 24h TTL + ETag。[`read_catalog`] 会返回可用数据（磁盘 or 兜底）；如 TTL 过期，调用方
//! 应 fire-and-forget 调 [`refresh_catalog`] 联网拉取。联网带 `If-None-Match: <etag>`，
//! 304 时不覆盖磁盘。刷新失败时保留旧缓存，不影响使用。
//!
//! ## 文件锁
//!
//! 写盘走 [`lock::write_atomic`]（atomic rename + 排他锁），与 §6.3 一致，多窗口/多进程
//! 并发刷新互不覆盖。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};

/// 联网刷新的最小间隔：24h 内命中缓存直接返回，不联网。
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// 联网请求的超时；models.dev 通常 < 1s，留 10s 余量防止网络抖动卡死启动。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 兜底 JSON：编译期嵌入，离线也能有完整目录。
const FALLBACK_JSON: &str = include_str!("./models_catalog_fallback.json");

/// 缓存文件名。
const CACHE_FILENAME: &str = "models_catalog.json";

/// 单个模型条目，与 models.dev 的 schema 对齐。
///
/// 字段全部 optional 化——models.dev 不保证每个模型都齐，缺失字段按 `None` / 默认值处理，
/// 前端渲染时对 `None` 不显示对应徽章。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// 模型 id，形如 `anthropic/claude-opus-4-5`。
    pub id: String,
    /// 人类可读名字。
    pub name: Option<String>,
    /// 模型家族（用于按组渲染卡片网格）。
    pub family: Option<String>,
    /// 是否支持 reasoning / thinking。
    #[serde(default)]
    pub reasoning: bool,
    /// 是否支持工具调用。
    #[serde(default)]
    pub tool_call: bool,
    /// 是否支持附件（图片/pdf 等）。
    #[serde(default)]
    pub attachment: bool,
    /// 支持的 effort 级别列表（如 ["low", "medium", "high", "extra"]），null 表示不限制。
    #[serde(default)]
    pub effort: Option<Vec<String>>,
    /// 推理配置的类型（如 "thinking"、"reasoning_effort"），null 表示无。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 是否支持 thinking（与 reasoning_effort 互斥），null 表示不限制。
    #[serde(default)]
    pub thinking: Option<bool>,
    /// 输入/输出模态。
    pub modalities: Option<CatalogModalities>,
    /// context / output / input 大小限制。
    pub limit: Option<CatalogLimits>,
    /// 训练数据截止日期。
    pub knowledge: Option<String>,
    /// 发布日期。
    pub release_date: Option<String>,
    /// 最近更新日期。
    pub last_updated: Option<String>,
}

/// 输入/输出模态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModalities {
    /// 输入模态：text / image / audio / video / pdf 等。
    #[serde(default)]
    pub input: Vec<String>,
    /// 输出模态：text / image / audio 等。
    #[serde(default)]
    pub output: Vec<String>,
}

/// 模型大小限制（token 数）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogLimits {
    /// 上下文窗口（输入 + 输出总上限）。
    pub context: Option<usize>,
    /// 单条请求输入上限。
    pub input: Option<usize>,
    /// 最大输出 token 数。
    pub output: Option<usize>,
}

/// 磁盘缓存文件结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogCache {
    /// HTTP 缓存用的 ETag；下次刷新带 `If-None-Match`，304 时跳过写盘。
    pub etag: Option<String>,
    /// 上次成功刷新的 Unix 毫秒时间戳。
    pub last_fetched_at_ms: i64,
    /// 模型目录，按 `id` 索引。
    pub entries: HashMap<String, CatalogEntry>,
}

/// 读取当前目录。先尝试磁盘缓存，失败则用内置兜底。
///
/// 调用方拿到 `CatalogCache` 后应检查 [`is_stale`]：如果过期，fire-and-forget 调
/// [`refresh_catalog`] 联网更新（不阻塞当前返回）。
pub fn read_catalog(data_dir: &Path) -> CatalogCache {
    match try_load_disk_cache(data_dir) {
        Ok(Some(cache)) => cache,
        Ok(None) => {
            tracing::info!("models_catalog 无磁盘缓存，使用内置兜底");
            fallback_cache()
        }
        Err(e) => {
            tracing::warn!(error = %e, "models_catalog 读取磁盘缓存失败，使用内置兜底");
            fallback_cache()
        }
    }
}

/// 磁盘缓存是否在 TTL 内。
pub fn is_stale(cache: &CatalogCache) -> bool {
    let now = now_ms();
    let age = now.saturating_sub(cache.last_fetched_at_ms);
    age as u64 > TTL.as_millis() as u64
}

/// 联网刷新目录。带 ETag，304 时仅更新时间戳；200 时覆盖 entries + etag；
/// 任何网络 / 解析失败都只 warn 不抛错（调用方仍能用旧缓存）。
///
/// 返回 `true` 表示有更新（200 写了新 entries），`false` 表示未修改或失败。
pub async fn refresh_catalog(data_dir: &Path) -> bool {
    let current = try_load_disk_cache(data_dir).ok().flatten();
    let if_none_match = current.as_ref().and_then(|c| c.etag.clone());

    match fetch_remote(if_none_match.as_deref()).await {
        Ok(RemoteResult::NotModified) => {
            tracing::debug!("models_catalog 远端未修改（304），保留缓存");
            // 304：只推进时间戳，让下次 TTL 检查不再过期。
            if let Some(mut cache) = current {
                cache.last_fetched_at_ms = now_ms();
                if let Err(e) = save_cache(data_dir, &cache) {
                    tracing::warn!(error = %e, "models_catalog 304 后推进时间戳失败");
                }
            }
            false
        }
        Ok(RemoteResult::Updated { entries, etag }) => {
            let count = entries.len();
            let cache = CatalogCache {
                etag,
                last_fetched_at_ms: now_ms(),
                entries,
            };
            if let Err(e) = save_cache(data_dir, &cache) {
                tracing::warn!(error = %e, "models_catalog 写磁盘缓存失败");
                return false;
            }
            tracing::info!(count, "models_catalog 已更新");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "models_catalog 联网刷新失败，保留旧缓存");
            false
        }
    }
}

/// 缓存文件路径。
fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILENAME)
}

fn try_load_disk_cache(data_dir: &Path) -> std::io::Result<Option<CatalogCache>> {
    let path = cache_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    match serde_json::from_slice::<CatalogCache>(&bytes) {
        Ok(cache) => Ok(Some(cache)),
        Err(e) => {
            tracing::warn!(error = %e, "models_catalog 解析磁盘缓存失败，视为不存在");
            Ok(None)
        }
    }
}

fn fallback_cache() -> CatalogCache {
    let entries: HashMap<String, CatalogEntry> =
        serde_json::from_str(FALLBACK_JSON).unwrap_or_else(|e| {
            panic!("内置 models_catalog_fallback.json 解析失败：{e}")
        });
    let count = entries.len();
    tracing::debug!(count, "models_catalog 内置兜底加载完成");
    CatalogCache {
        etag: None,
        last_fetched_at_ms: now_ms(),
        entries,
    }
}

fn save_cache(data_dir: &Path, cache: &CatalogCache) -> AppResult<()> {
    let json = serde_json::to_vec_pretty(cache)?;
    crate::storage::lock::write_atomic(&cache_path(data_dir), &json)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

enum RemoteResult {
    NotModified,
    Updated {
        entries: HashMap<String, CatalogEntry>,
        etag: Option<String>,
    },
}

/// 拉远端 JSON。带 ETag + 超时；304 → `NotModified`，200 → 解析为 `HashMap<id, entry>`。
async fn fetch_remote(if_none_match: Option<&str>) -> AppResult<RemoteResult> {
    let mut builder = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("hebbian/1.0 (+models.dev cache)")
        .build()?
        .get("https://models.dev/models.json");
    if let Some(etag) = if_none_match {
        builder = builder.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = builder.send().await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(RemoteResult::NotModified);
    }
    if !status.is_success() {
        return Err(AppError::Msg(format!(
            "models.dev 返回非 2xx 状态：{status}"
        )));
    }
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = resp.bytes().await?;
    let entries: HashMap<String, CatalogEntry> = serde_json::from_slice(&body).map_err(|e| {
        AppError::Msg(format!("models.dev JSON 解析失败：{e}"))
    })?;
    Ok(RemoteResult::Updated { entries, etag })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_parses() {
        let cache = fallback_cache();
        assert!(!cache.entries.is_empty(), "内置兜底不能是空 map");
        // 抽几个常见模型验证 schema 完整性。
        assert!(cache.entries.contains_key("anthropic/claude-opus-4-5"));
        assert!(cache.entries.contains_key("openai/gpt-5"));
        assert!(cache.entries.contains_key("google/gemini-2.5-pro"));
    }

    #[test]
    fn stale_boundary() {
        let mut cache = fallback_cache();
        // 刚刚加载的兜底不 stale。
        assert!(!is_stale(&cache));
        // 时间戳推到 25h 前，应该 stale。
        cache.last_fetched_at_ms -= TTL.as_millis() as i64 + 3_600_000;
        assert!(is_stale(&cache));
    }

    #[test]
    fn round_trip_serde() {
        let cache = fallback_cache();
        let json = serde_json::to_vec(&cache).unwrap();
        let back: CatalogCache = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.entries.len(), cache.entries.len());
    }
}
