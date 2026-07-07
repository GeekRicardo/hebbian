//! Tool output context shaping (Step 9 L0/L1).
//!
//! 工具实现返回 raw text；进入 transcript 前统一在这里做清洗、脱敏、超长行折叠，
//! 大输出再生成 head+tail 预览。artifact 保存的是 sanitized full text，而不是 raw secret。

use regex::Regex;
use std::sync::OnceLock;

const LONG_LINE_MAX_CHARS: usize = 800;
const LONG_LINE_HEAD_CHARS: usize = 240;
const LONG_LINE_TAIL_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedToolOutput {
    pub text: String,
    pub redactions_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPolicy {
    pub max_chars: usize,
    pub error_tail_bias: bool,
}

impl PreviewPolicy {
    pub fn new(max_chars: usize, is_error: bool, text: &str) -> Self {
        Self {
            max_chars,
            error_tail_bias: is_error || contains_error_signal(text),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadTailPreview {
    pub text: String,
    pub omitted_chars: usize,
    pub shown_head_chars: usize,
    pub shown_tail_chars: usize,
}

pub fn sanitize_tool_output(raw: &str) -> SanitizedToolOutput {
    let folded = fold_carriage_returns(raw);
    let stripped = strip_ansi_and_controls(&folded);
    let (redacted, redactions_count) = redact_secrets(&stripped);
    let text = elide_long_lines(&redacted);
    SanitizedToolOutput {
        text,
        redactions_count,
    }
}

pub fn head_tail_preview(text: &str, policy: PreviewPolicy) -> HeadTailPreview {
    let total_chars = text.chars().count();
    if total_chars <= policy.max_chars {
        return HeadTailPreview {
            text: text.to_string(),
            omitted_chars: 0,
            shown_head_chars: total_chars,
            shown_tail_chars: 0,
        };
    }

    let max_chars = policy.max_chars.max(32);
    let (head_chars, tail_chars) = if policy.error_tail_bias {
        (max_chars * 35 / 100, max_chars - (max_chars * 35 / 100))
    } else {
        (max_chars * 60 / 100, max_chars - (max_chars * 60 / 100))
    };

    let head = take_chars(text, head_chars);
    let tail = take_last_chars(text, tail_chars);
    let omitted_chars = total_chars.saturating_sub(head_chars + tail_chars);
    let preview = format!(
        "--- BEGIN HEAD ---\n{head}\n--- END HEAD ---\n\n... {omitted_chars} chars omitted ...\n\n--- BEGIN TAIL ---\n{tail}\n--- END TAIL ---"
    );

    HeadTailPreview {
        text: preview,
        omitted_chars,
        shown_head_chars: head_chars,
        shown_tail_chars: tail_chars,
    }
}

pub fn artifact_marker(
    tool_name: &str,
    call_id: &str,
    original_bytes: u64,
    sanitized_bytes: u64,
    line_count: u32,
    artifact_path: &str,
    preview: &HeadTailPreview,
    preview_text: &str,
) -> String {
    format!(
        "[工具输出过长，已保存完整内容]\nTool: {tool_name}\nCall ID: {call_id}\nOriginal: {original_bytes} bytes\nSanitized: {sanitized_bytes} bytes / {line_count} lines\nShown: first {head} chars + last {tail} chars\nFull output: {artifact_path}\n\n{preview_text}\n\nNeed details? Use Grep on the artifact first, then Read with offset/limit. Do not read the full file unless necessary.",
        head = preview.shown_head_chars,
        tail = preview.shown_tail_chars,
    )
}

fn fold_carriage_returns(input: &str) -> String {
    let mut out = String::new();
    for segment in input.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let body = if has_newline {
            &segment[..segment.len() - 1]
        } else {
            segment
        };
        let last_frame = body.rsplit('\r').next().unwrap_or(body);
        out.push_str(last_frame);
        if has_newline {
            out.push('\n');
        }
    }
    out
}

fn strip_ansi_and_controls(input: &str) -> String {
    let without_ansi = ansi_regex().replace_all(input, "");
    let mut out = String::new();
    for ch in without_ansi.chars() {
        match ch {
            '\u{08}' => {
                out.pop();
            }
            '\n' | '\t' => out.push(ch),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn redact_secrets(input: &str) -> (String, usize) {
    let mut text = input.to_string();
    let mut count = 0usize;
    for (regex, label) in secret_regexes() {
        let matches = regex.find_iter(&text).count();
        if matches > 0 {
            count += matches;
            text = regex
                .replace_all(&text, format!("[REDACTED:{label}]").as_str())
                .into_owned();
        }
    }
    (text, count)
}

fn elide_long_lines(input: &str) -> String {
    let mut out = String::new();
    for (idx, line) in input.split_inclusive('\n').enumerate() {
        let has_newline = line.ends_with('\n');
        let body = if has_newline {
            &line[..line.len() - 1]
        } else {
            line
        };
        if idx > 0 && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&elide_line(body));
        if has_newline {
            out.push('\n');
        }
    }
    out
}

fn elide_line(line: &str) -> String {
    let chars = line.chars().count();
    if chars <= LONG_LINE_MAX_CHARS {
        return line.to_string();
    }
    let head = take_chars(line, LONG_LINE_HEAD_CHARS);
    let tail = take_last_chars(line, LONG_LINE_TAIL_CHARS);
    let omitted = chars.saturating_sub(LONG_LINE_HEAD_CHARS + LONG_LINE_TAIL_CHARS);
    format!("{head}<elided {omitted} chars>{tail}")
}

fn contains_error_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "error",
        "failed",
        "panic",
        "traceback",
        "exception",
        "exit code",
        "cannot find",
        "mismatched",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn take_last_chars(s: &str, n: usize) -> String {
    let mut chars: Vec<char> = s.chars().rev().take(n).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn ansi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\)|P[^\x1B]*(?:\x1B\\))")
            .expect("valid ansi regex")
    })
}

fn secret_regexes() -> &'static [(Regex, &'static str)] {
    static RES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RES.get_or_init(|| {
        vec![
            (Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._~+/=-]{12,}").unwrap(), "bearer"),
            (Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}").unwrap(), "jwt"),
            (Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(), "private_key"),
            (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "aws_access_key"),
            (Regex::new(r"gh[pousr]_[A-Za-z0-9_]{20,}").unwrap(), "github_token"),
            (Regex::new(r"sk-(?:proj-)?[A-Za-z0-9_-]{20,}").unwrap(), "api_key"),
            (Regex::new(r"xox[baprs]-[A-Za-z0-9-]{20,}").unwrap(), "slack_token"),
            (Regex::new(r#"(?i)(token|api[_-]?key|password|secret)\s*[:=]\s*[^\s'"]{8,}"#).unwrap(), "secret_assignment"),
        ]
    })
    .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_carriage_return_progress_to_last_frame() {
        let raw = String::from_utf8(vec![
            b'p', b'u', b'l', b'l', b' ', b'1', b'%', b'\r', b'p', b'u', b'l', b'l', b' ', b'2', b'%', b'\r',
            b'p', b'u', b'l', b'l', b' ', b'd', b'o', b'n', b'e', b'\n', b'n', b'e', b'x', b't',
        ])
        .unwrap();
        let got = sanitize_tool_output(&raw);
        assert_eq!(got.text, "pull done\nnext");
    }

    #[test]
    fn strips_ansi_and_backspace_controls() {
        let got = sanitize_tool_output("\u{1b}[31mred\u{1b}[0m ax\u{08}b\u{7} ok");
        assert_eq!(got.text, "red ab ok");
    }

    #[test]
    fn redacts_secret_before_line_elision() {
        let raw = format!("prefix token={} suffix", "a".repeat(900));
        let got = sanitize_tool_output(&raw);
        assert_eq!(got.redactions_count, 1);
        assert!(got.text.contains("[REDACTED:secret_assignment]"));
        assert!(!got.text.contains(&"a".repeat(100)));
    }

    #[test]
    fn elides_long_lines_with_head_and_tail() {
        let raw = format!("{}TAIL", "x".repeat(900));
        let got = sanitize_tool_output(&raw);
        assert!(got.text.contains("<elided "));
        assert!(got.text.ends_with("TAIL"));
    }

    #[test]
    fn head_tail_preview_biases_tail_for_errors() {
        let text = format!("{}\nerror: boom\n{}", "a".repeat(1000), "z".repeat(1000));
        let preview = head_tail_preview(&text, PreviewPolicy::new(100, true, &text));
        assert!(preview.shown_tail_chars > preview.shown_head_chars);
        assert!(preview.text.contains("BEGIN HEAD"));
        assert!(preview.text.contains("BEGIN TAIL"));
    }
}
