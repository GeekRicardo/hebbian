use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("expected file header (¶path#HASH) at line {0}")]
    MissingFileHeader(usize),
    #[error("invalid hash at line {0}: must be 3 hex chars")]
    InvalidHash(usize),
    #[error("invalid keep range at line {0}: {1}")]
    InvalidKeepRange(usize, String),
    #[error("unexpected content at line {0}: {1}")]
    UnexpectedLine(usize, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub sections: Vec<FileSection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSection {
    pub path: String,
    pub expected_hash: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based 起始行（替换区间起点）
    pub start_line: usize,
    /// 1-based 结束行（替换区间终点，闭区间）
    pub end_line: usize,
    pub body: Vec<HunkLine>,
    /// EOF 锚点：追加到文件末尾，忽略 start/end
    pub is_eof_append: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Add(String),
    Keep { start: usize, end: usize },
}

pub fn parse_patch(text: &str) -> Result<Patch, ParseError> {
    let mut sections: Vec<FileSection> = Vec::new();
    let mut current_section: Option<FileSection> = None;
    let mut current_hunk: Option<Hunk> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;

        if let Some(rest) = raw_line.strip_prefix('¶') {
            flush_section(&mut current_section, &mut current_hunk, &mut sections);
            let (path, hash) = parse_file_header(rest, line_no)?;
            current_section = Some(FileSection {
                path,
                expected_hash: hash,
                hunks: Vec::new(),
            });
            continue;
        }

        let section = current_section
            .as_mut()
            .ok_or(ParseError::MissingFileHeader(line_no))?;

        if raw_line.is_empty() {
            continue;
        }

        if raw_line == "EOF" {
            flush_hunk(&mut current_hunk, section);
            current_hunk = Some(Hunk {
                start_line: 0,
                end_line: 0,
                body: Vec::new(),
                is_eof_append: true,
            });
            continue;
        }

        if let Some(rest) = raw_line.strip_prefix('+') {
            let hunk = current_hunk.get_or_insert_with(|| Hunk {
                start_line: 0,
                end_line: 0,
                body: Vec::new(),
                is_eof_append: false,
            });
            hunk.body
                .push(HunkLine::Add(strip_line_number_prefix(rest)));
            continue;
        }

        if let Some(rest) = raw_line.strip_prefix('&') {
            let (s, e) = parse_keep_range(rest, line_no)?;
            let hunk = current_hunk.get_or_insert_with(|| Hunk {
                start_line: 0,
                end_line: 0,
                body: Vec::new(),
                is_eof_append: false,
            });
            hunk.body.push(HunkLine::Keep { start: s, end: e });
            continue;
        }

        // hunk header: "5 8"
        if let Some((a, b)) = parse_hunk_header(raw_line) {
            flush_hunk(&mut current_hunk, section);
            current_hunk = Some(Hunk {
                start_line: a,
                end_line: b,
                body: Vec::new(),
                is_eof_append: false,
            });
            continue;
        }

        return Err(ParseError::UnexpectedLine(
            line_no,
            raw_line.chars().take(60).collect(),
        ));
    }

    flush_section(&mut current_section, &mut current_hunk, &mut sections);
    Ok(Patch { sections })
}

fn flush_hunk(current_hunk: &mut Option<Hunk>, section: &mut FileSection) {
    if let Some(h) = current_hunk.take() {
        section.hunks.push(h);
    }
}

fn flush_section(
    current_section: &mut Option<FileSection>,
    current_hunk: &mut Option<Hunk>,
    sections: &mut Vec<FileSection>,
) {
    if let Some(mut sec) = current_section.take() {
        if let Some(h) = current_hunk.take() {
            sec.hunks.push(h);
        }
        sections.push(sec);
    }
}

