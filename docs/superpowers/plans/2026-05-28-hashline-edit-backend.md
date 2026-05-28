# Hashline Edit Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现一份 oh-my-pi Hashline 风格的文件编辑链路（`Read` 输出带 hash 头与行号、`Edit` 接受 hashline patch 文本），通过 settings 单一开关在 `string-replace`（现状）和 `hashline` 之间切换，方便 A/B 试效果再决定去留。

**Architecture:**
- 新建 `crates/agent-core/src/edits/hashline/` 子模块，纯函数式实现 parse → apply（不持有状态、不直接 fs::write）
- 新增 `ReadHashlineTool` / `EditHashlineTool` 与现有 `ReadTool` / `EditTool` 平行存在；dispatch 注册时按 `settings.edit_backend` 二选一
- snapshot hash 由 `ReadStateTracker` 现有的 SHA-256 取前 3 位 hex（uppercase），完全复用现有读追踪机制，不引入新状态
- Tool description 内嵌 `prompt.md` 简化版（~80 行）+ JSON schema 只接受 `patch: string`，让模型直接生成 hashline 文本

**Tech Stack:** Rust（agent-core crate）, serde_json schema, sha2 (已在依赖里), 单元测试用 `cargo test -p agent-core`。前端 settings 用现有 React + Tauri command 结构。

---

## File Structure

新增 / 修改文件清单（先看清整体边界）：

**新增**：
- `crates/agent-core/src/edits/hashline/mod.rs` — 模块入口，re-export
- `crates/agent-core/src/edits/hashline/format.rs` — `hash3(content) -> String`（3-hex SHA-256 前缀）+ `render_with_line_numbers(content, hash) -> String`
- `crates/agent-core/src/edits/hashline/parser.rs` — `parse_patch(text) -> Result<Patch>`，把模型输出解析成内部 `Patch { sections: Vec<FileSection> }`
- `crates/agent-core/src/edits/hashline/apply.rs` — `apply_patch(patch, fs_reader) -> Result<Vec<FileChange>>`，纯函数：拿 patch + 当前文件内容 → 算出新内容；不写盘
- `crates/agent-core/src/edits/hashline/prompt.md` — 教模型写 hashline 的 80 行说明，`include_str!` 进 tool description
- `crates/agent-core/src/tools/edit_hashline.rs` — `EditHashlineTool`，实现 `Tool` trait，调 `apply_patch` 后落盘
- `crates/agent-core/src/tools/read_hashline.rs` — `ReadHashlineTool`，输出 `¶path#HASH\n1:line\n2:line\n...`
- `crates/agent-core/tests/hashline_roundtrip.rs` — 集成测试：写文件 → Read → Edit → 校验

**修改**：
- `crates/agent-core/src/edits/mod.rs` — 加 `pub mod hashline;`
- `crates/agent-core/src/tools/mod.rs` — 加 `pub mod edit_hashline; pub mod read_hashline;`
- `crates/agent-core/src/storage/settings.rs` — 加 `edit_backend: EditBackend` 字段（enum: `StringReplace` / `Hashline`，默认 `StringReplace`）
- `crates/agent-core/src/dispatch.rs`（或工具注册位置） — 按 `edit_backend` 二选一注册 `Edit` 与 `Read`
- `apps/desktop/frontend/src/desktop/ui/components/SettingsDialog.tsx`（或对应文件）— 加单选项让用户切换
- `crates/agent-core/src/read_state.rs` — 暴露 `compute_hash3(&str) -> String` 复用工具
- `docs/架构.md` §4.4 — 新增"Edit 后端可插拔"段落，记录 `edit_backend` 决策
- `docs/changelog.md` — 追加一条

---
## Task 1: settings.json 加 edit_backend 字段

**Files:**
- Modify: `crates/agent-core/src/storage/settings.rs`
- Test: `crates/agent-core/src/storage/settings.rs`（同文件 #[cfg(test)] mod）

- [ ] **Step 1: 写一个失败的测试**

在 `settings.rs` 末尾的 `#[cfg(test)] mod tests` 加：

```rust
#[test]
fn edit_backend_defaults_to_string_replace() {
    let settings = Settings::default();
    assert_eq!(settings.edit_backend, EditBackend::StringReplace);
}

#[test]
fn edit_backend_round_trip_json() {
    let json = r#"{"edit_backend":"hashline"}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.edit_backend, EditBackend::Hashline);
    let out = serde_json::to_string(&s).unwrap();
    assert!(out.contains(r#""edit_backend":"hashline""#));
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p agent-core --lib settings::tests::edit_backend
```
Expected: FAIL（`EditBackend` 未定义）

- [ ] **Step 3: 加最小实现**

在 `settings.rs` 加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EditBackend {
    #[default]
    StringReplace,
    Hashline,
}
```

在 `Settings` struct 里加字段：

```rust
#[serde(default)]
pub edit_backend: EditBackend,
```

注意：`#[serde(default)]` 让旧 settings.json（没这个字段）自动用默认值，向前兼容。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p agent-core --lib settings::tests::edit_backend
```
Expected: PASS

- [ ] **Step 5: 跑全 workspace check**

```bash
cargo check --workspace
```
Expected: 编译通过

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/storage/settings.rs
git commit -m "$(cat <<'EOF'
settings: 增加 edit_backend 字段（string-replace/hashline）

- Why: 为 Hashline edit 后端做 A/B 切换准备
- 影响范围: agent-core storage；旧 settings.json 通过 #[serde(default)] 兼容
- 留尾巴: 后续 task 接入 dispatch 注册逻辑
EOF
)"
```

---
## Task 2: hash3 与 read_state 复用

**Files:**
- Modify: `crates/agent-core/src/read_state.rs`
- Create: `crates/agent-core/src/edits/hashline/mod.rs`
- Create: `crates/agent-core/src/edits/hashline/format.rs`
- Modify: `crates/agent-core/src/edits/mod.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/agent-core/src/edits/hashline/format.rs`（先 touch 空文件再加 mod 引用）写：

