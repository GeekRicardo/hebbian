//! 内置只读命令白名单。出现在这里的命令会被 [`crate::effects::analyze_effects`]
//! 判定为 `EffectClass::ReadOnly`，**直接放行不审批**。
//!
//! 收录原则（保守）：
//! - 命令本身**只读**：不会改文件系统、不发出网络请求、不修改进程状态
//! - 即使加任意 flag 也几乎不会变成有副作用（避免 `rm`、`curl` 这类）
//! - 高频命令优先，让用户少看几次审批弹窗
//!
//! 对 `git` / `npm` / `cargo` 这类多子命令工具，区分到具体子命令粒度（`git status` 安全，
//! `git push` 不安全）。
//!
//! 红线：**宁可漏报（多审批）也不要误放（少审批）**。新增条目前先想清楚是否有任何 flag
//! 组合能让它变写操作或网络操作。

use super::shell_parse::ParsedCommand;

/// 顶级只读命令：argv[0] 在这里、且没有危险结构 → 直接安全。
const SAFE_ROOTS: &[&str] = &[
    // 文件系统只读
    "ls",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "file",
    "stat",
    "tree",
    "du",
    "df",
    "dirname",
    "basename",
    "realpath",
    "readlink",
    "lsblk",
    // 文本处理（纯过滤，无副作用）
    "echo",
    "printf",
    "true",
    "false",
    "yes",
    "seq",
    "sort",
    "uniq",
    "cut",
    "tr",
    "nl",
    "rev",
    "tac",
    "column",
    "fold",
    "expand",
    "unexpand",
    "fmt",
    "comm",
    "diff",
    "cmp",
    "jq", // 默认输出 stdout，无原地写（jq 没有 -i）
    "tee", // tee 严格说会写文件，但它没参数时只 stdout——简单起见排除
    // grep 系列
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ack",
    "ag",
    // 查找（`find` 加 -delete / -exec 会变危险，单独处理）
    // find 不在这里——交给子命令检查
    // 信息查询
    "which",
    "whereis",
    "type",
    "command",
    "alias",
    "whoami",
    "id",
    "uname",
    "hostname",
    "uptime",
    "date",
    "cal",
    "env",
    "printenv",
    "groups",
    "users",
    "locale",
    "tty",
    "arch",
    "nproc",
    "getconf",
    "lscpu",
    "free",
    "vmstat",
    // 网络只读查询（只查状态 / DNS，不发起会改远端的请求）
    "dig",
    "host",
    "nslookup",
    "lsof",
    "netstat",
    "ss",
    // 进程查询
    "ps",
    "pgrep",
    "jobs",
    // 日志只读查询
    "journalctl",
    "dmesg",
    // 帮助 / 文档
    "man",
    "help",
    "info",
    "tldr",
    // 编程语言版本查询
    "node",
    "python",
    "python3",
    "ruby",
    "perl",
    "go",
    "rustc",
    "java",
    "javac",
    // ↑ 注意：这些只在带 `--version` / `-V` 时安全，单独命令也无害（启动 REPL 会卡住等审批超时即可）
    // hash / 校验
    "md5",
    "md5sum",
    "shasum",
    "sha256sum",
    "sha1sum",
    "cksum",
    // 编码
    "base64",
    "xxd",
    "od",
];

