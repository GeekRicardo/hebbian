//! Shell 命令语义解析（架构 §4.4.2）。
//!
//! **段级判定**：一整行 shell 按 `&&` `||` `;` `|` 拆成多个段，每段独立产出：
//! - `argv`：tokenize 后的参数列表
//! - `write_targets`：段内识别到的写文件目标（`> FILE` / `>> FILE` / `tee FILE` /
//!   `sed -i FILE` / `cat > FILE` 等）；让 Edit deny 规则统一兜底 Bash 写文件
//! - 段级 [`fingerprint`](ParsedCommand::fingerprint)：剥掉 `timeout` / `nice` / `nohup` /
//!   `FOO=bar` 等修饰符 + 行内环境变量赋值后的 `"base [sub]"`（如 `git push`）。
//!   被分离出来的 env-var 放到 `env_prefix` 字段；命中 [`SENSITIVE_ENV_VARS`] 的 env-var
//!   会**升级为 [`DangerousKind::SensitiveEnvPrefix`]** 强制审批，覆盖任何 allow 规则
//!   ——保证 `LD_PRELOAD=evil ls` 不会被 `Bash(ls)` 静默放行
//!
//! 整行级再做一次 [`detect_dangerous_patterns`]：cd-git-compound / multi-cd /
//! write-git-meta / rm-rf-root / ast-too-complex。命中的危险模式**强制审批且不可
//! 记忆**——HitlGate 据此拒绝 AllowAndRemember 写盘。
//!
//! 与 [`super::safe_commands`] 的关系：`safe_commands::is_safe` 只看 `root` /
//! `argv`，因此本模块即使在重定向 / env-var 场景下也确保段被正常 push 到
//! `commands` 列表，让 safe_commands 能基于 argv 判断只读性。
//!
//! 红线：**解析失败 / 识别到任何不熟悉的结构 → 一律 fall back 到 dangerous**。

/// 单条解析后的命令（一段 plain command）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// 命令的根（argv[0]）。注意：剥离修饰符前的 root，例如 `timeout 30 git push`
    /// 的 root 是 `"timeout"`，[`fingerprint`](Self::fingerprint) 才返回剥后的 `"git push"`。
    pub root: String,
    /// 完整 argv，包含 root 自身。重定向部分已被剥离到 [`write_targets`](Self::write_targets)。
    /// **env-var 前缀仍保留在 argv 头部**——`safe_commands::is_safe` 等下游靠 argv 判断
    /// 安全性时不希望被剥得太干净；env_prefix 只是冗余视图供 fingerprint / 敏感检查用。
    pub argv: Vec<String>,
    /// 行内 env-var 赋值前缀（连续的 `FOO=bar` 形态）。剥离顺序与 [`fingerprint`](Self::fingerprint) 一致。
    /// 命中 [`SENSITIVE_ENV_VARS`] 的条目会触发 [`DangerousKind::SensitiveEnvPrefix`]。
    pub env_prefix: Vec<String>,
    /// 段内识别到的写文件目标：`>` / `>>` / `tee` / `sed -i` / `cat >` / `python -c open(...,'w')`
    /// 等。绝对/相对路径原样保留，由调用方做 canonicalize / 越界检查。
    pub write_targets: Vec<String>,
}

impl ParsedCommand {
    /// 排除 flag 的位置参数序列，例如 `git status -uno README` → `["status", "README"]`。
    pub fn positional(&self) -> Vec<&str> {
        self.argv
            .iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .map(|s| s.as_str())
            .collect()
    }

    /// 剥掉 timeout / nice / nohup / time / stdbuf / command / builtin / noglob 等修饰符
    /// **以及行内 env-var 赋值**后的 fingerprint。env-var 通过 [`env_prefix`](Self::env_prefix)
    /// 字段单独承载；命中敏感名单时由 [`DangerousKind::SensitiveEnvPrefix`] 拦截。
    ///
    /// 形如 `<modifier?> <env?> <base> [...flags] <sub?>`，输出 `base [sub]`：
    ///
    /// | 输入 argv | fingerprint | env_prefix |
    /// |---|---|---|
    /// | `timeout 30 git push origin main` | `git push` | `[]` |
    /// | `FOO=bar nice -n 10 cargo build` | `cargo build` | `["FOO=bar"]` |
    /// | `PYTHONPATH=/tmp python3 script.py` | `python3 script.py` | `["PYTHONPATH=/tmp"]`（敏感）|
    /// | `nohup -- npm install` | `npm install` | `[]` |
    /// | `ls -la` | `ls` | `[]` |
    /// | `rm -rf /tmp/x` | `rm /tmp/x` | `[]` |
    pub fn fingerprint(&self) -> String {
        let (_env_prefix, base_argv) = strip_prefix(&self.argv);
        if base_argv.is_empty() {
            // 整条命令都是修饰符 / env 赋值（异常输入）→ 回落到 root
            return self.root.clone();
        }
        let base = &base_argv[0];
        let sub = base_argv.iter().skip(1).find(|a| !a.starts_with('-'));
        match sub {
            Some(s) => format!("{base} {s}"),
            None => base.clone(),
        }
    }
}