```rust
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
        let expected = "¶#ABC\n1:alpha\n2:beta\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn render_handles_trailing_newline_absence() {
        let out = render_with_line_numbers("only", "ABC");
        assert_eq!(out, "¶#ABC\n1:only\n");
    }

    #[test]
    fn render_with_path() {
        let out = render_with_line_numbers_with_path("src/foo.rs", "x\n", "F00");
        assert_eq!(out, "¶src/foo.rs#F00\n1:x\n");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

先建模块入口 `crates/agent-core/src/edits/hashline/mod.rs`：

```rust
pub mod format;
```

在 `crates/agent-core/src/edits/mod.rs` 加：

```rust
pub mod hashline;
```

```bash
cargo test -p agent-core --lib edits::hashline::format::tests
```
Expected: FAIL（`hash3` / `render_with_line_numbers` 未定义）

- [ ] **Step 3: 实现 format.rs**

写入 `crates/agent-core/src/edits/hashline/format.rs`：

```rust
//! Hashline 文本格式工具：3-hex 内容指纹与带行号的渲染。
//!
//! hash3 取 SHA-256 前 12 bit (3 hex chars)。冲突概率 1/4096，
//! 对一次会话内的 stale-edit 防御足够；不是密码学场景。

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub fn hash3(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    // 取头两字节里的 3 个 hex nibble
    let b0 = digest[0];
    let b1 = digest[1];
    let mut out = String::with_capacity(3);
    write!(out, "{:X}{:X}{:X}", b0 >> 4, b0 & 0x0F, b1 >> 4).unwrap();
    out
}

pub fn render_with_line_numbers(content: &str, hash: &str) -> String {
    render_with_line_numbers_with_path("", content, hash)
}

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
    for line in content.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        let _ = write!(out, "{}:{}\n", line_no, trimmed);
        line_no += 1;
    }
    out
}
```

确认 `Cargo.toml` 已有 `sha2`（agent-core 现状已经依赖），无需新增。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p agent-core --lib edits::hashline::format::tests
```
Expected: PASS（5 个测试全过）

- [ ] **Step 5: 在 read_state.rs 暴露公共助手（如有需要）**

读 `read_state.rs` 看现有 hash 计算函数。如果已有 `compute_hash` 是 SHA-256 全量，**不要改它**——`hash3` 是独立用途（给模型看），读追踪用全 hash（防误判）。两个并存，注释里写清。

如果 `read_state.rs` 内部 hash 已经是公共可调用，跳过此步。否则在 `read_state.rs` 加：

```rust
/// 给 hashline 工具看的 3-hex 短指纹；读追踪自己仍用完整 SHA-256。
/// 短指纹只用于"模型说我看到的还是这一版"防幻觉，不参与 stale 判定。
pub fn short_content_fingerprint(content: &str) -> String {
    crate::edits::hashline::format::hash3(content)
}
```

（如果 read_state 不需要这个 helper 就跳过，让 hashline 工具直接调 `format::hash3`。）

- [ ] **Step 6: 跑 workspace check**

```bash
cargo check --workspace
```
Expected: 编译通过

- [ ] **Step 7: Commit**

```bash
git add crates/agent-core/src/edits/hashline/ crates/agent-core/src/edits/mod.rs
# 如改了 read_state.rs 一并 add
git commit -m "$(cat <<'EOF'
edits/hashline: 加 hash3 与 render_with_line_numbers 基础工具

- Why: Hashline 后端的格式渲染基础；hash3=SHA-256 前 12 bit，给模型做 stale 防御
- 影响范围: 新增 edits::hashline 模块，纯函数，不影响现有路径
- 留尾巴: parser / apply / 工具壳后续 task 实现
EOF
)"
```

---
## Task 3: hashline parser

**Files:**
- Create: `crates/agent-core/src/edits/hashline/parser.rs`
- Modify: `crates/agent-core/src/edits/hashline/mod.rs`

参考 oh-my-pi `packages/hashline/src/parser.ts`（结构定义）和 `prefixes.ts`（行号前缀剥离）。**只实现核心子集**，不做 oh-my-pi 完整的流式恢复（那是 streaming 应用场景，Hebbian 的 tool call 是一次性 payload）。

支持的语法：
```
¶path/to/file.rs#A1B           ← 文件头：路径 + 3-hex hash（必需）
5 8                            ← hunk header：替换原 5..=8 行
+new line one                  ← 新内容
+new line two
&10..15                        ← 保留原 10..=15 行
EOF                            ← 锚点：追加到文件尾
+appended line
```

不支持（先扔掉省事）：
- 流式 partial parse（一次性 payload 模式）
- 创建新文件（先用 string-replace 兜底；hashline 后端首版只做 modify）
- 删除文件
- `*A,B`（move 语法，复杂度低收益）

- [ ] **Step 1: 写失败测试**

`crates/agent-core/src/edits/hashline/parser.rs`：

```rust
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
    }

    #[test]
    fn parse_strips_line_number_prefix_in_added_line() {
        // 模型经常把 "5:hello" 这种 cat -n 风格抄进 + 行
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
        assert!(err.to_string().contains("file header"));
    }

    #[test]
    fn parse_rejects_bad_hash_length() {
        let err = parse_patch("¶a.rs#XX\n5 8\n+x\n").unwrap_err();
        assert!(err.to_string().contains("hash"));
    }

    #[test]
    fn parse_multi_section() {
        let input = "¶a.rs#001\n1 1\n+x\n¶b.rs#002\n2 2\n+y\n";
        let p = parse_patch(input).unwrap();
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[1].path, "b.rs");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

加 `pub mod parser;` 到 `crates/agent-core/src/edits/hashline/mod.rs`。

```bash
cargo test -p agent-core --lib edits::hashline::parser::tests
```
Expected: FAIL（类型未定义）

- [ ] **Step 3: 实现 parser.rs**

```rust
//! Hashline patch 文本 → 内部 AST。一次性解析，不支持流式恢复。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("expected file header (¶path#HASH) at line {0}")]
    MissingFileHeader(usize),
    #[error("invalid hash at line {0}: must be 3 hex chars")]
    InvalidHash(usize),
    #[error("invalid hunk header at line {0}: {1}")]
    InvalidHunkHeader(usize, String),
    #[error("unexpected line at {0}: {1}")]
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
        let line = raw_line;

        if let Some(rest) = line.strip_prefix('¶') {
            // flush 前一段
            if let Some(mut sec) = current_section.take() {
                if let Some(h) = current_hunk.take() {
                    sec.hunks.push(h);
                }
                sections.push(sec);
            }
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

        if line.is_empty() {
            continue;
        }

        if line == "EOF" {
            if let Some(h) = current_hunk.take() {
                section.hunks.push(h);
            }
            current_hunk = Some(Hunk {
                start_line: 0,
                end_line: 0,
                body: Vec::new(),
                is_eof_append: true,
            });
            continue;
        }

        if let Some(rest) = line.strip_prefix('+') {
            let h = current_hunk
                .get_or_insert_with(|| Hunk {
                    start_line: 0,
                    end_line: 0,
                    body: Vec::new(),
                    is_eof_append: false,
                });
            h.body.push(HunkLine::Add(strip_line_number_prefix(rest)));
            continue;
        }

        if let Some(rest) = line.strip_prefix('&') {
            let (s, e) = parse_keep_range(rest, line_no)?;
            let h = current_hunk
                .get_or_insert_with(|| Hunk {
                    start_line: 0,
                    end_line: 0,
                    body: Vec::new(),
                    is_eof_append: false,
                });
            h.body.push(HunkLine::Keep { start: s, end: e });
            continue;
        }

        // hunk header: "5 8"
        if let Some((a, b)) = parse_hunk_header(line) {
            if let Some(h) = current_hunk.take() {
                section.hunks.push(h);
            }
            current_hunk = Some(Hunk {
                start_line: a,
                end_line: b,
                body: Vec::new(),
                is_eof_append: false,
            });
            continue;
        }

        return Err(ParseError::UnexpectedLine(line_no, line.to_string()));
    }

    if let Some(mut sec) = current_section.take() {
        if let Some(h) = current_hunk.take() {
            sec.hunks.push(h);
        }
        sections.push(sec);
    }

    Ok(Patch { sections })
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
    let (a, b) = rest.split_once("..").ok_or_else(|| {
        ParseError::InvalidHunkHeader(line_no, format!("&{}", rest))
    })?;
    let s: usize = a.trim().parse().map_err(|_| {
        ParseError::InvalidHunkHeader(line_no, format!("&{}", rest))
    })?;
    let e: usize = b.trim().parse().map_err(|_| {
        ParseError::InvalidHunkHeader(line_no, format!("&{}", rest))
    })?;
    Ok((s, e))
}

