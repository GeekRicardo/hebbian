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

impl MessageAttachment {
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