/// 一整行 shell 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedShell {
    /// 所有可识别的命令（按 `&&` `||` `;` `|` 拆分得到）。
    pub commands: Vec<ParsedCommand>,
    /// 解析过程中遇到的可疑结构（命令替换、subshell、后台 `&`、解析失败等）。
    /// 只要非空，调用方应当作不安全，直接走完整审批。
    pub dangerous: bool,
    /// `dangerous = true` 时附带的人类可读原因（用于 debug / 日志）。
    pub danger_reason: Option<String>,
    /// 整行级别识别到的危险复合模式（§4.4.2.2）。命中任一种均强制审批，
    /// 且 HitlGate 拒绝 AllowAndRemember 落盘。
    pub dangerous_kinds: Vec<DangerousKind>,
}

/// 整行级危险复合模式（架构 §4.4.2.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DangerousKind {
    /// `cd <path> && git ...`：可触发 target 目录的不可信 `.git/hooks`。
    CdGitCompound,
    /// 单行多次 cd。
    MultiCd,
    /// 写入 `.git/hooks/**` / `.git/config` / `HEAD` / `objects/...` / `refs/...`。
    WriteGitMeta(String),
    /// `rm -rf` 命中 `/` / `~` / `$HOME` / `..` 等根级路径。
    RmRfRoot(String),
    /// 命中 [`SENSITIVE_ENV_VARS`]（LD_PRELOAD / DYLD_INSERT_LIBRARIES / PYTHONPATH 等）。
    SensitiveEnvPrefix(Vec<String>),
    /// AST 拆段超过本模块的覆盖范围（嵌套 subshell / 命令替换 / 异常 quoting）。
    AstTooComplex,
}

impl DangerousKind {
    pub fn label(&self) -> &'static str {
        match self {
            DangerousKind::CdGitCompound => "cd-git-compound",
            DangerousKind::MultiCd => "multi-cd",
            DangerousKind::WriteGitMeta(_) => "write-git-meta",
            DangerousKind::RmRfRoot(_) => "rm-rf-root",
            DangerousKind::SensitiveEnvPrefix(_) => "sensitive-env-prefix",
            DangerousKind::AstTooComplex => "ast-too-complex",
        }
    }
}

/// 已知的"动态库 / 解释器注入"环境变量名单（架构 §4.4.2.1）。
/// 命中任一即触发 [`DangerousKind::SensitiveEnvPrefix`]，强制审批。
pub const SENSITIVE_ENV_VARS: &[&str] = &[
    // Linux / macOS 动态链接器
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    // Python
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONSTARTUP",
    // Perl
    "PERL5LIB",
    "PERL5OPT",
    // Ruby
    "RUBYLIB",
    "RUBYOPT",
    // Node
    "NODE_OPTIONS",
    "NODE_PATH",
    // Shell 字段分隔符（IFS 注入）
    "IFS",
];