fn strip_line_number_prefix(s: &str) -> String {
    // 模型回写 cat -n 行号 "123:..." 或 "  123\t..." 时剥掉
    if let Some(idx) = s.find(':') {
        let head = &s[..idx];
        if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()) {
            return s[idx + 1..].to_string();
        }
    }
    s.to_string()
}
```

确认 `Cargo.toml` 已有 `thiserror`（看一眼再决定要不要加）。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p agent-core --lib edits::hashline::parser::tests
```
Expected: PASS（7 个）

- [ ] **Step 5: workspace check**

```bash
cargo check --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/edits/hashline/
git commit -m "$(cat <<'EOF'
edits/hashline: parser 实现（¶header / +add / &keep / EOF）

- Why: Hashline 后端的文本→AST 解析；只做一次性 payload，不做流式恢复
- 影响范围: 新增模块；行号前缀自动剥离（防模型回抄 cat -n）
- 留尾巴: apply 与 tool 壳层下一 task
EOF
)"
```

---
## Task 4: hashline apply（纯函数：patch + 当前内容 → 新内容）

**Files:**
- Create: `crates/agent-core/src/edits/hashline/apply.rs`
- Modify: `crates/agent-core/src/edits/hashline/mod.rs`

apply 是核心：拿到 parser 输出 + 当前文件内容，算出 new 内容。**不写文件**——上层 tool 才落盘。

- [ ] **Step 1: 写失败测试**

`crates/agent-core/src/edits/hashline/apply.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::super::format::hash3;
    use super::super::parser::parse_patch;
    use super::*;

    fn apply_one(original: &str, patch_text: &str) -> Result<String, ApplyError> {
        let patch = parse_patch(patch_text).map_err(ApplyError::Parse)?;
        let section = &patch.sections[0];
        apply_section(section, original)
    }

    #[test]
    fn replace_middle_lines() {
        let original = "a\nb\nc\nd\ne\n";
        let h = hash3(original);
        let patch = format!("¶f#{}\n2 3\n+B\n+C\n", h);
        let out = apply_one(original, &patch).unwrap();
        assert_eq!(out, "a\nB\nC\nd\ne\n");
    }

    #[test]
    fn keep_range_preserves_lines() {
        let original = "L1\nL2\nL3\nL4\nL5\n";
        let h = hash3(original);
        // 替换 1..5：新首行 + 保留 2..4 + 新末行
        let patch = format!("¶f#{}\n1 5\n+TOP\n&2..4\n+BOTTOM\n", h);
        let out = apply_one(original, &patch).unwrap();
        assert_eq!(out, "TOP\nL2\nL3\nL4\nBOTTOM\n");
    }

    #[test]
    fn eof_appends() {
        let original = "head\n";
        let h = hash3(original);
        let patch = format!("¶f#{}\nEOF\n+tail\n", h);
        let out = apply_one(original, &patch).unwrap();
        assert_eq!(out, "head\ntail\n");
    }

    #[test]
    fn rejects_stale_hash() {
        let original = "x\n";
        let patch = "¶f#000\n1 1\n+y\n";
        let err = apply_one(original, patch).unwrap_err();
        assert!(matches!(err, ApplyError::StaleHash { .. }));
    }

    #[test]
    fn rejects_out_of_range_hunk() {
        let original = "only one line\n";
        let h = hash3(original);
        let patch = format!("¶f#{}\n5 7\n+x\n", h);
        let err = apply_one(original, &patch).unwrap_err();
        assert!(matches!(err, ApplyError::OutOfRange { .. }));
    }

    #[test]
    fn keep_range_out_of_bounds() {
        let original = "a\nb\n";
        let h = hash3(original);
        let patch = format!("¶f#{}\n1 2\n&5..6\n", h);
        let err = apply_one(original, &patch).unwrap_err();
        assert!(matches!(err, ApplyError::OutOfRange { .. }));
    }

    #[test]
    fn multiple_hunks_descending_safety() {
        // 一段里多个 hunk 时，apply 必须按"从后往前"应用，避免行号漂移
        let original = "1\n2\n3\n4\n5\n6\n";
        let h = hash3(original);
        // 改 1..2 → X，再改 5..6 → Y
        let patch = format!("¶f#{}\n1 2\n+X\n5 6\n+Y\n", h);
        let out = apply_one(original, &patch).unwrap();
        assert_eq!(out, "X\n3\n4\nY\n");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p agent-core --lib edits::hashline::apply::tests
```
Expected: FAIL

- [ ] **Step 3: 实现 apply.rs**

