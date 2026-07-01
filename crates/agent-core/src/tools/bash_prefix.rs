//! Classifier A — Bash 命令前缀提取。
//!
//! 从一段 shell 命令提取用于规则匹配的 **prefix**：
//! - `Prefix("git commit")` → 可拿去查 `Bash(git commit *)` 等 allow/deny 规则
//! - `None` → 太宽泛（如裸 `git`、`npm run`），不形成 prefix，走 ask
//! - `CommandInjectionDetected` → 命令注入（`$()`、反引号、进程替换等），强制 ask
//!
//! `extract_prefix` 是本地启发式 fallback，`classify_prefix` 是 AutoMode 下可选的
//! LLM 版本：严格解析 `prefix` / `none` / `command_injection_detected` 三种输出。

use std::sync::Arc;

use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};
use tracing::warn;

pub const BASH_PREFIX_CLASSIFIER_SYSTEM: &str = r#"You extract the authorization prefix for one bash command segment.

Output exactly one of:
- prefix: <bash prefix>
- none
- command_injection_detected

Rules:
- Strip scheduling wrappers such as timeout, time, nice, stdbuf, nohup, command, builtin, noglob.
- Preserve inline environment assignments in the prefix if they change the command meaning.
- For dispatcher-style commands such as git, npm, pnpm, yarn, cargo, docker, kubectl, gh, aws, go, use base plus verb as the prefix.
- For single-command tools such as cat, cd, find, grep, pytest, sleep, ls, use only the base command.
- Do not include file paths, flags, branch names, remotes, or script arguments after the verb.
- If the segment contains command substitution, backticks, process substitution, comment injection, or an extra command hidden after a newline, output command_injection_detected.
- If the command is too broad to form a stable allowlist prefix, output none.

Examples:
cat foo.txt -> prefix: cat
git commit -m "foo" -> prefix: git commit
git push -> none
git push origin master -> prefix: git push
npm test -> none
npm test --foo -> prefix: npm test
npm run lint -> none
npm run lint -- "foo" -> prefix: npm run lint
FOO=bar BAZ=qux ls -la -> prefix: FOO=bar BAZ=qux ls
PYTHONPATH=/tmp python3 script.py arg1 -> prefix: PYTHONPATH=/tmp python3
git status`ls` -> command_injection_detected
"#;

/// 提取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashPrefix {
    /// 成功提取到可用于规则匹配的前缀，如 `"git commit"`、`"cargo test"`。
    Prefix(String),
    /// 太宽泛或无法形成有意义前缀（裸 dispatcher、纯 flag 等）。
    None,
    /// 检测到命令注入模式（`$()`、反引号、进程替换、多行拼接）。
    CommandInjectionDetected,
}

impl BashPrefix {
    pub fn as_fingerprint(&self) -> Option<&str> {
        match self {
            BashPrefix::Prefix(prefix) => Some(prefix.as_str()),
            BashPrefix::None | BashPrefix::CommandInjectionDetected => None,
        }
    }
}

/// 调用 LLM classifier 提取 Bash prefix。失败时返回 `Ok(None)`，调用方保留静态
/// effects，不让辅助 classifier 故障改变普通审批语义。
pub async fn classify_prefix(
    client: &Arc<dyn ModelClient>,
    model_id: &str,
    command_segment: &str,
    cancel: common::CancelFlag,
) -> Result<Option<BashPrefix>, ModelError> {
    let request = ModelRequest {
        model: model_id.to_string(),
        system: Some(BASH_PREFIX_CLASSIFIER_SYSTEM.to_string()),
        entries: vec![TranscriptEntry::User(UserEntry::text(format!(
            "command: {command_segment}"
        )))],
        tools: Vec::new(),
        max_tokens: 32,
        reasoning: None,
        meta: model_gateway::types::ModelCallMeta {
            tag: model_gateway::types::ModelCallTag::Classifier,
            ..Default::default()
        },
    };

    // 传 dispatcher 真实 cancel：中断时这个 prefix 分类 LLM 调用要能立即停。
    let response = client.complete(request, cancel).await?;
    Ok(parse_classifier_output(&extract_text(&response)))
}