/// 判断一个形如 `NAME=value` / `NAME+=value` 的 token 是否命中敏感名单。
pub fn is_sensitive_env(assignment: &str) -> bool {
    let Some(eq) = assignment.find('=') else {
        return false;
    };
    let name = &assignment[..eq];
    let name = name.trim_end_matches('+');
    SENSITIVE_ENV_VARS.contains(&name)
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

/// 解析一整行 shell 命令。
pub fn parse(line: &str) -> Result<ParsedShell, ParseError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(ParseError::Empty);
    }

    let segments = split_top_level(line)?;

    let mut commands = Vec::with_capacity(segments.len());
    let mut dangerous = false;
    let mut reason: Option<String> = None;
    let mut dangerous_kinds: Vec<DangerousKind> = Vec::new();

    for seg in segments {
        if seg.trim().is_empty() {
            continue;
        }

        // 1) 抽离重定向 → write_targets，得到 cleaned segment 给 shlex
        let (cleaned, write_targets) = extract_redirections(&seg);

        // 2) 复杂结构检测（$() / `…` / <(...) / >(...) / subshell / 后台 &）
        if let Some(why) = sniff_complex_structure(&cleaned) {
            dangerous = true;
            reason.get_or_insert_with(|| why.to_string());
            if !dangerous_kinds.contains(&DangerousKind::AstTooComplex) {
                dangerous_kinds.push(DangerousKind::AstTooComplex);
            }
            // 段无法可靠 tokenize，跳过此段——但段已不影响 fingerprint 一致性，
            // 因为整行已被标 AstTooComplex 强制审批
            continue;
        }

        // 3) shlex.split 取 argv（含 env-var 前缀；env-var 不再阻塞 push，让 fingerprint 能识别）
        let Some(argv) = shlex::split(&cleaned) else {
            dangerous = true;
            reason.get_or_insert_with(|| format!("tokenize failed: {cleaned}"));
            dangerous_kinds.push(DangerousKind::AstTooComplex);
            continue;
        };
        if argv.is_empty() {
            continue;
        }

        // 4) 拆出 env_prefix（不剥离 argv，env-var 留在 argv 头部供 safe_commands 等下游使用）
        let (env_prefix, _base_argv) = strip_prefix(&argv);

        // 5) 命中敏感 env-var → 整行级 DangerousKind
        let sensitive: Vec<String> = env_prefix
            .iter()
            .filter(|e| is_sensitive_env(e))
            .cloned()
            .collect();
        if !sensitive.is_empty() {
            let kind = DangerousKind::SensitiveEnvPrefix(sensitive);
            if !dangerous_kinds.iter().any(|k| k.label() == kind.label()) {
                dangerous_kinds.push(kind);
            }
        }

        // 6) 组合 ParsedCommand：含写目标也保留段
        let cmd = ParsedCommand {
            root: argv[0].clone(),
            argv,
            env_prefix,
            write_targets,
        };
        commands.push(cmd);
    }

    // 7) 写目标里追加 sed -i / tee 这类需要 argv 才能识别的目标
    for cmd in commands.iter_mut() {
        let mut extra = collect_argv_write_targets(cmd);
        cmd.write_targets.append(&mut extra);
    }

    // 8) 整行级危险模式（cd-git / multi-cd / write-git-meta / rm-rf-root）
    let extra_kinds = detect_dangerous_patterns(&commands);
    for k in extra_kinds {
        if !dangerous_kinds.contains(&k) {
            dangerous_kinds.push(k);
        }
    }
    if !dangerous_kinds.is_empty() && !dangerous {
        // 整行危险模式独立于 sniff 结果，也要让 dangerous=true 兼容旧调用方
        dangerous = true;
        reason.get_or_insert_with(|| {
            dangerous_kinds
                .iter()
                .map(DangerousKind::label)
                .collect::<Vec<_>>()
                .join(",")
        });
    }

    Ok(ParsedShell {
        commands,
        dangerous,
        danger_reason: reason,
        dangerous_kinds,
    })
}