```rust
//! Hashline AST → 新文件内容（纯函数）。

use super::format::hash3;
use super::parser::{FileSection, Hunk, HunkLine, ParseError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("stale hash for {path}: patch says {expected}, current is {actual} — 请重新 Read 后再 Edit")]
    StaleHash {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("hunk out of range: {0}")]
    OutOfRange(String),
}

pub fn apply_section(section: &FileSection, original: &str) -> Result<String, ApplyError> {
    let actual_hash = hash3(original);
    if actual_hash != section.expected_hash {
        return Err(ApplyError::StaleHash {
            path: section.path.clone(),
            expected: section.expected_hash.clone(),
            actual: actual_hash,
        });
    }

    let original_lines: Vec<&str> = split_keep_no_newline(original);
    let mut hunks_sorted: Vec<&Hunk> = section.hunks.iter().collect();
    // 按起点降序，从后往前应用，避免行号漂移
    hunks_sorted.sort_by(|a, b| {
        let key = |h: &Hunk| if h.is_eof_append { usize::MAX } else { h.start_line };
        key(b).cmp(&key(a))
    });

    let mut lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();

    for h in hunks_sorted {
        apply_hunk_in_place(h, &mut lines, &original_lines)?;
    }

    Ok(join_with_newline(&lines, has_trailing_newline(original)))
}

fn apply_hunk_in_place(
    h: &Hunk,
    lines: &mut Vec<String>,
    original_lines: &[&str],
) -> Result<(), ApplyError> {
    let expanded = expand_body(&h.body, original_lines)?;
    if h.is_eof_append {
        lines.extend(expanded);
        return Ok(());
    }
    // 1-based 转 0-based 闭区间
    if h.start_line == 0 || h.end_line < h.start_line || h.end_line > lines.len() {
        return Err(ApplyError::OutOfRange(format!(
            "{}..{} (file has {} lines)",
            h.start_line,
            h.end_line,
            lines.len()
        )));
    }
    let start = h.start_line - 1;
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

fn split_keep_no_newline(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = content.split('\n').collect();
    // 末尾 \n 会产生一个空字符串，去掉
    if content.ends_with('\n') {
        v.pop();
    }
    v
}

fn has_trailing_newline(s: &str) -> bool {
    s.ends_with('\n')
}

fn join_with_newline(lines: &[String], trailing: bool) -> String {
    let mut out = lines.join("\n");
    if trailing && !out.is_empty() {
        out.push('\n');
    } else if trailing && out.is_empty() {
        out.push('\n');
    }
    out
}
```

⚠️ 注意 `join_with_newline` 的边界：原文件有尾 newline 时，apply 后必须保持；没有则不补。`split_keep_no_newline` 与 `join_with_newline` 是配对的。

加 `pub mod apply;` 到 `mod.rs`。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p agent-core --lib edits::hashline::apply::tests
```
Expected: PASS（7 个）

- [ ] **Step 5: workspace check**

```bash
cargo check --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/edits/hashline/
git commit -m "$(cat <<'EOF'
edits/hashline: apply 实现（纯函数 patch → 新内容）

- Why: 完成 hashline 核心算法层，与 IO 解耦便于单测
- 影响范围: 新增 apply.rs；stale hash / 越界 / 多 hunk 行号漂移全有用例
- 留尾巴: tool 壳层 + dispatch 接入下一 task
EOF
)"
```

---
## Task 5: prompt.md 教学文本

**Files:**
- Create: `crates/agent-core/src/edits/hashline/prompt.md`

把 oh-my-pi 的 `prompt.md` 精简到 Hebbian 当前支持的语法子集（删 streaming、删 *A,B move、删 create/delete file 段落）。让模型一看就会。

- [ ] **Step 1: 写文件**

`crates/agent-core/src/edits/hashline/prompt.md`：

```markdown
# Hashline Edit Format

Hebbian 的 `Edit` 工具接受 hashline 格式：用行号锚点替代 old_string / new_string。
比起字符串替换，hashline 在改大文件局部时显著省 token（`&A..B` 可以保留原文区段不复制）。

## 输入示例

`Read` 工具输出形如：

    ¶src/lib.rs#A1B
    1:fn main() {
    2:    println!("hi");
    3:}

- 头行 `¶<path>#<HASH>`：3 位 hex 是当前内容指纹，必须原样回填到 patch 头
- 正文 `N:<line>`：N 是 1-based 行号

## patch 语法

```
¶src/lib.rs#A1B            ← 必填，照抄 Read 给的 hash
2 2                        ← 替换原 2..=2 行（1-based 闭区间）
+    println!("hello");
```

多 hunk：

```
¶a.rs#001
1 1
+new top
5 7
+replaced
```

保留原文区段（省 token，强烈推荐）：

```
¶a.rs#001
1 20
+new line one
&3..15
+new last line
```
↑ 替换 1..=20 行，新内容里中间夹原文 3..=15 行。

追加到文件末尾：

```
¶a.rs#001
EOF
+appended line A
+appended line B
```

## 重要约束

1. **hash 必须用 Read 最近一次给的那个**——文件改过后 hash 会变，stale hash 会被拒绝；先重新 Read 再 Edit
2. **行号一律以 Read 最新输出为准**，不要凭印象算
3. **`+` 行的内容不带行号前缀**——直接写代码本身。如果不小心写成 `+5:foo`，工具会自动剥掉 `5:` 但别依赖
4. 一个 patch 可以含多文件（多个 `¶header`），但同一文件的多 hunk 行号互不重叠
5. 不支持创建新文件、删除文件、移动行——这些场景用别的工具

## 错误处理

- `stale hash`：文件改过了，重新 Read 再 Edit
- `out of range`：hunk 的行号超出文件总行数，对照 Read 输出修正
```

- [ ] **Step 2: workspace check**

```bash
cargo check --workspace
```
（include_str! 没人引用，这步主要确认文件路径合法。）

- [ ] **Step 3: Commit**

```bash
git add crates/agent-core/src/edits/hashline/prompt.md
git commit -m "$(cat <<'EOF'
edits/hashline: 加 prompt.md 教学文本

- Why: 后续 tool description 通过 include_str! 内嵌；模型一看就会写 hashline
- 影响范围: 纯文档；尚未被代码引用
EOF
)"
```

---
## Task 6: ReadHashlineTool 实现

**Files:**
- Create: `crates/agent-core/src/tools/read_hashline.rs`
- Modify: `crates/agent-core/src/tools/mod.rs`

参考现有 `crates/agent-core/src/tools/read.rs`。**核心区别**：输出格式从 `cat -n` 改为 hashline 头 + `N:line`，**其余逻辑（mtime 追踪、读追踪登记、二进制检测、image 处理、行截断）全部保留并复用**。

策略：**先复制再抽象**。直接把 `read.rs` 全文复制改名为 `read_hashline.rs`，只替换最后一步格式化函数。50 行重复 < 引入新抽象的耦合成本。

- [ ] **Step 1: 写失败测试**

`crates/agent-core/src/tools/read_hashline.rs` 末尾加 `#[cfg(test)] mod tests`，照搬 `read.rs` 现有测试构造方式，再加两条 hashline 特有断言：