fn extract_text(resp: &ModelResponse) -> String {
    match resp {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text.clone(),
    }
}

pub fn parse_classifier_output(raw: &str) -> Option<BashPrefix> {
    let first = raw.lines().map(str::trim).find(|line| !line.is_empty())?;

    if first.eq_ignore_ascii_case("none") {
        return Some(BashPrefix::None);
    }
    if first.eq_ignore_ascii_case("command_injection_detected") {
        return Some(BashPrefix::CommandInjectionDetected);
    }
    let Some(rest) = first.strip_prefix("prefix:") else {
        warn!(output = %first, "bash prefix classifier returned unrecognized output");
        return None;
    };
    let prefix = rest.trim();
    if prefix.is_empty()
        || prefix.contains('\n')
        || prefix == "none"
        || prefix == "command_injection_detected"
    {
        return None;
    }
    Some(BashPrefix::Prefix(prefix.to_string()))
}

// ── dispatcher 列表 ─────────────────────────────────────────────────
// 命中 dispatcher 时需要取下一个 positional arg 作为 verb 才形成 prefix；
// 裸 dispatcher（如 `git`）→ None。

const DISPATCHERS: &[&str] = &[
    "git",
    "npm",
    "yarn",
    "pnpm",
    "cargo",
    "docker",
    "docker-compose",
    "kubectl",
    "gh",
    "aws",
    "gcloud",
    "brew",
    "apt",
    "apt-get",
    "yum",
    "dnf",
    "pacman",
    "pip",
    "pip3",
    "gem",
    "bundle",
    "cmake",
    "ninja",
    "go",
    "rustc",
    "rustup",
    "node",
    "deno",
    "bun",
    "python",
    "python3",
    "java",
    "javac",
    "mvn",
    "gradle",
    "dotnet",
    "nuget",
    "terraform",
    "ansible",
    "helm",
    "ssh",
    "scp",
    "rsync",
    "curl",
    "wget",
    "tar",
    "zip",
    "unzip",
    "7z",
    "xargs",
    "sudo",
    "env",
    "strace",
    "ltrace",
    "systemctl",
    "journalctl",
];

/// 需要至少两个 positional arg 才形成 prefix 的 dispatcher。
/// 例如 `npm run lint` 需要 verb；`cargo test` 也需要。
/// 对比 `make`（无 verb 即默认 target）不算 dispatcher。
fn is_dispatcher(cmd: &str) -> bool {
    DISPATCHERS.contains(&cmd)
}

// ── modifier 剥离 ──────────────────────────────────────────────────
// 与 ParsedCommand::fingerprint 逻辑一致，但独立于 shell_parse。

const MODIFIER_PREFIXES: &[&str] = &["timeout"];

const MODIFIER_SET: &[&str] = &["command", "builtin", "noglob", "nice", "nohup"];

/// 从 argv 中剥离 modifier 和 inline env-var，返回 (env_prefix, base_argv)。
fn strip_modifiers_and_env(argv: &[String]) -> (Vec<String>, Vec<String>) {
    let mut env_prefix = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut iter = argv.iter().peekable();
    let mut skipping = true;

    while let Some(arg) = iter.next() {
        if skipping {
            // timeout 可带秒数参数，如 `timeout 30s`
            if arg.starts_with("timeout") {
                // 如果下一个 token 看起来像 timeout 的参数（不是 flag 开头、不是 modifier），跳过
                if let Some(next) = iter.peek() {
                    if !next.starts_with('-')
                        && !MODIFIER_SET.contains(&next.as_str())
                        && !MODIFIER_PREFIXES.iter().any(|p| next.starts_with(p))
                    {
                        iter.next(); // 跳过秒数参数
                    }
                }
                continue;
            }
            // nice 可带 -n <priority> 参数
            if arg == "nice" {
                // 跳过 -n flag 及其数值参数
                if let Some(next) = iter.peek() {
                    if next.starts_with('-') {
                        iter.next(); // 跳过 -n
                                     // 跳过数值参数（如果有）
                        if let Some(val) = iter.peek() {
                            if val.chars().all(|c| c.is_ascii_digit() || c == '-') {
                                iter.next();
                            }
                        }
                    }
                }
                continue;
            }
            if MODIFIER_SET.contains(&arg.as_str()) {
                continue;
            }
        }
        skipping = false;

        // inline env-var: FOO=bar
        if let Some(eq_pos) = arg.find('=') {
            if eq_pos > 0
                && arg[..eq_pos]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
            {
                env_prefix.push(arg.clone());
                continue;
            }
        }

        rest.push(arg.clone());
    }

    (env_prefix, rest)
}