/// 在 top-level（不在引号内）按 `&&` `||` `;` `|` 切分。
fn split_top_level(line: &str) -> Result<Vec<String>, ParseError> {
    let bytes = line.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();

    let mut in_single = false;
    let mut in_double = false;
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

/// 从段中抽出重定向写目标（`>` `>>` `&>` `2>` 等），返回 `(cleaned, targets)`。
/// `<<EOF` heredoc 与 `< FILE` 读重定向不算写——前者写到子进程 stdin，后者纯读。
fn extract_redirections(seg: &str) -> (String, Vec<String>) {
    let bytes = seg.as_bytes();
    let mut cleaned = String::with_capacity(seg.len());
    let mut targets = Vec::new();

    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;

        if in_single {
            cleaned.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            cleaned.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                cleaned.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }

        // 识别重定向 token 前先消化引号
        if c == '\'' {
            in_single = true;
            cleaned.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            cleaned.push(c);
            i += 1;
            continue;
        }

        // 重定向匹配：`>>` `>` `&>` `2>` `2>>` `1>` `1>>` `<<` heredoc
        // heredoc 不抽出（写到子进程 stdin，不写文件系统）；`<` 读，跳过
        let rest = &seg[i..];
        if rest.starts_with(">>")
            || rest.starts_with("&>")
            || rest.starts_with("1>>")
            || rest.starts_with("2>>")
            || rest.starts_with("1>")
            || rest.starts_with("2>")
        {
            let op_len = if rest.starts_with("1>>") || rest.starts_with("2>>") {
                3
            } else if rest.starts_with(">>")
                || rest.starts_with("&>")
                || rest.starts_with("1>")
                || rest.starts_with("2>")
            {
                2
            } else {
                1
            };
            // 跳过 op + 空白，抓 token 作为目标
            let mut j = i + op_len;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            // fd 复制（`2>&1` / `1>&2` / `&>&-` 等）：scan_token 会因 `&` 开头返回 None,
            // 单纯跳 op 会把 `&1` 留在 cleaned 让后续 sniff 误判为后台 `&`。这里先识别
            // 整段 fd-dup 一次性消耗：op + 可选空白 + `&` + 数字/`-`。
            if let Some(dup_end) = scan_fd_dup(seg, j) {
                i = dup_end;
                continue;
            }
            if let Some((target, end)) = scan_token(seg, j) {
                // 排除 `>&N` 类的 fd 复制（兜底：极少数 scan_fd_dup 漏的情况）
                if !target.starts_with('&') {
                    targets.push(target);
                }
                i = end;
            } else {
                // 没有合法 token → 异常，保守把 op 也吃掉
                i += op_len;
            }
            continue;
        }
        if c == '>' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if let Some(dup_end) = scan_fd_dup(seg, j) {
                i = dup_end;
                continue;
            }
            if let Some((target, end)) = scan_token(seg, j) {
                if !target.starts_with('&') {
                    targets.push(target);
                }
                i = end;
            } else {
                i += 1;
            }
            continue;
        }
        if rest.starts_with("<<") {
            // heredoc：跳过 `<<` + 可选 `-` + delimiter token；不抽目标
            let mut j = i + 2;
            if j < bytes.len() && bytes[j] == b'-' {
                j += 1;
            }
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if let Some((_, end)) = scan_token(seg, j) {
                i = end;
            } else {
                i += 2;
            }
            continue;
        }
        if c == '<' {
            // 读重定向：跳过 `<` + token，不算写
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if let Some((_, end)) = scan_token(seg, j) {
                i = end;
            } else {
                i += 1;
            }
            continue;
        }

        cleaned.push(c);
        i += 1;
    }

    (cleaned, targets)
}

/// 识别 fd 复制（`&N` / `&-`）形态，返回吃完该段后的 end index。
/// 用法：在 `>` / `2>` / `1>` 等 op 后调一次，命中即整段吞掉，避免把 `&1` 留给后续误判。
fn scan_fd_dup(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if start >= bytes.len() || bytes[start] != b'&' {
        return None;
    }
    let mut j = start + 1;
    if j >= bytes.len() {
        return None;
    }
    // `&-` 关闭描述符；`&N` 复制到 fd N
    if bytes[j] == b'-' {
        return Some(j + 1);
    }
    if !bytes[j].is_ascii_digit() {
        return None;
    }
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    Some(j)
}

/// 从 `s[start..]` 抓一个 shell token（含简单引号包裹）。返回 `(token_value, end_index)`。
fn scan_token(s: &str, start: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if start >= bytes.len() {
        return None;
    }
    let first = bytes[start] as char;
    if first == ' ' || first == '\t' || first == '|' || first == '&' || first == ';' {
        return None;
    }

    let mut i = start;
    let mut buf = String::new();
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_single {
            if c == '\'' {
                in_single = false;
                i += 1;
                continue;
            }
            buf.push(c);
            i += 1;
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
                i += 1;
                continue;
            }
            if c == '\\' && i + 1 < bytes.len() {
                buf.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            buf.push(c);
            i += 1;
            continue;
        }
        if c == '\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            i += 1;
            continue;
        }
        if c == ' ' || c == '\t' || c == '|' || c == '&' || c == ';' || c == '>' || c == '<' {
            break;
        }
        buf.push(c);
        i += 1;
    }
    if buf.is_empty() {
        None
    } else {
        Some((buf, i))
    }
}