```rust
#[tokio::test]
async fn outputs_hashline_header_and_numbered_lines() {
    let (tool, dir) = make_test_tool();
    let p = dir.path().join("foo.txt");
    std::fs::write(&p, "alpha\nbeta\n").unwrap();
    let out = call_read(&tool, &p).await.unwrap();
    assert!(out.starts_with("¶"), "must start with ¶ header: {}", out);
    assert!(out.contains("\n1:alpha\n"));
    assert!(out.contains("\n2:beta\n"));
}

#[tokio::test]
async fn hash_matches_format_hash3() {
    let (tool, dir) = make_test_tool();
    let p = dir.path().join("foo.txt");
    let content = "line\n";
    std::fs::write(&p, content).unwrap();
    let out = call_read(&tool, &p).await.unwrap();
    let expected = crate::edits::hashline::format::hash3(content);
    assert!(out.contains(&format!("#{}\n", expected)),
        "header must include #{}: {}", expected, out);
}
```

`make_test_tool` / `call_read` 按 `read.rs` 现有测试脚手架抄一份——不要发明新的命名。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p agent-core --lib tools::read_hashline
```
Expected: FAIL（文件不存在）

- [ ] **Step 3: 实现 read_hashline.rs**

复制 `read.rs` 全文为骨架，做四处改动：

1. struct 改名 `ReadTool` → `ReadHashlineTool`
2. `name()` 仍返回 `"Read"`（**不改名**——同一工具的两个后端实现，对模型不可见）
3. `description()` 改成简短说明：

```rust
fn description(&self) -> &str {
    "读取文件。返回 hashline 格式：第一行 `¶<path>#<HASH>`，正文 `N:line` 带 1-based 行号。HASH 是当前内容的 3 位 hex 指纹，Edit 工具会校验。"
}
```

4. 格式化输出那一段，把 cat -n 拼接换成：

```rust
let display_path = relative_path_for_display(&abs_path, &workspace);
let hash = crate::edits::hashline::format::hash3(&content);
let formatted = crate::edits::hashline::format::render_with_line_numbers_with_path(
    &display_path,
    &content,
    &hash,
);
```

- 路径用与原 cat -n 路径一致的相对路径（保证模型回填 patch 时 `¶<path>#HASH` 能被 `Workspace::resolve` 正确还原）
- 行截断（>2000 行）的提示文本可以直接拼在 hashline 输出末尾，加一行 `... truncated to first 2000 lines, use offset/limit ...`，不影响 hashline 头/行号格式
- 二进制 / image 分支直接返回，不走 hashline 格式化（这两种本来就不该被 Edit）

注册到 `crates/agent-core/src/tools/mod.rs`：

```rust
pub mod read_hashline;
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p agent-core --lib tools::read_hashline
cargo test -p agent-core --lib tools::read
```
Expected: 两边都 PASS（确认原 ReadTool 测试不受影响）

- [ ] **Step 5: workspace check**

```bash
cargo check --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/tools/read_hashline.rs crates/agent-core/src/tools/mod.rs
git commit -m "$(cat <<'EOF'
tools: 新增 ReadHashlineTool（hashline 格式输出）

- Why: Hashline 后端要求 Read 输出带 ¶path#hash 头 + N:line 行号
- 影响范围: 与 ReadTool 平行；name() 同为 "Read"，dispatch 二选一注册
- 留尾巴: dispatch 接入与 EditHashlineTool 下一 task
EOF
)"
```

---

## Task 7: EditHashlineTool 实现

**Files:**
- Create: `crates/agent-core/src/tools/edit_hashline.rs`
- Modify: `crates/agent-core/src/tools/mod.rs`

工具壳层做这些事：
1. JSON schema 只接受 `patch: string`（一个入参）
2. `parser::parse_patch` 解析
3. 对每个 `FileSection`：检查 read_state → 读文件 → `apply::apply_section` → 落盘 → 更新 tracker
4. 错误用现有 `EditTool` 的错误风格返回，让模型能自纠

- [ ] **Step 1: 写失败测试**

`crates/agent-core/src/tools/edit_hashline.rs` 末尾加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::edits::hashline::format::hash3;

    #[tokio::test]
    async fn applies_simple_replacement() {
        let (tool, dir) = make_test_tool();
        let p = dir.path().join("foo.txt");
        let original = "alpha\nbeta\ngamma\n";
        std::fs::write(&p, original).unwrap();
        mark_read_for_test(&tool, &p, original);

        let patch = format!(
            "¶{}#{}\n2 2\n+BETA\n",
            display_path(&p),
            hash3(original)
        );
        let res = tool.execute(serde_json::json!({ "patch": patch })).await.unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "alpha\nBETA\ngamma\n");
        assert!(res.contains("applied"));
    }

    #[tokio::test]
    async fn rejects_unread_file() {
        let (tool, dir) = make_test_tool();
        let p = dir.path().join("foo.txt");
        std::fs::write(&p, "x\n").unwrap();
        let patch = format!("¶{}#{}\n1 1\n+y\n", display_path(&p), hash3("x\n"));
        let err = tool.execute(serde_json::json!({ "patch": patch })).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("read"),
            "未先 Read 必须报错: {}", err);
    }

    #[tokio::test]
    async fn rejects_stale_hash() {
        let (tool, dir) = make_test_tool();
        let p = dir.path().join("foo.txt");
        std::fs::write(&p, "current\n").unwrap();
        mark_read_for_test(&tool, &p, "current\n");

        let patch = format!("¶{}#000\n1 1\n+y\n", display_path(&p));
        let err = tool.execute(serde_json::json!({ "patch": patch })).await.unwrap_err();
        let s = err.to_string().to_lowercase();
        assert!(s.contains("stale") || s.contains("hash"));
    }

    #[tokio::test]
    async fn multi_file_patch() {
        let (tool, dir) = make_test_tool();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "A1\n").unwrap();
        std::fs::write(&b, "B1\n").unwrap();
        mark_read_for_test(&tool, &a, "A1\n");
        mark_read_for_test(&tool, &b, "B1\n");

        let patch = format!(
            "¶{}#{}\n1 1\n+A2\n¶{}#{}\n1 1\n+B2\n",
            display_path(&a), hash3("A1\n"),
            display_path(&b), hash3("B1\n"),
        );
        tool.execute(serde_json::json!({ "patch": patch })).await.unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "A2\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "B2\n");
    }
}
```

`make_test_tool` / `mark_read_for_test` / `display_path` 按现有 `edit.rs` 的测试脚手架抄。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p agent-core --lib tools::edit_hashline
```
Expected: FAIL

- [ ] **Step 3: 实现 edit_hashline.rs**

