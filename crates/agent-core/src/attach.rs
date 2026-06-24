//! 输入框「粘贴/拖拽路径」的探测与分流。纯 fs 逻辑，无 surface 依赖：
//! desktop 原生拖拽、hebweb 浏览器拖拽共用同一份分类。
//!
//! 设计要点：**粘贴/拖拽路径 = 引用而非上传**——文件/目录都只回路径，交由前端加进
//! allowed_paths，由 agent 按需 Read，不把内容塞进上下文。仅原生拖拽的小图片/小文本会
//! 直接读成附件（DropOutcome::Image / TextFile），其余一律退回引用，绝不丢文件。

use std::path::Path;

const MAX_DROP_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_DROP_IMAGE_BYTES: u64 = 12 * 1024 * 1024;

/// 前端粘贴/拖拽路径时的探测结果。文件/目录都让前端加进 allowed_paths，
/// 由 agent 按需 Read，不把内容塞进上下文。（真正的「上传」走附件复制路径。）
#[derive(serde::Serialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachPathResult {
    File { path: String },
    Dir { path: String },
    Missing { path: String },
}

/// 探测单条粘贴路径形态。file:// URI 会被 percent-decode。
pub fn attach_path(path: &str) -> AttachPathResult {
    let raw = path.trim();
    if raw.is_empty() {
        return AttachPathResult::Missing {
            path: path.to_string(),
        };
    }
    let cleaned = normalize_dropped_path(raw);
    let p = Path::new(&cleaned);
    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => {
            return AttachPathResult::Missing {
                path: path.to_string(),
            }
        }
    };
    let resolved = p.to_string_lossy().into_owned();
    if meta.is_dir() {
        AttachPathResult::Dir { path: resolved }
    } else {
        AttachPathResult::File { path: resolved }
    }
}

/// 原生拖拽落到输入框时，每个磁盘路径的分流结果。
/// 支持的小图片 / 文本 → 读成附件进上下文；其余（目录、大文件、二进制、未知类型）
/// → 只回路径让前端加进 allowed_paths，由 agent 按需 Read，与 `attach_path` 同语义。
/// tag/字段刻意对齐前端 `MessageAttachment`（image / text_file），分流后可直接入附件列表。
#[derive(serde::Serialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DropOutcome {
    Image {
        name: String,
        media_type: String,
        data: String,
    },
    TextFile {
        name: String,
        media_type: String,
        content: String,
    },
    Reference {
        path: String,
    },
    Missing {
        path: String,
    },
}

/// 批量分流原生拖拽路径。
pub fn drop_paths(paths: Vec<String>) -> Vec<DropOutcome> {
    paths.into_iter().map(classify_dropped_path).collect()
}

fn classify_dropped_path(raw: String) -> DropOutcome {
    use base64::Engine;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DropOutcome::Missing { path: raw };
    }
    let cleaned = normalize_dropped_path(trimmed);
    let p = Path::new(&cleaned);
    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => return DropOutcome::Missing { path: raw },
    };
    let resolved = p.to_string_lossy().into_owned();
    // 目录永远只引用：内容无从「读成附件」。
    if meta.is_dir() {
        return DropOutcome::Reference { path: resolved };
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| resolved.clone());

    if let Some(media_type) = image_media_type(p) {
        if meta.len() <= MAX_DROP_IMAGE_BYTES {
            if let Ok(bytes) = std::fs::read(p) {
                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                return DropOutcome::Image {
                    name,
                    media_type: media_type.to_string(),
                    data,
                };
            }
        }
        // 超限或读失败 → 退回引用，不丢这个文件。
        return DropOutcome::Reference { path: resolved };
    }

    if let Some(media_type) = text_media_type(p) {
        if meta.len() <= MAX_DROP_TEXT_BYTES {
            if let Ok(content) = std::fs::read_to_string(p) {
                return DropOutcome::TextFile {
                    name,
                    media_type: media_type.to_string(),
                    content,
                };
            }
        }
        return DropOutcome::Reference { path: resolved };
    }

    // 未知类型（二进制 / 无扩展名等）→ 引用兜底。
    DropOutcome::Reference { path: resolved }
}

