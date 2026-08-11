//! 行级 diff。
//!
//! 自己写而不是引第三方 crate：需求只有「两段文本按行比，标出增删」这一件事，
//! LCS 三十行就够，省一个依赖。
//!
//! **规模保护**：LCS 是 O(n·m)，大文件会直接把界面卡死。超过阈值时退化成
//! 「整段删 + 整段增」——信息量下降但不会卡住，且用户一眼能看出是大改动。

/// 一行 diff 的归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Equal,
    Insert,
    Delete,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
    /// 旧文件里的行号（1-based）。新增行没有。
    pub old_no: Option<usize>,
    /// 新文件里的行号（1-based）。删除行没有。
    pub new_no: Option<usize>,
}

/// 超过这个行数就不做 LCS 了。两侧行数相乘约 4 亿次比较是明显卡顿的量级，
/// 2000×2000 = 400 万在现代机器上是毫秒级，作为上限稳妥。
const MAX_LCS_LINES: usize = 2000;

pub fn line_diff(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    if old.len() > MAX_LCS_LINES || new.len() > MAX_LCS_LINES {
        return whole_replace(&old, &new);
    }

    // 经典 LCS 表。lcs[i][j] = old[i..] 与 new[j..] 的最长公共子序列长度。
    let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            out.push(DiffLine {
                kind: DiffKind::Equal,
                text: old[i].to_string(),
                old_no: Some(i + 1),
                new_no: Some(j + 1),
            });
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(DiffLine {
                kind: DiffKind::Delete,
                text: old[i].to_string(),
                old_no: Some(i + 1),
                new_no: None,
            });
            i += 1;
        } else {
            out.push(DiffLine {
                kind: DiffKind::Insert,
                text: new[j].to_string(),
                old_no: None,
                new_no: Some(j + 1),
            });
            j += 1;
        }
    }
    while i < old.len() {
        out.push(DiffLine {
            kind: DiffKind::Delete,
            text: old[i].to_string(),
            old_no: Some(i + 1),
            new_no: None,
        });
        i += 1;
    }
    while j < new.len() {
        out.push(DiffLine {
            kind: DiffKind::Insert,
            text: new[j].to_string(),
            old_no: None,
            new_no: Some(j + 1),
        });
        j += 1;
    }
    out
}

fn whole_replace(old: &[&str], new: &[&str]) -> Vec<DiffLine> {
    let mut out = Vec::with_capacity(old.len() + new.len());
    for (i, line) in old.iter().enumerate() {
        out.push(DiffLine {
            kind: DiffKind::Delete,
            text: (*line).to_string(),
            old_no: Some(i + 1),
            new_no: None,
        });
    }
    for (j, line) in new.iter().enumerate() {
        out.push(DiffLine {
            kind: DiffKind::Insert,
            text: (*line).to_string(),
            old_no: None,
            new_no: Some(j + 1),
        });
    }
    out
}

/// 增删行数统计，给「+12 −3」这种小标签用。
pub fn stats(lines: &[DiffLine]) -> (usize, usize) {
    let added = lines.iter().filter(|l| l.kind == DiffKind::Insert).count();
    let removed = lines.iter().filter(|l| l.kind == DiffKind::Delete).count();
    (added, removed)
}

/// 只保留改动附近的行，中间大段没变的折叠掉。`context` = 上下各留几行。
/// 返回 `None` 表示这里是被折叠掉的一段（渲染成「… 省略 N 行」）。
pub fn collapse(lines: &[DiffLine], context: usize) -> Vec<Option<DiffLine>> {
    let keep: Vec<bool> = lines
        .iter()
        .enumerate()
        .map(|(i, _)| {
            lines
                .iter()
                .enumerate()
                .any(|(j, l)| l.kind != DiffKind::Equal && i.abs_diff(j) <= context)
        })
        .collect();

    let mut out = Vec::new();
    let mut skipping = false;
    for (i, line) in lines.iter().enumerate() {
        if keep[i] {
            out.push(Some(line.clone()));
            skipping = false;
        } else if !skipping {
            out.push(None);
            skipping = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_insert_is_detected() {
        let d = line_diff("a\nb", "a\nx\nb");
        assert_eq!(stats(&d), (1, 0));
        assert_eq!(d[1].kind, DiffKind::Insert);
        assert_eq!(d[1].text, "x");
    }

    #[test]
    fn pure_delete_is_detected() {
        let d = line_diff("a\nx\nb", "a\nb");
        assert_eq!(stats(&d), (0, 1));
    }

    #[test]
    fn replacement_shows_both_sides() {
        let d = line_diff("a\nold\nb", "a\nnew\nb");
        assert_eq!(stats(&d), (1, 1));
    }

    #[test]
    fn identical_text_has_no_changes() {
        let d = line_diff("a\nb\nc", "a\nb\nc");
        assert_eq!(stats(&d), (0, 0));
        assert!(d.iter().all(|l| l.kind == DiffKind::Equal));
    }

    /// 超过 LCS 上限时不能卡死，也不能返回空——退化成整段替换。
    #[test]
    fn huge_files_fall_back_to_whole_replace() {
        let big: String = (0..MAX_LCS_LINES + 10)
            .map(|i| format!("line {i}\n"))
            .collect();
        let d = line_diff(&big, "one line");
        let (added, removed) = stats(&d);
        assert_eq!(added, 1);
        assert_eq!(removed, MAX_LCS_LINES + 10);
    }

    #[test]
    fn collapse_hides_untouched_middle() {
        let before: String = (0..40).map(|i| format!("l{i}\n")).collect();
        let mut after_lines: Vec<String> = (0..40).map(|i| format!("l{i}")).collect();
        after_lines[20] = "changed".to_string();
        let after = after_lines.join("\n");

        let d = line_diff(&before, &after);
        let folded = collapse(&d, 3);
        // 折叠后一定比原来短，且至少有一处「省略」占位。
        assert!(folded.len() < d.len());
        assert!(folded.iter().any(|l| l.is_none()));
    }
}