```rust
//! Hashline 后端的 Edit 工具：JSON 入参只有 patch: string。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::AppResult;
use crate::edits::hashline::{
    apply::{apply_section, ApplyError},
    format::hash3,
    parser::parse_patch,
};
use crate::read_state::ReadStateTracker;
use crate::tools::Tool;
use crate::workspace::Workspace;

pub struct EditHashlineTool {
    workspace: Arc<Workspace>,
    tracker: Option<Arc<ReadStateTracker>>,
}

impl EditHashlineTool {
    pub fn new(workspace: Arc<Workspace>, tracker: Option<Arc<ReadStateTracker>>) -> Self {
        Self { workspace, tracker }
    }
}

#[async_trait]
impl Tool for EditHashlineTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        include_str!("../edits/hashline/prompt.md")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Hashline patch 文本。完整语法见工具说明。"
                }
            },
            "required": ["patch"]
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let patch_text = input
            .get("patch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| /* 用现有 EditTool 同款 bad_input 构造 */ todo!())?;

        let patch = parse_patch(patch_text).map_err(|e| /* bad_input */ todo!())?;

        let mut report = Vec::with_capacity(patch.sections.len());

        for section in &patch.sections {
            let abs_path = self.workspace.resolve(&section.path)?;

            // 1) 读追踪：未读 / stale 直接拒绝（与 EditTool 同款逻辑）
            if let Some(tracker) = &self.tracker {
                tracker.ensure_can_edit(&abs_path)?; // 名字按现有 tracker API 来
            }

            // 2) 读当前内容
            let original = std::fs::read_to_string(&abs_path)?;

            // 3) 应用 patch（纯函数；hash 校验在里面）
            let new_content = apply_section(section, &original).map_err(|e| match e {
                ApplyError::Parse(p) => /* bad_input */ todo!(),
                ApplyError::StaleHash { .. } | ApplyError::OutOfRange(_) => /* bad_input */ todo!(),
            })?;

            // 4) 落盘
            std::fs::write(&abs_path, &new_content)?;

            // 5) 更新读追踪：mtime + 内容指纹
            if let Some(tracker) = &self.tracker {
                tracker.record_write(&abs_path, &new_content);
            }

            report.push(format!(
                "applied {} ({} hunk{}) → new hash {}",
                section.path,
                section.hunks.len(),
                if section.hunks.len() == 1 { "" } else { "s" },
                hash3(&new_content),
            ));
        }

        Ok(report.join("\n"))
    }
}
```

⚠️ 代码里 `todo!()` 占位的几处错误构造，要照搬 `edit.rs` 现有的错误风格——`AppError::bad_input(msg)` / `anyhow!` / `crate::error::Error::*` 取决于 agent-core 当前的错误模型。**实施时先读一遍 `edit.rs` 的 execute() 错误分支再写**。

⚠️ `tracker.ensure_can_edit` / `tracker.record_write` 是占位名，按 `read_state.rs` 现有 public API 来。如果现有 `EditTool` 是直接调 `tracker.check_path_was_read` + `tracker.set_path_state`，照抄同样调用。

注册：

```rust
// crates/agent-core/src/tools/mod.rs
pub mod edit_hashline;
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p agent-core --lib tools::edit_hashline
cargo test -p agent-core --lib tools::edit
```
Expected: 全 PASS

- [ ] **Step 5: workspace check**

```bash
cargo check --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/tools/edit_hashline.rs crates/agent-core/src/tools/mod.rs
git commit -m "$(cat <<'EOF'
tools: 新增 EditHashlineTool（patch: string 一个入参）

- Why: Hashline 后端的 Edit 工具壳层；内嵌 prompt.md 当 description
- 影响范围: 与 EditTool 平行；name() 同为 "Edit"，dispatch 二选一
- 留尾巴: dispatch 注册按 settings.edit_backend 二选一下一 task
EOF
)"
```

---

## Task 8: dispatch 按 settings.edit_backend 二选一注册

**Files:**
- Modify: `crates/agent-core/src/dispatch.rs`（或工具注册的实际位置——`harness.rs` 也可能）

实施前先确认：用 `Mcp__codegraph__codegraph_search` 找 `EditTool::new` 的唯一 caller，那就是注册点。

- [ ] **Step 1: 定位注册点**

```bash
# 用 codegraph 找：
# Mcp__codegraph__codegraph_callers symbol=EditTool 或 grep "EditTool::new"
```

记下文件路径与具体函数。

- [ ] **Step 2: 写失败测试**

如果注册点有 unit test 覆盖（比如 `register_default_tools` 之类），加一条：

```rust
#[test]
fn edit_backend_hashline_swaps_in_hashline_tools() {
    let mut settings = Settings::default();
    settings.edit_backend = EditBackend::Hashline;
    let registry = build_tool_registry(&settings, /* deps */);
    let edit = registry.get_tool("Edit").unwrap();
    // 通过 schema 形态区分：hashline 的 schema 只有 patch 一个字段
    let schema = edit.parameters_schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("patch"));
    assert!(!props.contains_key("old_string"));
}

#[test]
fn edit_backend_default_uses_string_replace() {
    let settings = Settings::default();
    let registry = build_tool_registry(&settings, /* deps */);
    let schema = registry.get_tool("Edit").unwrap().parameters_schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("old_string"));
    assert!(props.contains_key("new_string"));
}
```

如果注册点没有 unit test 覆盖，跳过此步，靠集成测试（Task 10）验证。

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test -p agent-core --lib dispatch  # 或注册点对应 mod
```

- [ ] **Step 4: 改注册逻辑**

在注册函数里：

```rust
use crate::storage::settings::EditBackend;

match settings.edit_backend {
    EditBackend::StringReplace => {
        registry.register(Box::new(ReadTool::new(workspace.clone(), tracker.clone())));
        registry.register(Box::new(EditTool::new(workspace.clone(), tracker.clone())));
    }
    EditBackend::Hashline => {
        registry.register(Box::new(ReadHashlineTool::new(workspace.clone(), tracker.clone())));
        registry.register(Box::new(EditHashlineTool::new(workspace.clone(), tracker.clone())));
    }
}
```

注意：
- `Read` 和 `Edit` **必须配套切换**——两个后端的 Read 输出格式与 Edit 输入格式强耦合，不能 hashline Edit 配 cat -n Read
- settings 是从哪里拿到的？看注册函数现有签名。若签名里没有，加 `&Settings` 参数。若注册时机早于 settings 加载，那 settings 必须沿 `Workspace` / `Session` 链路传到注册点（这是 task 顺序里的隐性依赖，发现时及时调整）

- [ ] **Step 5: 跑测试**

```bash
cargo test -p agent-core --lib
```
Expected: 全 PASS

- [ ] **Step 6: workspace check**

```bash
cargo check --workspace
```

- [ ] **Step 7: Commit**

```bash
git add crates/agent-core/src/dispatch.rs  # 或实际改动文件
git commit -m "$(cat <<'EOF'
dispatch: 按 settings.edit_backend 二选一注册 Read/Edit