fn parse_file_header(rest: &str, line_no: usize) -> Result<(String, String), ParseError> {
    let (path, hash) = rest
        .rsplit_once('#')
        .ok_or(ParseError::MissingFileHeader(line_no))?;
    if hash.len() != 3 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ParseError::InvalidHash(line_no));
    }
    Ok((path.to_string(), hash.to_uppercase()))
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let (a, b) = line.split_once(' ')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn parse_keep_range(rest: &str, line_no: usize) -> Result<(usize, usize), ParseError> {
    let (a, b) = rest
        .split_once("..")
        .ok_or_else(|| ParseError::InvalidKeepRange(line_no, format!("&{rest}")))?;
    let s: usize = a
        .trim()
        .parse()
        .map_err(|_| ParseError::InvalidKeepRange(line_no, format!("&{rest}")))?;
    let e: usize = b
        .trim()
        .parse()
        .map_err(|_| ParseError::InvalidKeepRange(line_no, format!("&{rest}")))?;
    Ok((s, e))
}

/// 剥离模型回写的 cat -n 行号前缀（如 `123:content` → `content`）。
fn strip_line_number_prefix(s: &str) -> String {
    if let Some(idx) = s.find(':') {
        let head = &s[..idx];
        if !head.is_empty() && head.trim().chars().all(|c| c.is_ascii_digit()) {
            return s[idx + 1..].to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_replacement() {
        let input = "¶src/foo.rs#ABC\n5 7\n+hello\n+world\n";
        let p = parse_patch(input).unwrap();
        assert_eq!(p.sections.len(), 1);
        let s = &p.sections[0];
        assert_eq!(s.path, "src/foo.rs");
        assert_eq!(s.expected_hash, "ABC");
        assert_eq!(s.hunks.len(), 1);
        assert_eq!(s.hunks[0].start_line, 5);
        assert_eq!(s.hunks[0].end_line, 7);
        assert_eq!(
            s.hunks[0].body,
            vec![HunkLine::Add("hello".into()), HunkLine::Add("world".into())]
        );
    }

    #[test]
    fn parse_with_keep_range() {
        let input = "¶a.rs#FFF\n1 10\n+top\n&3..7\n+bottom\n";
        let p = parse_patch(input).unwrap();
        let body = &p.sections[0].hunks[0].body;
        assert_eq!(body.len(), 3);
        assert!(matches!(body[1], HunkLine::Keep { start: 3, end: 7 }));
    }

    #[test]
    fn parse_eof_anchor() {
        let input = "¶a.rs#001\nEOF\n+tail line\n";
        let p = parse_patch(input).unwrap();
        let h = &p.sections[0].hunks[0];
        assert!(h.is_eof_append);
        assert_eq!(h.body, vec![HunkLine::Add("tail line".into())]);
    }

    #[test]
    fn parse_strips_line_number_prefix_in_added_line() {
        let input = "¶a.rs#001\n5 5\n+5:hello\n";
        let p = parse_patch(input).unwrap();
        if let HunkLine::Add(s) = &p.sections[0].hunks[0].body[0] {
            assert_eq!(s, "hello", "行号前缀必须被剥离");
        } else {
            panic!("expected Add");
        }
    }

    #[test]
    fn parse_rejects_missing_header() {
        let err = parse_patch("5 8\n+x\n").unwrap_err();
        assert!(
            err.to_string().contains("file header"),
            "错误信息应提及 file header: {err}"
        );
    }

    #[test]
    fn parse_rejects_bad_hash_length() {
        let err = parse_patch("¶a.rs#XX\n5 8\n+x\n").unwrap_err();
        assert!(
            err.to_string().contains("hash"),
            "错误信息应提及 hash: {err}"
        );
    }

    #[test]
    fn parse_multi_section() {
        let input = "¶a.rs#001\n1 1\n+x\n¶b.rs#002\n2 2\n+y\n";
        let p = parse_patch(input).unwrap();
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[1].path, "b.rs");
        assert_eq!(p.sections[1].expected_hash, "002");
    }

    #[test]
    fn parse_hash_normalized_to_uppercase() {
        let input = "¶a.rs#abc\n1 1\n+x\n";
        let p = parse_patch(input).unwrap();
        assert_eq!(p.sections[0].expected_hash, "ABC");
    }

    #[test]
    fn parse_empty_lines_ignored() {
        let input = "¶a.rs#001\n\n1 1\n\n+x\n\n";
        let p = parse_patch(input).unwrap();
        assert_eq!(p.sections[0].hunks[0].body, vec![HunkLine::Add("x".into())]);
    }
}