// ── 命令注入检测 ──────────────────────────────────────────────────

/// 检查单个 token 是否包含命令注入模式。
fn token_has_injection(token: &str) -> bool {
    // $() — 命令替换
    // 反引号
    // <() >() — 进程替换
    token.contains("$(") || token.contains('`') || token.contains("<(") || token.contains(">(")
}

/// 检查 argv 中是否有命令注入模式。
fn argv_has_injection(argv: &[String]) -> bool {
    argv.iter().any(|t| token_has_injection(t))
}

// ── 主入口 ────────────────────────────────────────────────────────

/// 从一段命令的 argv 和 env_prefix 中提取 bash prefix。
///
/// 调用方应先用 tree-sitter 拆段，再对每段的 `ParsedCommand` 调用本函数。
pub fn extract_prefix(argv: &[String], env_prefix: &[String]) -> BashPrefix {
    // 1. 命令注入检测
    if argv_has_injection(argv) || argv_has_injection(env_prefix) {
        return BashPrefix::CommandInjectionDetected;
    }

    // 2. 剥离 modifier 和 env（调用方可能已做过，这里再做一次确保干净）
    let (stripped_env, stripped_argv) = strip_modifiers_and_env(argv);

    // 合并调用方传入的 env_prefix（去重）
    let mut all_env: Vec<String> = env_prefix.to_vec();
    for e in &stripped_env {
        if !all_env.contains(e) {
            all_env.push(e.clone());
        }
    }

    if stripped_argv.is_empty() {
        // 纯 env-var 赋值：`FOO=bar` 单独一行
        return BashPrefix::None;
    }

    let base = &stripped_argv[0];

    // 3. dispatcher 处理
    if is_dispatcher(base) {
        // 找第一个非 flag positional arg 作为 verb
        let verb = stripped_argv.iter().skip(1).find(|a| !a.starts_with('-'));

        match verb {
            Some(v) => {
                let prefix = if all_env.is_empty() {
                    format!("{base} {v}")
                } else {
                    format!("{} {base} {v}", all_env.join(" "))
                };
                BashPrefix::Prefix(prefix)
            }
            None => {
                // 裸 dispatcher（如 `git`、`npm`）→ 太宽泛
                BashPrefix::None
            }
        }
    } else {
        // 4. unitary 命令：base 即为 prefix
        let prefix = if all_env.is_empty() {
            base.clone()
        } else {
            format!("{} {base}", all_env.join(" "))
        };
        BashPrefix::Prefix(prefix)
    }
}

// ── tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn simple_unitary() {
        assert_eq!(
            extract_prefix(&argv("ls -la"), &[]),
            BashPrefix::Prefix("ls".into())
        );
    }

    #[test]
    fn dispatcher_with_verb() {
        assert_eq!(
            extract_prefix(&argv("git commit -m msg"), &[]),
            BashPrefix::Prefix("git commit".into()),
        );
    }

    #[test]
    fn dispatcher_bare_returns_none() {
        assert_eq!(extract_prefix(&argv("git"), &[]), BashPrefix::None);
    }

    #[test]
    fn dispatcher_flag_only_returns_none() {
        assert_eq!(
            extract_prefix(&argv("git --version"), &[]),
            BashPrefix::None
        );
    }

    #[test]
    fn env_prefix_preserved() {
        assert_eq!(
            extract_prefix(&argv("git commit -m msg"), &["GIT_DIR=/tmp".into()]),
            BashPrefix::Prefix("GIT_DIR=/tmp git commit".into()),
        );
    }

    #[test]
    fn env_only_returns_none() {
        assert_eq!(extract_prefix(&["FOO=bar".into()], &[]), BashPrefix::None,);
    }

    #[test]
    fn command_substitution_detected() {
        assert_eq!(
            extract_prefix(&argv("echo $(rm -rf /)"), &[]),
            BashPrefix::CommandInjectionDetected,
        );
    }

    #[test]
    fn backtick_detected() {
        assert_eq!(
            extract_prefix(&argv("echo `whoami`"), &[]),
            BashPrefix::CommandInjectionDetected,
        );
    }

    #[test]
    fn process_substitution_detected() {
        assert_eq!(
            extract_prefix(&argv("diff <(ls) <(ls)"), &[]),
            BashPrefix::CommandInjectionDetected,
        );
    }

    #[test]
    fn modifier_stripped() {
        assert_eq!(
            extract_prefix(&argv("nice -n 19 cargo test"), &[]),
            BashPrefix::Prefix("cargo test".into()),
        );
    }

    #[test]
    fn timeout_modifier_stripped() {
        assert_eq!(
            extract_prefix(&argv("timeout 30s cargo build"), &[]),
            BashPrefix::Prefix("cargo build".into()),
        );
    }

    #[test]
    fn inline_env_stripped() {
        assert_eq!(
            extract_prefix(
                &[
                    "NODE_ENV=production".into(),
                    "npm".into(),
                    "run".into(),
                    "build".into()
                ],
                &[]
            ),
            BashPrefix::Prefix("NODE_ENV=production npm run".into()),
        );
    }

    #[test]
    fn make_is_unitary() {
        // make 不在 dispatcher 列表中，make + target 直接形成 prefix
        assert_eq!(
            extract_prefix(&argv("make -j4 all"), &[]),
            BashPrefix::Prefix("make".into()),
        );
    }

    #[test]
    fn cargo_test_prefix() {
        assert_eq!(
            extract_prefix(&argv("cargo test --lib"), &[]),
            BashPrefix::Prefix("cargo test".into()),
        );
    }

    #[test]
    fn docker_run_prefix() {
        assert_eq!(
            extract_prefix(&argv("docker run -it ubuntu bash"), &[]),
            BashPrefix::Prefix("docker run".into()),
        );
    }

    #[test]
    fn injection_in_env_detected() {
        assert_eq!(
            extract_prefix(&[], &["EVIL=$(rm -rf /)".into()]),
            BashPrefix::CommandInjectionDetected,
        );
    }

    #[test]
    fn npm_run_no_args() {
        // `npm run` 有 verb "run"，形成 prefix
        assert_eq!(
            extract_prefix(&argv("npm run"), &[]),
            BashPrefix::Prefix("npm run".into())
        );
    }

    #[test]
    fn parses_classifier_prefix_output() {
        assert_eq!(
            parse_classifier_output("prefix: git commit\n"),
            Some(BashPrefix::Prefix("git commit".into()))
        );
    }

    #[test]
    fn parses_classifier_none_output() {
        assert_eq!(parse_classifier_output("none"), Some(BashPrefix::None));
    }

    #[test]
    fn parses_classifier_injection_output() {
        assert_eq!(
            parse_classifier_output("command_injection_detected"),
            Some(BashPrefix::CommandInjectionDetected)
        );
    }

    #[test]
    fn rejects_classifier_preamble() {
        assert_eq!(parse_classifier_output("The prefix is git status"), None);
    }
}