- Why: 让 Hashline 后端真正接入；Read+Edit 强耦合必须配套切换
- 影响范围: agent-core dispatch；默认 StringReplace 不影响现状
- 留尾巴: 前端 settings UI / 集成测试 / 架构文档下一批
EOF
)"
```

---

## Task 9: 前端 Settings UI 加切换项

**Files:**
- Modify: `apps/desktop/frontend/src/desktop/ui/components/SettingsDialog.tsx`（或对应路径，先 grep `edit_backend` / `Settings` 组件定位）
- Modify: `apps/desktop/src/chat.rs` 或对应 Tauri command 桥（如有 settings 透传需要）
- Modify: `apps/desktop/frontend/src/desktop/types.ts`（settings 类型定义）

- [ ] **Step 1: 定位前端 settings 编辑入口**

```bash
# 用 grep 找前端 settings dialog 的实际文件
rg -l "edit_backend\|editBackend\|Settings\b" apps/desktop/frontend/src
```

- [ ] **Step 2: types.ts 加字段**

```typescript
export interface Settings {
  // ... 已有字段
  edit_backend: "string-replace" | "hashline";
}
```

- [ ] **Step 3: SettingsDialog 加 UI**

加一个 RadioGroup 或 Select：

```tsx
<section>
  <h3>Edit 工具后端</h3>
  <p className="text-muted-foreground text-sm">
    切换 Edit 工具的实现方式。改完即时生效，无需重启。
  </p>
  <RadioGroup
    value={settings.edit_backend}
    onValueChange={(v) => updateSettings({ edit_backend: v as Settings["edit_backend"] })}
  >
    <RadioGroupItem value="string-replace" id="eb-sr">
      字符串替换（默认）
      <span className="text-muted-foreground text-xs block">
        用 old_string / new_string 精确替换；适合精确小改
      </span>
    </RadioGroupItem>
    <RadioGroupItem value="hashline" id="eb-hl">
      Hashline（实验）
      <span className="text-muted-foreground text-xs block">
        按行号 + 内容指纹的紧凑 patch 格式；适合大文件局部改动
      </span>
    </RadioGroupItem>
  </RadioGroup>
</section>
```

⚠️ UI 文案纪律：**不要写**「Rust EditTool」「dispatch 注册」「parameters_schema」等内部术语。上面文案给用户讲场景与权衡。

- [ ] **Step 4: 验证类型**

```bash
pnpm exec tsc --noEmit
```
Expected: 通过

- [ ] **Step 5: 手动验证（dev 模式）**

```bash
pnpm tauri dev
# 打开 Settings → 切换到 Hashline → 让 agent 读一个文件
# 看 model_io.jsonl 确认 Read 输出已变为 ¶path#hash 格式
# 再让 agent 改一行，确认 Edit 收到 patch: "..." 入参
```

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/frontend/src/desktop/ apps/desktop/src/
git commit -m "$(cat <<'EOF'
desktop settings: 加 edit_backend 切换 UI

- Why: 用户能在不重启的情况下切换 Edit 后端做 A/B
- 影响范围: 设置弹窗；types.ts 同步加字段
- 留尾巴: 集成测试 + 架构文档 + changelog 下一批
EOF
)"
```

---

## Task 10: 集成测试：Read → Edit roundtrip

**Files:**
- Create: `crates/agent-core/tests/hashline_roundtrip.rs`

集成测试用真文件系统，端到端验证：
1. 建临时 workspace
2. 写一个 50 行文件
3. 用 `ReadHashlineTool` 读出来，拿到 `¶path#hash` + 1:line 输出
4. 构造一个 hashline patch（替换中间几行、用 `&A..B` 保留其他行）
5. 用 `EditHashlineTool.execute` 应用
6. 重新读，确认结果符合预期 + 新 hash 已变

- [ ] **Step 1: 写测试**

```rust
//! Read → Edit roundtrip 集成测试
use agent_core::edits::hashline::format::hash3;
use agent_core::read_state::ReadStateTracker;
use agent_core::tools::edit_hashline::EditHashlineTool;
use agent_core::tools::read_hashline::ReadHashlineTool;
use agent_core::tools::Tool;
use agent_core::workspace::Workspace;
use std::sync::Arc;

#[tokio::test]
async fn hashline_read_then_edit_then_read() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Arc::new(Workspace::for_test(dir.path()));
    let tracker = Arc::new(ReadStateTracker::new());

    let file_path = dir.path().join("foo.rs");
    let original: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
    std::fs::write(&file_path, &original).unwrap();

    let read_tool = ReadHashlineTool::new(workspace.clone(), Some(tracker.clone()));
    let edit_tool = EditHashlineTool::new(workspace.clone(), Some(tracker.clone()));

    // 1) Read
    let read_out = read_tool
        .execute(serde_json::json!({ "file_path": file_path.to_string_lossy() }))
        .await
        .unwrap();
    let h1 = hash3(&original);
    assert!(read_out.contains(&format!("#{}\n", h1)));
    assert!(read_out.contains("\n5:line 5\n"));

    // 2) Edit：替换 4..6 → 三个新行；保留 1..3 通过 hunk 之外
    let patch = format!(
        "¶foo.rs#{}\n4 6\n+L4-new\n+L5-new\n+L6-new\n",
        h1
    );
    edit_tool
        .execute(serde_json::json!({ "patch": patch }))
        .await
        .unwrap();

    let after = std::fs::read_to_string(&file_path).unwrap();
    let expected: String = ["line 1", "line 2", "line 3",
                            "L4-new", "L5-new", "L6-new",
                            "line 7", "line 8", "line 9", "line 10"]
        .iter()
        .map(|s| format!("{}\n", s))
        .collect();
    assert_eq!(after, expected);

    // 3) 再 Read，确认新 hash
    let read_out2 = read_tool
        .execute(serde_json::json!({ "file_path": file_path.to_string_lossy() }))
        .await
        .unwrap();
    let h2 = hash3(&after);
    assert_ne!(h1, h2);
    assert!(read_out2.contains(&format!("#{}\n", h2)));
}

#[tokio::test]
async fn hashline_stale_hash_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Arc::new(Workspace::for_test(dir.path()));
    let tracker = Arc::new(ReadStateTracker::new());
    let file_path = dir.path().join("a.txt");
    std::fs::write(&file_path, "x\n").unwrap();

    let read_tool = ReadHashlineTool::new(workspace.clone(), Some(tracker.clone()));
    let edit_tool = EditHashlineTool::new(workspace.clone(), Some(tracker.clone()));

    read_tool.execute(serde_json::json!({ "file_path": file_path.to_string_lossy() })).await.unwrap();
    // 外部改文件，模型不知情
    std::fs::write(&file_path, "y\n").unwrap();
    // 用旧 hash 试图改
    let patch = format!("¶a.txt#{}\n1 1\n+z\n", hash3("x\n"));
    let err = edit_tool.execute(serde_json::json!({ "patch": patch })).await.unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(s.contains("stale") || s.contains("hash") || s.contains("modified"));
}
```