/// 接受 file:// URI（macOS Finder / GTK 拖拽常见格式）并 percent-decode。
/// 非 file:// 路径原样返回。粘贴路径与原生拖拽都经此归一化。
fn normalize_dropped_path(raw: &str) -> String {
    raw.strip_prefix("file://")
        .map(percent_decode)
        .unwrap_or_else(|| raw.to_string())
}

/// 按扩展名判图片附件类型；非图片返回 None。
fn image_media_type(p: &Path) -> Option<&'static str> {
    match path_ext_lower(p).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("webp") => Some("image/webp"),
        Some("gif") => Some("image/gif"),
        _ => None,
    }
}

/// 按扩展名判文本附件类型；非已知文本返回 None。扩展名集合与前端 `isTextFile` 对齐。
fn text_media_type(p: &Path) -> Option<&'static str> {
    match path_ext_lower(p).as_deref() {
        Some("md") | Some("markdown") => Some("text/markdown"),
        Some("json") | Some("jsonl") => Some("application/json"),
        Some("xml") => Some("application/xml"),
        Some("html") => Some("text/html"),
        Some("css") => Some("text/css"),
        Some("csv") => Some("text/csv"),
        Some(
            "txt" | "ts" | "tsx" | "js" | "jsx" | "rs" | "py" | "go" | "java" | "c" | "cpp" | "h"
            | "hpp" | "yaml" | "yml" | "toml" | "sql",
        ) => Some("text/plain"),
        _ => None,
    }
}

fn path_ext_lower(p: &Path) -> Option<String> {
    p.extension().map(|e| e.to_string_lossy().to_lowercase())
}

fn percent_decode(s: &str) -> String {
    // 简易 percent decode：只处理常见 %XX 的两位 hex；其余保持原样。
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_path_references_file_without_reading_content() {
        let dir = std::env::temp_dir().join(format!("heb-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.md");
        std::fs::write(&file, "敏感内容不该进上下文").unwrap();

        // 文件路径 = 引用：回 File { path }，绝不读内容（结果里没有 content / data 字段）。
        let res = attach_path(&file.to_string_lossy());
        match &res {
            AttachPathResult::File { path } => assert!(path.ends_with("note.md")),
            other => panic!("文件路径应回 File，实际 {other:?}"),
        }
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["kind"], "file");
        assert!(json.get("content").is_none() && json.get("data").is_none());
        assert!(!json.to_string().contains("敏感内容"));

        // file:// URI 同样按文件引用。
        let uri = format!("file://{}", file.to_string_lossy());
        assert!(matches!(attach_path(&uri), AttachPathResult::File { .. }));

        // 目录回 Dir，缺失回 Missing。
        assert!(matches!(
            attach_path(&dir.to_string_lossy()),
            AttachPathResult::Dir { .. }
        ));
        assert!(matches!(
            attach_path(&dir.join("nope.txt").to_string_lossy()),
            AttachPathResult::Missing { .. }
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drop_paths_classifies_by_type() {
        let dir = std::env::temp_dir().join(format!("heb-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("note.md");
        std::fs::write(&text, "# 标题").unwrap();
        let png = dir.join("shot.png");
        // 最小合法 PNG 头即可，分流只看扩展名 + 大小。
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        let bin = dir.join("blob.unknownext");
        std::fs::write(&bin, [0u8, 1, 2, 3]).unwrap();

        let s = |p: &Path| p.to_string_lossy().into_owned();
        let out = drop_paths(vec![
            s(&text),
            s(&png),
            s(&bin),
            s(&dir),              // 目录 → 引用
            s(&dir.join("nope")), // 缺失 → missing
        ]);

        assert!(matches!(&out[0], DropOutcome::TextFile { content, .. } if content == "# 标题"));
        assert!(
            matches!(&out[1], DropOutcome::Image { media_type, .. } if media_type == "image/png")
        );
        assert!(matches!(&out[2], DropOutcome::Reference { .. }));
        assert!(matches!(&out[3], DropOutcome::Reference { .. }));
        assert!(matches!(&out[4], DropOutcome::Missing { .. }));

        std::fs::remove_dir_all(&dir).ok();
    }
}
