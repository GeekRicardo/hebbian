//! 简单的 shell 命令语义解析。
//!
//! 用途：让 [`BashTool::classify`] 能根据命令实际内容（不只是工具名）决定是否需要审批。
//!
//! 策略：
//! - 把整条 shell line 按 `&&` `||` `;` 切成多个**段**（segment）。
//! - 段内若含 `|`，进一步拆成多个**管道阶段**（stage）。
//! - 每个 stage 用 `shlex::split` 解析出 argv。
//! - 任何"危险结构"（重定向、命令替换、子 shell、变量赋值、后台 `&` 等）出现就标记
//!   `dangerous = true`——调用方应直接当成 destructive。
//!
//! 这不是一个完整的 bash AST，只是给「自动放行只读命令」做兜底用。原则：
//! **解析失败、识别到任何不熟悉的结构 → 一律 fall back 到完整审批**。

/// 单条解析后的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// 命令的根（argv[0]），例如 `git status -s` 的根是 `"git"`。
    pub root: String,
    /// 完整 argv，包含 root 自身。
    pub argv: Vec<String>,
}

impl ParsedCommand {
    /// 排除 flag 的位置参数序列，例如 `git status -uno README` → `["status", "README"]`。
    /// 用于判断 `git status` 这种"根 + 子命令"结构。
    pub fn positional(&self) -> Vec<&str> {
        self.argv
            .iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .map(|s| s.as_str())
            .collect()
    }
}

/// 一整行 shell 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedShell {
    /// 所有可识别的命令（按 `&&` `||` `;` `|` 拆分得到）。
    pub commands: Vec<ParsedCommand>,
    /// 解析过程中遇到的可疑结构（重定向、命令替换、子 shell 等）。
    /// 只要非空，调用方应当作不安全，直接走完整审批。
    pub dangerous: bool,
    /// `dangerous = true` 时附带的人类可读原因（用于 debug / 日志）。
    pub danger_reason: Option<String>,
}

/// 解析一整行 shell 命令。
///
/// 返回 `Err` 表示连最基本的 token 化都失败（例如未闭合的引号）；调用方应当成"不安全"处理。
pub fn parse(line: &str) -> Result<ParsedShell, ParseError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(ParseError::Empty);
    }

    // 1) 先按未引用区域的分隔符（&& || ; | & 以及 子 shell 边界）切。
    let segments = split_top_level(line)?;

    let mut commands = Vec::with_capacity(segments.len());
    let mut dangerous = false;
    let mut reason: Option<String> = None;

    for seg in segments {
        if seg.trim().is_empty() {
            continue;
        }
        // 2) 段内做危险结构嗅探（重定向、命令替换、subshell、变量赋值、heredoc...）。
        if let Some(why) = sniff_dangerous(&seg) {
            dangerous = true;
            reason.get_or_insert_with(|| why.to_string());
            continue;
        }
        // 3) shlex 拆 argv。
        let Some(argv) = shlex::split(&seg) else {
            return Err(ParseError::Tokenize(seg.clone()));
        };
        if argv.is_empty() {
            continue;
        }
        // 4) 形如 `FOO=bar cmd` 的环境变量前缀视为危险（影响行为）。
        if argv[0].contains('=') && !argv[0].starts_with('=') {
            dangerous = true;
            reason.get_or_insert_with(|| format!("inline env assignment: {}", argv[0]));
            continue;
        }
        commands.push(ParsedCommand {
            root: argv[0].clone(),
            argv,
        });
    }

    Ok(ParsedShell {
        commands,
        dangerous,
        danger_reason: reason,
    })
}

/// 解析失败。调用方都该当成「需要完整审批」。
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("空命令")]
    Empty,
    #[error("引号未闭合或词法错误：{0}")]
    Tokenize(String),
    #[error("未闭合的子 shell / 引号")]
    Unbalanced,
}