/// `(root, sub_arg0)` 形式的安全子命令，例如 `("git", "status")`。
/// 在 root 不属于 [`SAFE_ROOTS`] 时再查一次这个表。
const SAFE_SUBCOMMANDS: &[(&str, &str)] = &[
    // git 只读子命令
    ("git", "status"),
    ("git", "diff"),
    ("git", "log"),
    ("git", "show"),
    ("git", "branch"),
    ("git", "tag"),
    ("git", "remote"),
    ("git", "config"), // `git config -l/--get` 是只读；`git config key value` 写——靠 dangerous=true 不影响，但带写 flag 仍会放行 ⚠️
    // ↑ TODO: 如果用户对此不满，挪到 conditional 列表
    ("git", "rev-parse"),
    ("git", "describe"),
    ("git", "blame"),
    ("git", "reflog"),
    ("git", "stash"), // `git stash list/show` 安全，`git stash push/pop` 写——同上
    ("git", "ls-files"),
    ("git", "ls-tree"),
    ("git", "shortlog"),
    ("git", "cat-file"),
    ("git", "show-ref"),
    ("git", "rev-list"),
    ("git", "for-each-ref"),
    ("git", "symbolic-ref"),
    ("git", "merge-base"),
    ("git", "name-rev"),
    ("git", "whatchanged"),
    ("git", "count-objects"),
    ("git", "verify-commit"),
    // cargo 只读子命令
    ("cargo", "tree"),
    ("cargo", "metadata"),
    ("cargo", "search"), // 联网，但不写本地
    ("cargo", "version"),
    ("cargo", "--version"),
    ("cargo", "help"),
    // npm / pnpm / yarn 查询
    ("npm", "ls"),
    ("npm", "list"),
    ("npm", "view"),
    ("npm", "outdated"),
    ("npm", "config"),
    ("pnpm", "ls"),
    ("pnpm", "list"),
    ("pnpm", "why"),
    ("yarn", "list"),
    ("yarn", "why"),
    // docker 查询
    ("docker", "ps"),
    ("docker", "images"),
    ("docker", "logs"),
    ("docker", "inspect"),
    ("docker", "version"),
    ("docker", "info"),
    ("docker", "stats"),
    ("docker", "top"),
    ("docker", "port"),
    ("docker", "history"),
    ("docker", "diff"),
    // kubectl 查询
    ("kubectl", "get"),
    ("kubectl", "describe"),
    ("kubectl", "logs"),
    ("kubectl", "config"),
    ("kubectl", "version"),
    ("kubectl", "top"),
    ("kubectl", "explain"),
    ("kubectl", "api-resources"),
    ("kubectl", "api-versions"),
    ("kubectl", "cluster-info"),
    // go 查询
    ("go", "version"),
    ("go", "env"),
    ("go", "list"),
    ("go", "doc"),
    // rustup
    ("rustup", "show"),
    ("rustup", "which"),
    // brew 查询
    ("brew", "list"),
    ("brew", "info"),
    ("brew", "search"),
    ("brew", "config"),
    ("brew", "doctor"),
    // pip 查询
    ("pip", "list"),
    ("pip", "show"),
    ("pip", "freeze"),
    ("pip", "config"),
    ("pip3", "list"),
    ("pip3", "show"),
    ("pip3", "freeze"),
    // helm 查询
    ("helm", "list"),
    ("helm", "status"),
    ("helm", "get"),
    ("helm", "show"),
    ("helm", "history"),
    ("helm", "version"),
    ("helm", "env"),
    // systemctl 查询
    ("systemctl", "status"),
    ("systemctl", "list-units"),
    ("systemctl", "list-unit-files"),
    ("systemctl", "is-active"),
    ("systemctl", "is-enabled"),
    ("systemctl", "show"),
    ("systemctl", "cat"),
    // terraform 查询（plan / validate 只算只读分析，不改 state）
    ("terraform", "plan"),
    ("terraform", "show"),
    ("terraform", "validate"),
    ("terraform", "output"),
    ("terraform", "version"),
    ("terraform", "providers"),
];

/// 「不可记忆」命令根：不可逆 / 高破坏的命令，**每次出现都弹审批且禁止记忆**
/// （架构 §4.4.2.3）。它们本身不在 [`SAFE_ROOTS`]，所以一定落到"会写"侧；
/// 这张表只是额外标记"哪怕用户点了 AllowAndRemember 也不落盘"——下次同命令仍审批。
///
/// 与 [`crate::tools::shell_parse::DangerousKind`] 的区别：危险复合模式是"结构危险"
/// （如 `cd && git` / `rm -rf /`），按整行命中；本名单是"命令本身不可逆"，按 root 命中，
/// 即使 `rm -rf ./build` 这种工作区内的普通删除也每次确认。
const NEVER_REMEMBER_ROOTS: &[&str] = &[
    "rm", "rmdir", "dd", "shred", "truncate", "fdisk", "gdisk", "parted", "wipefs",
];

/// 判断一段命令是否"不可记忆"（每次审批、禁止落盘记忆）。
/// 命中 [`NEVER_REMEMBER_ROOTS`] 或 `mkfs` 系列（`mkfs` / `mkfs.ext4` / ...）。
pub fn is_never_remember(cmd: &ParsedCommand) -> bool {
    let root = cmd.root.as_str();
    NEVER_REMEMBER_ROOTS.contains(&root) || root.starts_with("mkfs")
}

