use thiserror::Error;

use super::format::hash3;
use super::parser::{FileSection, Hunk, HunkLine, ParseError};

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(
        "stale hash for {path}: patch says {expected}, current is {actual} — 请重新 Read 后再 Edit"
    )]
    StaleHash {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("hunk out of range: {0}")]
    OutOfRange(String),
}

/// 把一个 FileSection 的 patch 应用到 `original` 内容，返回新内容。
///
/// 纯函数：不读写文件，便于单元测试。
pub fn apply_section(section: &FileSection, original: &str) -> Result<String, ApplyError> {
    let actual_hash = hash3(original);
    if actual_hash != section.expected_hash {
        return Err(ApplyError::StaleHash {
            path: section.path.clone(),
            expected: section.expected_hash.clone(),
            actual: actual_hash,
        });
    }

    let original_lines = split_lines(original);

    // 按起点降序应用，避免行号漂移；EOF hunk 排最后（视为 usize::MAX）
    let mut hunks_sorted: Vec<&Hunk> = section.hunks.iter().collect();
    hunks_sorted.sort_by_key(|h| {
        std::cmp::Reverse(if h.is_eof_append {
            usize::MAX
        } else {
            h.start_line
        })
    });

    let mut lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();

    for h in hunks_sorted {
        apply_hunk(&mut lines, h, &original_lines)?;
    }

    Ok(join_lines(&lines, original.ends_with('\n')))
}

fn apply_hunk(
    lines: &mut Vec<String>,
    h: &Hunk,
    original_lines: &[&str],
) -> Result<(), ApplyError> {
    let expanded = expand_body(&h.body, original_lines)?;

    if h.is_eof_append {
        lines.extend(expanded);
        return Ok(());
    }

    if h.start_line == 0 || h.end_line < h.start_line || h.end_line > lines.len() {
        return Err(ApplyError::OutOfRange(format!(
            "lines {}..={} (file has {} lines)",
            h.start_line,
            h.end_line,
            lines.len()
        )));
    }

    let start = h.start_line - 1; // 转 0-based
    let end = h.end_line; // exclusive
    lines.splice(start..end, expanded);
    Ok(())
}

fn expand_body(body: &[HunkLine], original_lines: &[&str]) -> Result<Vec<String>, ApplyError> {
    let mut out = Vec::with_capacity(body.len());
    for hl in body {
        match hl {
            HunkLine::Add(s) => out.push(s.clone()),
            HunkLine::Keep { start, end } => {
                if *start == 0 || end < start || *end > original_lines.len() {
                    return Err(ApplyError::OutOfRange(format!(
                        "&{}..{} (file has {} lines)",
                        start,
                        end,
                        original_lines.len()
                    )));
                }
                for i in (start - 1)..*end {
                    out.push(original_lines[i].to_string());
                }
            }
        }
    }
    Ok(out)
}

fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = content.split('\n').collect();
    // 末尾 \n 会产生一个空字符串，去掉，让行号 1-based 对齐
    if content.ends_with('\n') {
        v.pop();
    }
    v
}

fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    if lines.is_empty() {
        return if trailing_newline {
            "\n".to_string()
        } else {
            String::new()
        };
    }
    let mut out = lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::parser::parse_patch;
    use super::*;

    fn apply_str(original: &str, patch_text: &str) -> Result<String, ApplyError> {
        let patch = parse_patch(patch_text).map_err(ApplyError::Parse)?;
        apply_section(&patch.sections[0], original)
    }

    #[test]
    fn replace_middle_lines() {
        let original = "a\nb\nc\nd\ne\n";
        let h = hash3(original);
        let patch = format!("¶f#{h}\n2 3\n+B\n+C\n");
        let out = apply_str(original, &patch).unwrap();
        assert_eq!(out, "a\nB\nC\nd\ne\n");
    }

    #[test]
    fn keep_range_preserves_lines() {
        let original = "L1\nL2\nL3\nL4\nL5\n";
        let h = hash3(original);
        // 替换 1..5：新首行 + 保留 2..4 + 新末行
        let patch = format!("¶f#{h}\n1 5\n+TOP\n&2..4\n+BOTTOM\n");
        let out = apply_str(original, &patch).unwrap();
        assert_eq!(out, "TOP\nL2\nL3\nL4\nBOTTOM\n");
    }

    #[test]
    fn eof_appends() {
        let original = "head\n";
        let h = hash3(original);
        let patch = format!("¶f#{h}\nEOF\n+tail\n");
        let out = apply_str(original, &patch).unwrap();
        assert_eq!(out, "head\ntail\n");
    }

    #[test]
    fn rejects_stale_hash() {
        let original = "x\n";
        let patch = "¶f#000\n1 1\n+y\n";
        let err = apply_str(original, patch).unwrap_err();
        assert!(matches!(err, ApplyError::StaleHash { .. }));
    }

    #[test]
    fn rejects_out_of_range_hunk() {
        let original = "only one line\n";
        let h = hash3(original);
        let patch = format!("¶f#{h}\n5 7\n+x\n");
        let err = apply_str(original, &patch).unwrap_err();
        assert!(matches!(err, ApplyError::OutOfRange(_)));
    }

    #[test]
    fn keep_range_out_of_bounds() {
        let original = "a\nb\n";
        let h = hash3(original);
        let patch = format!("¶f#{h}\n1 2\n&5..6\n");
        let err = apply_str(original, &patch).unwrap_err();
        assert!(matches!(err, ApplyError::OutOfRange(_)));
    }

    #[test]
    fn multiple_hunks_no_line_drift() {
        // 改 1..2 → X，再改 5..6 → Y；从后往前 apply 所以行号不漂移
        let original = "1\n2\n3\n4\n5\n6\n";
        let h = hash3(original);
        let patch = format!("¶f#{h}\n1 2\n+X\n5 6\n+Y\n");
        let out = apply_str(original, &patch).unwrap();
        assert_eq!(out, "X\n3\n4\nY\n");
    }

    #[test]
    fn preserves_trailing_newline() {
        let original = "line\n";
        let h = hash3(original);
        let patch = format!("¶f#{h}\n1 1\n+new\n");
        let out = apply_str(original, &patch).unwrap();
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn preserves_no_trailing_newline() {
        let original = "line";
        let h = hash3(original);
        let patch = format!("¶f#{h}\n1 1\n+new\n");
        let out = apply_str(original, &patch).unwrap();
        assert!(!out.ends_with('\n'));
    }
}