/// 嗅探段内是否含命令替换 / process substitution / subshell / 后台 `&`。
/// **不再**把 `>` `<` 重定向标 dangerous（已被 [`extract_redirections`] 抽走）。
/// **不再**把 inline `FOO=bar` 标 dangerous（已在 fingerprint 保留为 env 前缀）。
fn sniff_complex_structure(seg: &str) -> Option<&'static str> {
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
            '<' if i + 1 < bytes.len() && bytes[i + 1] as char == '(' => {
                return Some("<(...) process substitution");
            }
            '>' if i + 1 < bytes.len() && bytes[i + 1] as char == '(' => {
                return Some(">(...) process substitution");
            }
            '(' | ')' | '{' | '}' => return Some("subshell or group"),
            '&' => return Some("background execution"),
            // `#` 后裸文本视为注释注入（claude code 调研结论）
            '#' if i == 0 || matches!(bytes[i - 1] as char, ' ' | '\t') => {
                return Some("comment injection");
            }
            _ => i += 1,
        }
    }
    None
}

/// 时间 / 优先级 / 调度修饰符表（按 token 比较，配 flag 形如 `-n N`）。
const SCHED_MODIFIERS: &[&str] = &[
    "timeout", "time", "nice", "stdbuf", "nohup", "command", "builtin", "noglob", "ionice",
];

/// 剥离修饰符前缀。返回 `(env_assignments, base_argv)`。
/// `env_assignments` 是连续的 `FOO=bar` 段；`base_argv` 是真正的命令行。
///
/// 调度修饰符与 env-var **可以交错**：`timeout 30 FOO=bar nice -n 10 cargo build` 期望
/// fingerprint 为 `cargo build`、env_prefix 为 `["FOO=bar"]`。因此外层 loop 直到再也剥
/// 不动为止——每一轮内剥一次修饰符 + 连续 env-var。
pub fn strip_prefix(argv: &[String]) -> (Vec<String>, Vec<String>) {
    let mut i = 0;
    let mut env: Vec<String> = Vec::new();
    loop {
        let before = i;

        // (a) 剥时间 / 优先级 / 调度修饰符（最多一次）
        if i < argv.len() && SCHED_MODIFIERS.contains(&argv[i].as_str()) {
            let tok = argv[i].as_str();
            i += 1;
            // 吃掉该修饰符的 flag / 数值参数
            match tok {
                "timeout" => {
                    // 可选 flags：--foreground / --preserve-status / --verbose / -k DURATION / --kill-after=… / -s SIG / --signal=…
                    while i < argv.len() && argv[i].starts_with("--") {
                        i += 1;
                    }
                    while i < argv.len() && (argv[i] == "-k" || argv[i] == "-s" || argv[i] == "--")
                    {
                        i += if argv[i] == "--" { 1 } else { 2 };
                    }
                    // 时长 token
                    if i < argv.len() && is_duration_or_num(&argv[i]) {
                        i += 1;
                    }
                }
                "nice" => {
                    // `-n N` 或 `-N`
                    if i < argv.len() && argv[i] == "-n" {
                        i += 2.min(argv.len() - i + 1);
                    } else if i < argv.len()
                        && argv[i].starts_with('-')
                        && argv[i][1..].chars().all(|c| c.is_ascii_digit() || c == '-')
                    {
                        i += 1;
                    }
                }
                "stdbuf" => {
                    // `-i…` / `-o…` / `-e…`
                    while i < argv.len() && argv[i].starts_with("-") && argv[i].len() >= 2 {
                        let b = argv[i].as_bytes()[1] as char;
                        if matches!(b, 'i' | 'o' | 'e') {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
                "ionice" => {
                    while i < argv.len() && argv[i].starts_with('-') {
                        i += 1;
                    }
                }
                "nohup" | "command" | "builtin" | "noglob" | "time" => {
                    if i < argv.len() && argv[i] == "--" {
                        i += 1;
                    }
                }
                _ => {}
            }
        }

        // (b) 行内环境变量赋值（连续吃）
        while i < argv.len() && is_env_assignment(&argv[i]) {
            env.push(argv[i].clone());
            i += 1;
        }

        // 一轮内剥不动任何东西 → 跳出，让后续 base 取剩余 argv
        if i == before {
            break;
        }
    }

    let base = if i < argv.len() {
        argv[i..].to_vec()
    } else {
        Vec::new()
    };
    (env, base)
}

fn is_env_assignment(tok: &str) -> bool {
    if let Some(eq) = tok.find('=') {
        if eq == 0 {
            return false;
        }
        let name = &tok[..eq];
        name.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic() || c == '_'
            } else {
                c.is_ascii_alphanumeric() || c == '_'
            }
        })
    } else {
        false
    }
}