/// 在 top-level（不在引号内）按 `&&` `||` `;` `|` 切分。
///
/// 遇到 `(` `` ` `` `$(` 等会直接返回 [`ParseError::Unbalanced`]——交由 [`sniff_dangerous`]
/// 在段级也能识别出来，再次确认安全。
fn split_top_level(line: &str) -> Result<Vec<String>, ParseError> {
    let bytes = line.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();

    let mut in_single = false; // ' ... '
    let mut in_double = false; // " ... "
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;

        if in_single {
            buf.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            buf.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                buf.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }

        match c {
            '\'' => {
                in_single = true;
                buf.push(c);
                i += 1;
            }
            '"' => {
                in_double = true;
                buf.push(c);
                i += 1;
            }
            '\\' if i + 1 < bytes.len() => {
                // 转义字符：原样保留两字节交给 shlex 处理
                buf.push(c);
                buf.push(bytes[i + 1] as char);
                i += 2;
            }
            '&' if i + 1 < bytes.len() && bytes[i + 1] as char == '&' => {
                push_segment(&mut out, &mut buf);
                i += 2;
            }
            '|' if i + 1 < bytes.len() && bytes[i + 1] as char == '|' => {
                push_segment(&mut out, &mut buf);
                i += 2;
            }
            ';' | '|' => {
                push_segment(&mut out, &mut buf);
                i += 1;
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }

    if in_single || in_double {
        return Err(ParseError::Unbalanced);
    }
    push_segment(&mut out, &mut buf);
    Ok(out)
}

fn push_segment(out: &mut Vec<String>, buf: &mut String) {
    let seg = std::mem::take(buf);
    let trimmed = seg.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
}

/// 嗅探段内是否含「危险结构」。返回 `Some(原因)` 即认为该段不安全。
///
/// 这里只看不在引号内的特殊符号。能识别出来就降级为"必须审批"，
/// 不需要枚举所有 bash 语法，**保守即安全**。
fn sniff_dangerous(seg: &str) -> Option<&'static str> {
    let bytes = seg.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == '\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                i += 1;
            }
            '"' => {
                in_double = true;
                i += 1;
            }
            '\\' if i + 1 < bytes.len() => i += 2,
            '`' => return Some("backtick command substitution"),
            '$' if i + 1 < bytes.len() && bytes[i + 1] as char == '(' => {
                return Some("$(...) command substitution");
            }
            '$' if i + 1 < bytes.len() && bytes[i + 1] as char == '{' => {
                // ${VAR} 算可疑（可能被注入）；但太常见，单独标记
                return Some("${...} parameter expansion");
            }
            '<' if i + 1 < bytes.len() && bytes[i + 1] as char == '(' => {
                return Some("<(...) process substitution");
            }
            '>' if i + 1 < bytes.len() && bytes[i + 1] as char == '(' => {
                return Some(">(...) process substitution");
            }
            '<' | '>' => return Some("redirection"),
            '(' | ')' | '{' | '}' => return Some("subshell or group"),
            '&' => return Some("background execution"),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> ParsedShell {
        parse(line).unwrap_or_else(|e| panic!("parse failed for {line:?}: {e}"))
    }

    #[test]
    fn simple_command() {
        let r = cmd("ls -la");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 1);
        assert_eq!(r.commands[0].root, "ls");
        assert_eq!(r.commands[0].argv, vec!["ls", "-la"]);
    }

    #[test]
    fn split_double_amp() {
        let r = cmd("cd foo && rm -rf bar");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 2);
        assert_eq!(r.commands[0].root, "cd");
        assert_eq!(r.commands[1].root, "rm");
    }

    #[test]
    fn split_pipe() {
        let r = cmd("git log | head -5");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 2);
        assert_eq!(r.commands[0].root, "git");
        assert_eq!(r.commands[1].root, "head");
    }

    #[test]
    fn split_semicolon() {
        let r = cmd("ls ; pwd ; whoami");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 3);
    }

    #[test]
    fn split_or() {
        let r = cmd("test -f x || touch x");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 2);
    }

    #[test]
    fn redirection_is_dangerous() {
        let r = cmd("echo hi > /tmp/x");
        assert!(r.dangerous);
    }

    #[test]
    fn command_substitution_is_dangerous() {
        let r = cmd("echo $(whoami)");
        assert!(r.dangerous);
        let r = cmd("echo `whoami`");
        assert!(r.dangerous);
    }

    #[test]
    fn subshell_is_dangerous() {
        let r = cmd("(cd foo && ls)");
        assert!(r.dangerous);
    }

    #[test]
    fn background_is_dangerous() {
        let r = cmd("sleep 100 &");
        assert!(r.dangerous);
    }

    #[test]
    fn inline_env_is_dangerous() {
        let r = cmd("FOO=bar make");
        assert!(r.dangerous);
    }

    #[test]
    fn quotes_protect_special_chars() {
        let r = cmd("echo 'hi && bye'");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 1);
        assert_eq!(r.commands[0].argv, vec!["echo", "hi && bye"]);
    }

    #[test]
    fn double_quotes_protect_special_chars() {
        let r = cmd(r#"echo "a | b""#);
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 1);
    }

    #[test]
    fn unbalanced_quote_is_error() {
        assert!(parse("echo 'hi").is_err());
    }

    #[test]
    fn empty_is_error() {
        assert!(matches!(parse("   "), Err(ParseError::Empty)));
    }

    #[test]
    fn positional_skips_flags() {
        let r = cmd("git status -uno README.md");
        let pos = r.commands[0].positional();
        assert_eq!(pos, vec!["status", "README.md"]);
    }
}