⚠️ `Workspace::for_test` / `ReadStateTracker::new` 这些构造方式按 `read.rs` / `edit.rs` 现有集成测试抄。如果现有集成测试用别的 helper，照抄那个 helper。

- [ ] **Step 2: 跑测试**

```bash
cargo test -p agent-core --test hashline_roundtrip
```
Expected: 2 个 PASS

- [ ] **Step 3: Commit**

```bash
git add crates/agent-core/tests/hashline_roundtrip.rs
git commit -m "$(cat <<'EOF'
test: hashline roundtrip 集成测试

- Why: 端到端验证 Read→Edit 链路；锁住 stale hash 防御行为
- 影响范围: 仅测试
EOF
)"
```

---

## Task 11: 架构文档与 changelog

**Files:**
- Modify: `docs/架构.md`
- Modify: `docs/changelog.md`

- [ ] **Step 1: 架构.md 加段落**

找到 §4.4（工具）章节，加一小节"Edit 后端可插拔"。**不另起大章节**——这是工具子机制，不是新模块。

模板：

```markdown
#### 4.4.X Edit 后端可插拔

`Read` 与 `Edit` 工具有两套实现，由 `settings.edit_backend` 切换：

- `string-replace`（默认）：`Edit` 接受 `old_string` / `new_string` 精确替换；`Read` 输出 cat -n 风格行号
- `hashline`（实验）：`Edit` 接受 `patch: string`（hashline 文本格式：`¶path#HASH` 头 + `+add` / `&keep` / `EOF` 语法）；`Read` 输出 `¶path#HASH\nN:line` 形式

**为什么强耦合**：Hashline patch 里的行号与 hash 必须基于 `Read` 最近一次的输出，因此两个后端的 Read+Edit 必须成对切换，dispatch 注册时按 `edit_backend` 二选一。

**hash 是什么**：内容 SHA-256 前 12 bit (3 hex chars)，给模型做 stale 防御；读追踪仍用完整 SHA-256 做内部判定，互不依赖。

**实现位置**：
- 算法：`crates/agent-core/src/edits/hashline/{format,parser,apply}.rs`
- 工具壳：`crates/agent-core/src/tools/{read_hashline,edit_hashline}.rs`
- 教学 prompt：`crates/agent-core/src/edits/hashline/prompt.md`（include_str! 进 tool description）
```

- [ ] **Step 2: §13 决策表追加一行**

| 时间 | 决策 | 原因 |
|---|---|---|
| 2026-05-28 | 引入 hashline 作为 Edit 第二后端，settings 切换 | A/B 验证大文件局部改动场景下 token 与正确率收益 |

- [ ] **Step 3: changelog.md 追加一条**

```markdown
## 2026-05-28 — Hashline edit backend（试验性）

新增 oh-my-pi Hashline 风格的 Read+Edit 后端，settings 一键切换。

**Why**：
- oh-my-pi 用 hashline 解决了大文件局部改动 token 浪费 + 防 stale 编辑两件事
- Hebbian 现有 `Edit` 用 `old_string`/`new_string` 在大文件改一小块时仍要传完整 old_string，浪费 token
- 引入 hashline 后能 A/B 实测在 Claude 上的格式遵从度与正确率，再决定是否长期保留

**改动列表**：
- 新增 `crates/agent-core/src/edits/hashline/`：format（hash3 + 渲染）+ parser + apply + prompt.md
- 新增 `crates/agent-core/src/tools/{read_hashline,edit_hashline}.rs`
- `storage/settings.rs` 加 `edit_backend: EditBackend` 字段（默认 `StringReplace`，旧 settings.json 通过 `#[serde(default)]` 兼容）
- dispatch 按 settings 二选一注册 Read+Edit
- 前端 SettingsDialog 加切换 UI
- 集成测试 `tests/hashline_roundtrip.rs`

**借鉴细节**：参考了 oh-my-pi packages/hashline 的 parser / apply / prompt 设计。简化掉：流式恢复（Hebbian tool call 一次性）、move 语法 `*A,B`、创建/删除文件（首版只做 modify）。

**影响范围**：agent-core + desktop frontend；协议不变，model_io.jsonl 落盘格式不变（新增工具名仍是 Read / Edit）。

**留尾巴**：
- 试用后若决定保留，prompt.md 与 description 还可进一步精修
- 创建新文件 / 删除文件场景目前 hashline 后端不支持，模型遇到时需要回退到其他工具（hashline 后端下还没暴露替代方案，可能需要单独的 Write tool 或扩展 hashline 语法）
- 多文件 patch 跨 fs 操作目前是顺序写，中间失败时已写入的 section 不会回滚——hashline 后端首版接受这个风险
```

- [ ] **Step 4: Commit**

```bash
git add docs/架构.md docs/changelog.md
git commit -m "$(cat <<'EOF'
docs: 记录 hashline edit 后端

- 架构.md §4.4 加 Edit 后端可插拔小节
- §13 决策表追加一行
- changelog 追加 2026-05-28 条目
EOF
)"
```

---

## Self-Review Checklist

- [ ] **Spec coverage**：
  - settings 字段 ✅ Task 1
  - hash + 渲染 ✅ Task 2
  - parser ✅ Task 3
  - apply ✅ Task 4
  - prompt 教学 ✅ Task 5
  - Read tool ✅ Task 6
  - Edit tool ✅ Task 7
  - dispatch 切换 ✅ Task 8
  - 前端 UI ✅ Task 9
  - 集成测试 ✅ Task 10
  - 文档 ✅ Task 11

- [ ] **类型一致性**：`EditBackend` 全程一个名字；`hash3` / `parse_patch` / `apply_section` 在 task 3-7 之间签名一致；测试里的 `make_test_tool` / `mark_read_for_test` / `display_path` 在 task 6-7-10 一致

- [ ] **Placeholder 扫描**：Task 7 实现里有 `todo!()` 占位——明确标注是"实施时按现有 EditTool 错误风格替换"，**不是**模糊指令；Task 8 Step 1 让实施者用 codegraph 自己定位注册点——明确不是 TODO

- [ ] **karpathy-guidelines**：先复制再抽象（Task 6 明示）、不引入早熟抽象、错误处理只在系统边界（patch 解析失败、文件 IO）

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-28-hashline-edit-backend.md`. Two execution options:

**1. Subagent-Driven (recommended)** — 每个 Task 一个 fresh subagent，task 之间我做两阶段 review，迭代快

**2. Inline Execution** — 在当前 session 里按 task 顺序执行，每 2-3 个 task 一个 checkpoint 给你看

哪种走？