/// 判断单条命令是否安全（只读、无副作用）。
pub fn is_safe(cmd: &ParsedCommand) -> bool {
    let root = cmd.root.as_str();

    // 0) 段内有任何写文件目标（重定向 / tee / sed -i / python -c "open(...,'w')" / ...）
    //    → 一律不安全。哪怕 root 是 echo / cat 也不行。
    if !cmd.write_targets.is_empty() || cmd.has_heredoc {
        return false;
    }

    // 1) 顶级只读命令
    if SAFE_ROOTS.contains(&root) {
        return true;
    }

    // 2) 根 + 子命令组合
    let positional = cmd.positional();
    if let Some(first_pos) = positional.first() {
        if SAFE_SUBCOMMANDS
            .iter()
            .any(|(r, s)| *r == root && *s == *first_pos)
        {
            return true;
        }
    }

    // 3) `find PATH ...`：排除任何 `-delete` / `-exec` / `-fprint` 等副作用 flag
    if root == "find" {
        // find 默认行为是只读列出。若 argv 中含 -delete / -exec / -execdir / -ok / -okdir / -fprint
        // / -fprintf / -fls 等就拒绝。
        let dangerous_find_flags: &[&str] = &[
            "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
        ];
        if cmd
            .argv
            .iter()
            .any(|a| dangerous_find_flags.iter().any(|f| a == *f))
        {
            return false;
        }
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::super::shell_parse::parse;
    use super::*;

    fn first(line: &str) -> ParsedCommand {
        parse(line).unwrap().commands.into_iter().next().unwrap()
    }

    #[test]
    fn ls_is_safe() {
        assert!(is_safe(&first("ls -la")));
    }

    #[test]
    fn rm_is_unsafe() {
        assert!(!is_safe(&first("rm -rf /tmp/x")));
    }

    #[test]
    fn git_status_is_safe() {
        assert!(is_safe(&first("git status -uno")));
        assert!(is_safe(&first("git diff HEAD~1")));
        assert!(is_safe(&first("git log --oneline")));
    }

    #[test]
    fn git_push_is_unsafe() {
        assert!(!is_safe(&first("git push origin main")));
        assert!(!is_safe(&first("git commit -am foo")));
        assert!(!is_safe(&first("git checkout -b new")));
    }

    #[test]
    fn find_default_safe() {
        assert!(is_safe(&first("find . -name *.rs")));
    }

    #[test]
    fn find_with_delete_unsafe() {
        assert!(!is_safe(&first("find . -name *.tmp -delete")));
    }

    #[test]
    fn find_with_exec_unsafe() {
        // `-exec` flag 是不安全的，由 is_safe 判定。
        // tree-sitter 把 `{}` 正确解析为 concatenation，不再产生 false positive dangerous。
        let parsed = parse("find . -type f -exec echo {} +").unwrap();
        assert!(!parsed.dangerous);

        let direct = first("find . -type f -exec foo");
        assert!(!is_safe(&direct));
    }

    #[test]
    fn unknown_command_unsafe() {
        assert!(!is_safe(&first("custom_script.sh")));
    }

    #[test]
    fn cat_is_safe() {
        assert!(is_safe(&first("cat README.md")));
    }

    #[test]
    fn new_readonly_roots_are_safe() {
        for cmd in ["jq . a.json", "dirname /a/b", "dig example.com", "diff a b", "comm a b"] {
            assert!(is_safe(&first(cmd)), "{cmd} 应判只读");
        }
    }

    #[test]
    fn new_readonly_subcommands_are_safe() {
        for cmd in [
            "kubectl top pods",
            "helm list",
            "git cat-file -p HEAD",
            "docker stats",
            "terraform plan",
        ] {
            assert!(is_safe(&first(cmd)), "{cmd} 应判只读");
        }
    }

    #[test]
    fn never_remember_matches_destructive_roots() {
        for cmd in ["rm -rf x", "dd if=/dev/zero of=x", "mkfs.ext4 /dev/sda1", "shred x"] {
            assert!(is_never_remember(&first(cmd)), "{cmd} 应判不可记忆");
        }
    }

    #[test]
    fn never_remember_excludes_ordinary_writes() {
        for cmd in ["cp a b", "touch x", "mkdir d", "mv a b"] {
            assert!(!is_never_remember(&first(cmd)), "{cmd} 不应判不可记忆");
        }
    }
}