fn is_duration_or_num(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let (digits, suffix) = match tok.chars().last().unwrap() {
        'd' | 'h' | 'm' | 's' => (&tok[..tok.len() - 1], true),
        _ => (tok, false),
    };
    let _ = suffix;
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// 从 argv 抽出 sed -i / tee 类需要语义识别的写目标（重定向已在 extract_redirections 抽走）。
fn collect_argv_write_targets(cmd: &ParsedCommand) -> Vec<String> {
    let mut out = Vec::new();
    let (_env, base) = strip_prefix(&cmd.argv);
    if base.is_empty() {
        return out;
    }
    let root = base[0].as_str();
    match root {
        "tee" => {
            // tee [-a] [--] FILE [FILE...]
            for tok in base.iter().skip(1) {
                if tok == "-a" || tok == "--append" || tok == "--" || tok.starts_with('-') {
                    continue;
                }
                out.push(tok.clone());
            }
        }
        "sed" => {
            // sed -i FILE / sed -i'.bak' FILE / sed --in-place FILE
            let in_place = base.iter().any(|t| {
                t == "-i" || t.starts_with("-i") || t == "--in-place" || t.starts_with("--in-place")
            });
            if in_place {
                // 取最后一个非 flag 的位置参数当目标（第一个非 flag 通常是 sed 脚本）
                let positional: Vec<&String> = base
                    .iter()
                    .skip(1)
                    .filter(|t| !t.starts_with('-'))
                    .collect();
                if positional.len() >= 2 {
                    if let Some(target) = positional.last() {
                        out.push((*target).clone());
                    }
                }
            }
        }
        "python" | "python3" | "python2" => {
            // python -c "open('foo','w')..." / python -c "open(...,'a')..."
            for i in 1..base.len() {
                if base[i] == "-c" && i + 1 < base.len() {
                    let body = &base[i + 1];
                    if let Some(target) = python_open_target(body) {
                        out.push(target);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// 简单识别 `open('FILE','w'|'a'|'wb'|...)` / `open("FILE", ...)`，返回 FILE。
fn python_open_target(body: &str) -> Option<String> {
    let idx = body.find("open(")?;
    let after = &body[idx + 5..];
    let bytes = after.as_bytes();
    let first = bytes.first()?;
    let (quote, content_start) = match *first {
        b'\'' => (b'\'', 1),
        b'"' => (b'"', 1),
        _ => return None,
    };
    let end = after[content_start..]
        .as_bytes()
        .iter()
        .position(|&b| b == quote)?;
    let path = &after[content_start..content_start + end];
    // 后面应当跟逗号 + 写模式
    let rest = &after[content_start + end + 1..];
    let rest_trim = rest.trim_start();
    if !rest_trim.starts_with(',') {
        return None;
    }
    let mode_part = rest_trim[1..].trim_start();
    if mode_part.starts_with('\'') || mode_part.starts_with('"') {
        let q = mode_part.as_bytes()[0];
        let m_end = mode_part[1..].as_bytes().iter().position(|&b| b == q)?;
        let mode = &mode_part[1..1 + m_end];
        if mode.contains('w') || mode.contains('a') || mode.contains('x') || mode.contains('+') {
            return Some(path.to_string());
        }
    }
    None
}

/// 整行级危险复合模式检测（架构 §4.4.2.2）。
pub fn detect_dangerous_patterns(commands: &[ParsedCommand]) -> Vec<DangerousKind> {
    let mut kinds = Vec::new();

    // multi-cd / cd-git-compound
    let cd_count = commands.iter().filter(|c| c.root == "cd").count();
    if cd_count >= 2 {
        kinds.push(DangerousKind::MultiCd);
    }
    if cd_count >= 1 {
        // cd 之后是否出现 git 段
        let mut seen_cd = false;
        for cmd in commands {
            if cmd.root == "cd" {
                seen_cd = true;
            } else if seen_cd && cmd.root == "git" {
                kinds.push(DangerousKind::CdGitCompound);
                break;
            }
        }
    }

    // write-git-meta：写目标命中 git 元数据
    for cmd in commands {
        for t in &cmd.write_targets {
            if is_git_meta_path(t) {
                kinds.push(DangerousKind::WriteGitMeta(t.clone()));
            }
        }
    }

    // rm-rf-root：rm -r/-rf/-fr 命中根级路径
    for cmd in commands {
        if cmd.root != "rm" {
            continue;
        }
        let recursive = cmd.argv.iter().any(|a| {
            a == "-r"
                || a == "-R"
                || a == "--recursive"
                || (a.starts_with('-')
                    && !a.starts_with("--")
                    && (a.contains('r') || a.contains('R')))
        });
        if !recursive {
            continue;
        }
        for tok in cmd.argv.iter().skip(1) {
            if tok.starts_with('-') {
                continue;
            }
            if is_root_level_path(tok) {
                kinds.push(DangerousKind::RmRfRoot(tok.clone()));
            }
        }
    }

    kinds
}

fn is_git_meta_path(p: &str) -> bool {
    let trimmed = p.trim_start_matches("./");
    trimmed.contains(".git/hooks")
        || trimmed.contains(".git/config")
        || trimmed.ends_with(".git/HEAD")
        || trimmed == ".git/HEAD"
        || trimmed.contains(".git/objects")
        || trimmed.contains(".git/refs")
}

fn is_root_level_path(p: &str) -> bool {
    matches!(p, "/" | "~" | "$HOME" | ".." | "../" | "/*" | "~/" | "~/.*")
        || p.starts_with("/*")
        || p.starts_with("~/*")
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
        assert_eq!(r.commands[0].fingerprint(), "ls");
    }

    #[test]
    fn split_double_amp() {
        let r = cmd("cd foo && rm -rf bar");
        assert_eq!(r.commands.len(), 2);
        assert_eq!(r.commands[0].root, "cd");
        assert_eq!(r.commands[1].root, "rm");
    }

    #[test]
    fn split_pipe() {
        let r = cmd("git log | head -5");
        assert_eq!(r.commands.len(), 2);
    }

    #[test]
    fn redirection_extracted_to_write_targets() {
        let r = cmd("echo hi > /tmp/x");
        // 段保留，重定向被抽走
        assert_eq!(r.commands.len(), 1);
        assert_eq!(r.commands[0].argv, vec!["echo", "hi"]);
        assert_eq!(r.commands[0].write_targets, vec!["/tmp/x".to_string()]);
    }

    #[test]
    fn append_redirection() {
        let r = cmd("echo hi >> /tmp/x");
        assert_eq!(r.commands[0].write_targets, vec!["/tmp/x".to_string()]);
    }

    #[test]
    fn fd_dup_not_a_write_target() {
        let r = cmd("foo 2>&1");
        // `&1` 是 fd dup，不当写目标
        assert!(r.commands[0].write_targets.is_empty());
    }

    #[test]
    fn read_redirection_not_write() {
        let r = cmd("cat < /etc/hosts");
        assert!(r.commands[0].write_targets.is_empty());
    }

    #[test]
    fn tee_extracts_write_targets() {
        let r = cmd("foo | tee a.log b.log");
        // 第二段是 tee
        let tee_seg = &r.commands[1];
        assert!(tee_seg.write_targets.iter().any(|t| t == "a.log"));
        assert!(tee_seg.write_targets.iter().any(|t| t == "b.log"));
    }

    #[test]
    fn sed_in_place_extracts_target() {
        let r = cmd("sed -i 's/a/b/' file.txt");
        assert_eq!(r.commands[0].write_targets, vec!["file.txt".to_string()]);
    }

    #[test]
    fn python_open_write() {
        let r = cmd(r#"python -c "open('secrets.txt','w').write('x')""#);
        assert_eq!(r.commands[0].write_targets, vec!["secrets.txt".to_string()]);
    }

    #[test]
    fn command_substitution_is_dangerous() {
        let r = cmd("echo $(whoami)");
        assert!(r.dangerous);
        assert!(r.dangerous_kinds.contains(&DangerousKind::AstTooComplex));
        let r = cmd("echo `whoami`");
        assert!(r.dangerous);
    }

    #[test]
    fn subshell_is_dangerous() {
        let r = cmd("(cd foo && ls)");
        assert!(r.dangerous);
        assert!(r.dangerous_kinds.contains(&DangerousKind::AstTooComplex));
    }

    #[test]
    fn background_is_dangerous() {
        let r = cmd("sleep 100 &");
        assert!(r.dangerous);
    }

    #[test]
    fn inline_env_separated_to_env_prefix() {
        let r = cmd("FOO=bar make all");
        // env 段不再被跳过，正常 push 进 commands
        assert_eq!(r.commands.len(), 1);
        // argv 头部保留 env-var（safe_commands 等下游靠 argv 兜底判断）
        assert_eq!(r.commands[0].argv, vec!["FOO=bar", "make", "all"]);
        // 而 env_prefix 单独维护剥离视图，fingerprint 不含 env
        assert_eq!(r.commands[0].env_prefix, vec!["FOO=bar"]);
        assert_eq!(r.commands[0].fingerprint(), "make all");
        // 普通 env-var 不触发 DangerousKind
        assert!(!r
            .dangerous_kinds
            .iter()
            .any(|k| matches!(k, DangerousKind::SensitiveEnvPrefix(_))));
    }

    #[test]
    fn sensitive_env_var_triggers_dangerous_kind() {
        let r = cmd("LD_PRELOAD=/tmp/evil.so ls -la");
        assert!(r
            .dangerous_kinds
            .iter()
            .any(|k| matches!(k, DangerousKind::SensitiveEnvPrefix(_))));
        // fingerprint 仍按真实命令产出，不被 env 污染
        assert_eq!(r.commands[0].fingerprint(), "ls");
        assert_eq!(r.commands[0].env_prefix, vec!["LD_PRELOAD=/tmp/evil.so"]);
    }

    #[test]
    fn pythonpath_is_sensitive() {
        let r = cmd("PYTHONPATH=/tmp python3 script.py");
        assert!(r
            .dangerous_kinds
            .iter()
            .any(|k| matches!(k, DangerousKind::SensitiveEnvPrefix(_))));
        assert_eq!(r.commands[0].fingerprint(), "python3 script.py");
    }

    #[test]
    fn timeout_modifier_stripped_from_fingerprint() {
        let r = cmd("timeout 30 git push origin main");
        assert_eq!(r.commands.len(), 1);
        assert_eq!(r.commands[0].fingerprint(), "git push");
    }

    #[test]
    fn nice_modifier_stripped_from_fingerprint() {
        let r = cmd("nice -n 10 cargo build");
        assert_eq!(r.commands[0].fingerprint(), "cargo build");
    }

    #[test]
    fn nohup_dash_dash_stripped() {
        let r = cmd("nohup -- npm install");
        assert_eq!(r.commands[0].fingerprint(), "npm install");
    }

    #[test]
    fn combined_modifiers_with_env() {
        let r = cmd("timeout 30 FOO=bar nice -n 10 cargo build --release");
        assert_eq!(r.commands[0].fingerprint(), "cargo build");
        assert_eq!(r.commands[0].env_prefix, vec!["FOO=bar"]);
    }

    #[test]
    fn fingerprint_strips_flags() {
        let r = cmd("rm -rf /tmp/x");
        assert_eq!(r.commands[0].fingerprint(), "rm /tmp/x");
        let r2 = cmd("git status -uno README.md");
        assert_eq!(r2.commands[0].fingerprint(), "git status");
    }

    #[test]
    fn dangerous_cd_git_compound() {
        let r = cmd("cd /tmp/evil && git status");
        assert!(r.dangerous_kinds.contains(&DangerousKind::CdGitCompound));
    }

    #[test]
    fn dangerous_multi_cd() {
        let r = cmd("cd /a && cd /b && ls");
        assert!(r.dangerous_kinds.contains(&DangerousKind::MultiCd));
    }

    #[test]
    fn dangerous_write_git_meta_via_redirect() {
        let r = cmd("echo evil > /repo/.git/hooks/post-merge");
        assert!(
            r.dangerous_kinds
                .iter()
                .any(|k| matches!(k, DangerousKind::WriteGitMeta(_))),
            "expected WriteGitMeta in {:?}",
            r.dangerous_kinds
        );
    }

    #[test]
    fn dangerous_rm_rf_root() {
        let r = cmd("rm -rf /");
        assert!(r
            .dangerous_kinds
            .iter()
            .any(|k| matches!(k, DangerousKind::RmRfRoot(_))));
        let r2 = cmd("rm -rf ~");
        assert!(r2
            .dangerous_kinds
            .iter()
            .any(|k| matches!(k, DangerousKind::RmRfRoot(_))));
    }

    #[test]
    fn rm_rf_to_subdir_is_safe_from_root_pattern() {
        let r = cmd("rm -rf ./build");
        assert!(!r
            .dangerous_kinds
            .iter()
            .any(|k| matches!(k, DangerousKind::RmRfRoot(_))));
    }

    #[test]
    fn quotes_protect_special_chars() {
        let r = cmd("echo 'hi && bye'");
        assert!(!r.dangerous);
        assert_eq!(r.commands.len(), 1);
        assert_eq!(r.commands[0].argv, vec!["echo", "hi && bye"]);
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

    #[test]
    fn comment_injection_marked_dangerous() {
        let r = cmd("git status # ; rm -rf /");
        assert!(r.dangerous);
    }
}
