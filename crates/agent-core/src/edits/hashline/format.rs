use std::fmt::Write as _;

/// 内容的 3-hex 短指纹（CRC-32 低 12 bit，uppercase）。
///
/// 给模型做 stale 防御：模型回填 patch 时必须带上 Read 给的 hash，
/// 文件改过后 hash 变，用旧 hash 的 patch 会被拒绝。
/// 冲突概率 1/4096，对单次会话内的 stale-edit 防御足够。
/// 读追踪（ReadStateTracker）仍用完整 CRC-32 u32 做内部判定，互不依赖。
pub fn hash3(content: &str) -> String {
    let crc = crc32fast::hash(content.as_bytes());
    // 取低 12 bit → 3 hex nibbles
    let nibbles = crc & 0xFFF;
    let mut out = String::with_capacity(3);
    write!(
        out,
        "{:X}{:X}{:X}",
        (nibbles >> 8) & 0xF,
        (nibbles >> 4) & 0xF,
        nibbles & 0xF
    )
    .unwrap();
    out
}

/// 把文件内容渲染成 hashline 格式（无路径头）。
pub fn render_with_line_numbers(content: &str, hash: &str) -> String {
    render_with_line_numbers_with_path("", content, hash)
}

/// 把文件内容渲染成 hashline 格式，带路径头。
///
/// 输出格式：
/// ```text
/// ¶src/foo.rs#A1B
/// 1:line one
/// 2:line two
/// ```
pub fn render_with_line_numbers_with_path(path: &str, content: &str, hash: &str) -> String {
    let mut out = String::with_capacity(content.len() + 32);
    out.push('¶');
    out.push_str(path);
    out.push('#');
    out.push_str(hash);
    out.push('\n');
    if content.is_empty() {
        return out;
    }
    let mut line_no = 1usize;
    // split_inclusive 保留 \n，让我们能正确处理末尾无换行的情况
    for line in content.split_inclusive('\n') {
        let text = line.strip_suffix('\n').unwrap_or(line);
        let _ = write!(out, "{}:{}\n", line_no, text);
        line_no += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash3_is_three_uppercase_hex() {
        let h = hash3("hello\n");
        assert_eq!(h.len(), 3);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, h.to_uppercase());
    }

    #[test]
    fn hash3_empty_content() {
        let h = hash3("");
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn hash3_stable_across_calls() {
        assert_eq!(hash3("foo bar"), hash3("foo bar"));
    }

    #[test]
    fn render_with_line_numbers_format() {
        let out = render_with_line_numbers("alpha\nbeta\n", "ABC");
        assert_eq!(out, "¶#ABC\n1:alpha\n2:beta\n");
    }

    #[test]
    fn render_handles_no_trailing_newline() {
        let out = render_with_line_numbers("only", "ABC");
        assert_eq!(out, "¶#ABC\n1:only\n");
    }

    #[test]
    fn render_with_path() {
        let out = render_with_line_numbers_with_path("src/foo.rs", "x\n", "F00");
        assert_eq!(out, "¶src/foo.rs#F00\n1:x\n");
    }
}
