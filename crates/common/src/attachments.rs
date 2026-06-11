use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageAttachment {
    TextFile {
        name: String,
        media_type: String,
        content: String,
    },
    Image {
        name: String,
        media_type: String,
        data: String,
    },
}

/// 按扩展名 + magic bytes 识别字节是不是受支持的图片，返回其 media_type。
/// 两者都要命中：扩展名给候选，magic bytes 防止「.png 实为文本」误判；不命中返回 `None`。
///
/// 受支持格式与 §3 各 provider 的图片块编码一致（png/jpeg/webp/gif）。PDF / 音频
/// 等后续多模态格式在此函数同级新增 `detect_*`（架构 §4.4.1 扩展位）。
pub fn detect_image_media_type(file_name: &str, bytes: &[u8]) -> Option<&'static str> {
    let ext = file_name
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let by_ext: &'static str = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    };
    let by_magic = sniff_image_magic(bytes)?;
    (by_magic == by_ext).then_some(by_ext)
}

/// 仅凭 magic bytes 判断图片格式（不看扩展名）。
fn sniff_image_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

impl MessageAttachment {
    /// 把原始图片字节编码成 base64 的 `Image` 附件。
    pub fn image_from_bytes(
        name: impl Into<String>,
        media_type: impl Into<String>,
        bytes: &[u8],
    ) -> Self {
        MessageAttachment::Image {
            name: name.into(),
            media_type: media_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    pub fn as_text_block(&self) -> Option<String> {
        match self {
            MessageAttachment::TextFile {
                name,
                media_type,
                content,
            } => Some(format!(
                "<file name=\"{}\" media_type=\"{}\">\n{}\n</file>",
                escape_attr(name),
                escape_attr(media_type),
                content
            )),
            MessageAttachment::Image { .. } => None,
        }
    }

    pub fn image_data_url(&self) -> Option<String> {
        match self {
            MessageAttachment::Image {
                media_type, data, ..
            } => Some(format!("data:{media_type};base64,{data}")),
            MessageAttachment::TextFile { .. } => None,
        }
    }
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_SIG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn detects_png_by_ext_and_magic() {
        assert_eq!(detect_image_media_type("a.png", PNG_SIG), Some("image/png"));
        assert_eq!(
            detect_image_media_type("UPPER.PNG", PNG_SIG),
            Some("image/png")
        );
    }

    #[test]
    fn detects_jpeg_webp_gif() {
        assert_eq!(
            detect_image_media_type("p.jpg", &[0xFF, 0xD8, 0xFF, 0x00]),
            Some("image/jpeg")
        );
        let webp = [b'R', b'I', b'F', b'F', 0, 0, 0, 0, b'W', b'E', b'B', b'P'];
        assert_eq!(detect_image_media_type("p.webp", &webp), Some("image/webp"));
        assert_eq!(
            detect_image_media_type("p.gif", b"GIF89a\x00"),
            Some("image/gif")
        );
    }

    #[test]
    fn ext_magic_mismatch_rejected() {
        // .png 后缀但内容是文本 → 不当图片处理（防误判）。
        assert_eq!(detect_image_media_type("fake.png", b"hello world"), None);
    }

    #[test]
    fn non_image_returns_none() {
        assert_eq!(detect_image_media_type("a.txt", b"hello"), None);
        assert_eq!(detect_image_media_type("noext", PNG_SIG), None);
    }

    #[test]
    fn image_from_bytes_encodes_base64() {
        let att = MessageAttachment::image_from_bytes("p.png", "image/png", PNG_SIG);
        match att {
            MessageAttachment::Image {
                name,
                media_type,
                data,
            } => {
                assert_eq!(name, "p.png");
                assert_eq!(media_type, "image/png");
                // iVBORw0KGgo = PNG 签名的 base64 前缀
                assert!(data.starts_with("iVBORw0KGgo"), "实际: {data}");
            }
            other => panic!("应是 Image，实际 {other:?}"),
        }
    }
}
