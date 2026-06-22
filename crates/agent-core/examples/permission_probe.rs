//! 权限审批探针：直接驱动真实审批链路，用真实的 `~/.hebbian/permissions.json` +
//! 默认 [`PermissionPolicy`]，把一条条工具调用喂进去，看到底「哪些自动放行 / 哪些要
//! 审批 / 哪些被拒」，并判断是否符合预期。探针**只做权限判定，不执行任何命令、不碰任何文件**。
//!
//! 复刻 dispatch 的两道闸：
//!   - **目录/路径越界审批**：`workspace.allows(path)` 不通过、且无 path 规则覆盖 → 越界审批
//!   - **工具审批**：`HitlGate::check`（Bash/Edit 默认 always_ask；段级 / 路径级规则可放行）
//!
//! 三种输入源：
//!   - 批量基线：一组预置调用 + 预期，跑一遍打表对照（默认）
//!   - 交互 REPL：手输 Bash 命令逐条判定；要审批的就地交互
//!   - 既有 session：把某个 session 的工具调用从上到下抽出来逐条过审批
//!
//! 交互审批里可以像桌面端那样：把解析出的命令前缀 / 目录列成复选框逐项勾选，再选
//! 作用域（once/session/project/global）写入审批白名单。session 只影响本次进程（内存），
//! project/global 会**真的写** `~/.hebbian/permissions.json`，写盘前会显式打印路径。
//!
//! 用法：
//! ```
//! cargo run -p agent-core --example permission_probe                  # 批量基线，tty 下接着进 REPL
//! cargo run -p agent-core --example permission_probe -- batch         # 仅批量基线
//! cargo run -p agent-core --example permission_probe -- repl          # 仅交互 REPL
//! cargo run -p agent-core --example permission_probe -- session <ID>  # 抽 session 的工具调用逐条审批
//! # 选项：--workdir <dir>（路径越界 / 项目级规则按此 workdir 匹配；session 默认用 session.workdir）
//! ```

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use agent_core::definition::PermissionPolicy;
use agent_core::effects::{analyze_effects, EffectClass, Effects};
use agent_core::permissions::{PermissionStore, RuleEffect};
use agent_core::storage::{default_data_dir, sessions};
use agent_core::tools::hitl::{HitlGate, PermissionDecision};
use agent_core::workspace::Workspace;
use protocol::PermissionScope;
use serde_json::{json, Value};

/// 探针进程内固定用这个 session_id 承载 session 作用域记忆（内存态，进程结束即丢）。
const PROBE_SESSION_ID: &str = "permission-probe";

/// session 模式里只抽这些「审批相关」的工具调用（Read/Grep 多为自动放行，噪音大）。
const SESSION_TOOLS: &[&str] = &["Bash", "Edit", "Read", "Grep"];

// ── 配色 ──────────────────────────────────────────────────────────────────────

fn color_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal())
}

fn paint(s: &str, code: &str) -> String {
    if color_on() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn green(s: &str) -> String {
    paint(s, "32")
}
fn yellow(s: &str) -> String {
    paint(s, "33")
}
fn red(s: &str) -> String {
    paint(s, "31")
}
fn cyan(s: &str) -> String {
    paint(s, "36")
}
fn dim(s: &str) -> String {
    paint(s, "2")
}
fn bold(s: &str) -> String {
    paint(s, "1")
}

// ── 预期 / 结果三态 ─────────────────────────────────────────────────────────────

/// 用户「直觉上」认为这条调用应当怎样处理。actual 与它不符即 ✗，正是要排查的点。
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Auto,
    Ask,
    #[allow(dead_code)]
    Deny,
}

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Auto,
    Ask,
    Deny,
}

impl Outcome {
    fn matches(self, expect: Expect) -> bool {
        matches!(
            (self, expect),
            (Outcome::Auto, Expect::Auto)
                | (Outcome::Ask, Expect::Ask)
                | (Outcome::Deny, Expect::Deny)
        )
    }

    fn colored(self) -> String {
        match self {
            Outcome::Auto => green("自动放行"),
            Outcome::Ask => yellow("需审批"),
            Outcome::Deny => red("拒绝"),
        }
    }
}

fn expect_label(e: Expect) -> &'static str {
    match e {
        Expect::Auto => "自动放行",
        Expect::Ask => "需审批",
        Expect::Deny => "拒绝",
    }
}

// ── 一次判定的结果 ──────────────────────────────────────────────────────────────

/// 把 dispatch 的两道闸合并成一次判定结果。
struct Judgement {
    effects: Effects,
    /// 越界（workspace 不允许且无 path 规则覆盖）的路径。
    out_of_scope: Vec<PathBuf>,
    decision: PermissionDecision,
}

impl Judgement {
    fn outcome(&self) -> Outcome {
        match &self.decision {
            PermissionDecision::Denied { .. } => Outcome::Deny,
            PermissionDecision::NeedsApproval { .. } => Outcome::Ask,
            PermissionDecision::Approved => {
                if self.out_of_scope.is_empty() {
                    Outcome::Auto
                } else {
                    // 工具自身放行，但路径越界 → 仍需路径审批
                    Outcome::Ask
                }
            }
        }
    }
}

/// 一条调用 = 工具名 + 输入（已是工具的 input JSON）。
struct Candidate {
    pattern: String,
    label: String,
    preselected: bool,
}

/// 复合命令里单段的白名单状态（决定审批弹窗里怎么展示这一段）。
enum SegStatus {
    /// 只读段：免审批、免记忆。
    Readonly,
    /// 已在白名单（命中某条 allow 规则）：本次无需再处理。
    Whitelisted(String),
    /// 不可记忆（rm/dd/…）：红色、不可勾选、每次必须确认。
    Unmemorable,
    /// 会写且尚未进白名单：本次要决定是否加入。
    NeedsApproval,
}

impl SegStatus {
    fn badge(&self) -> String {
        match self {
            SegStatus::Readonly => dim("只读·免审批"),
            SegStatus::Whitelisted(pat) => green(&format!("✓ 已在白名单 «{pat}»")),
            SegStatus::Unmemorable => red("⛔ 危险·不可记住（每次必审）"),
            SegStatus::NeedsApproval => yellow("● 待审批"),
        }
    }
}

struct Probe {
    gate: HitlGate,
    store: Arc<PermissionStore>,
    workspace: Arc<Workspace>,
    workdir: Option<PathBuf>,
    data_dir: PathBuf,
    /// `<data_dir>/sessions/<sid>`：agent 读写自己产物（tool_results/plans…）免越界审批，
    /// 复刻 dispatch 的 session_artifact 旁路。仅 history 模式按 session 设置。
    session_artifact_root: Option<PathBuf>,
}

impl Probe {
    fn new(workdir: Option<PathBuf>, allowed_paths: Vec<PathBuf>) -> Self {
        let data_dir = default_data_dir();
        let store = Arc::new(
            PermissionStore::open(&data_dir).expect("打开 PermissionStore 失败（~/.hebbian）"),
        );
        let workspace = match &workdir {
            Some(wd) => Workspace::new(wd.clone(), allowed_paths),
            None => Workspace::home_default(),
        };
        let gate = HitlGate::new(PermissionPolicy::default()).with_store(
            store.clone(),
            PROBE_SESSION_ID,
            workdir.clone(),
        );
        Self {
            gate,
            store,
            workspace,
            workdir,
            data_dir,
            session_artifact_root: None,
        }
    }

    fn class_label(class: EffectClass) -> &'static str {
        match class {
            EffectClass::ReadOnly => "只读",
            EffectClass::Mutating => "改文件",
            EffectClass::Destructive => "执行命令",
            EffectClass::Network => "网络",
            EffectClass::NeedsHumanInput => "求助",
        }
    }

    /// 跑一次判定：算 effects → 路径越界检查 → 工具审批。
    fn judge(&self, tool: &str, input: &Value) -> Judgement {
        let effects = analyze_effects(tool, input);
        let out_of_scope = self.out_of_scope_paths(&effects);
        let decision = self.gate.check(tool, &effects);
        Judgement {
            effects,
            out_of_scope,
            decision,
        }
    }

    /// 复刻 dispatch 的路径越界检查：workspace 允许 → 通过；否则查 PermissionStore 的
    /// path 规则 / paths 白名单；都不命中即越界。
    fn out_of_scope_paths(&self, effects: &Effects) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for p in &effects.paths {
            if self.workspace.allows(p) {
                continue;
            }
            if let Some(root) = &self.session_artifact_root {
                if p.starts_with(root) {
                    continue;
                }
            }
            let allowed_by_rule = self.store.allows_path(
                Some(PROBE_SESSION_ID),
                self.workdir.as_deref(),
                &p.to_string_lossy(),
            );
            if !allowed_by_rule {
                out.push(p.clone());
            }
        }
        out
    }

    /// 查 PermissionStore 命中了哪一层 / 哪条规则（仅诊断，解释「为什么这样判」）。
    fn store_note(&self, effects: &Effects) -> String {
        let hit = if !effects.segments.is_empty() {
            self.store.find_for_segments_diagnostic(
                Some(PROBE_SESSION_ID),
                self.workdir.as_deref(),
                "Bash",
                &effects.segments,
            )
        } else {
            None
        };
        match hit {
            Some(m) => format!("规则命中 [{}] «{}»", scope_label(m.scope), m.pattern),
            None => "无规则命中（按默认）".to_string(),
        }
    }

    fn note_for(&self, j: &Judgement) -> String {
        if let PermissionDecision::Denied { reason } = &j.decision {
            return red(reason);
        }
        if !j.out_of_scope.is_empty() {
            let dirs: Vec<String> = j
                .out_of_scope
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            return yellow(&format!("越界路径: {}", dirs.join(", ")));
        }
        dim(&self.store_note(&j.effects))
    }

    // ── 批量基线 ───────────────────────────────────────────────────────────────

    fn run_baseline(&self) -> usize {
        println!(
            "{}",
            bold("== 批量基线（真实 ~/.hebbian/permissions.json + 默认 policy）==")
        );
        println!("    workdir = {}", self.workspace.workdir().display());
        let cases = self.baseline_cases();
        println!(
            "{:<3} {:<46} {:<10} {:<8} {:<8} {}",
            "", "调用", "实际", "预期", "分类", "说明"
        );
        let mut mismatch = 0;
        for (label, tool, input, expect) in &cases {
            let j = self.judge(tool, input);
            let outcome = j.outcome();
            let ok = outcome.matches(*expect);
            if !ok {
                mismatch += 1;
            }
            println!(
                "{:<3} {:<46} {:<19} {:<8} {:<8} {}",
                if ok { green("✓") } else { red("✗") },
                truncate(label, 46),
                outcome.colored(),
                expect_label(*expect),
                Self::class_label(j.effects.class),
                self.note_for(&j),
            );
        }
        let summary = format!("\n小结：{} 条，{} 条与预期不符。", cases.len(), mismatch);
        println!(
            "{}",
            if mismatch == 0 {
                green(&summary)
            } else {
                yellow(&summary)
            }
        );
        mismatch
    }

    /// 预置基线用例。Bash + 路径工具（含目录越界）混排。
    /// 路径用例按当前 workdir 动态构造：workdir 内 vs 系统目录（越界）。
    fn baseline_cases(&self) -> Vec<(String, String, Value, Expect)> {
        let wd = self.workspace.workdir().to_path_buf();
        let in_file = wd.join("README.md");
        let in_src = wd.join("src/main.rs");
        let bash = |c: &str| json!({ "command": c });
        let mut v: Vec<(String, String, Value, Expect)> = vec![
            // ── Bash 只读 → 自动放行 ──
            ("ls -la".into(), "Bash".into(), bash("ls -la"), Expect::Auto),
            (
                "cat README.md".into(),
                "Bash".into(),
                bash("cat README.md"),
                Expect::Auto,
            ),
            (
                "grep -R foo src".into(),
                "Bash".into(),
                bash("grep -R foo src"),
                Expect::Auto,
            ),
            (
                "git status -uno".into(),
                "Bash".into(),
                bash("git status -uno"),
                Expect::Auto,
            ),
            // ── Bash 会写 → 审批 ──
            (
                "touch newfile.txt".into(),
                "Bash".into(),
                bash("touch newfile.txt"),
                Expect::Ask,
            ),
            (
                "echo data > out.txt".into(),
                "Bash".into(),
                bash("echo data > out.txt"),
                Expect::Ask,
            ),
            (
                "git push origin main".into(),
                "Bash".into(),
                bash("git push origin main"),
                Expect::Ask,
            ),
            (
                "npm install".into(),
                "Bash".into(),
                bash("npm install"),
                Expect::Ask,
            ),
            // 脚本解释器：白名单判只读 → 实际自动放行，但用户多半希望审批
            (
                "python3 script.py".into(),
                "Bash".into(),
                bash("python3 script.py"),
                Expect::Ask,
            ),
            // 不可记忆 / 危险复合
            (
                "rm -rf build".into(),
                "Bash".into(),
                bash("rm -rf build"),
                Expect::Ask,
            ),
            (
                "cd /tmp && git commit -am x".into(),
                "Bash".into(),
                bash("cd /tmp && git commit -am x"),
                Expect::Ask,
            ),
        ];
        // ── 路径 / 目录审批 ──
        v.push((
            format!("Read 工作区内 {}", in_file.display()),
            "Read".into(),
            json!({ "file_path": in_file }),
            Expect::Auto, // Read 自动放行 + 在边界内
        ));
        v.push((
            "Read /etc/hosts（越界）".into(),
            "Read".into(),
            json!({ "file_path": "/etc/hosts" }),
            Expect::Ask, // 工具放行但路径越界
        ));
        v.push((
            format!("Edit 工作区内 {}", in_src.display()),
            "Edit".into(),
            json!({ "file_path": in_src }),
            Expect::Ask, // Edit always_ask
        ));
        v.push((
            "Edit /etc/passwd（越界）".into(),
            "Edit".into(),
            json!({ "file_path": "/etc/passwd" }),
            Expect::Ask, // 越界 + always_ask
        ));
        v.push((
            "Grep /usr/lib（越界目录）".into(),
            "Grep".into(),
            json!({ "pattern": "foo", "path": "/usr/lib" }),
            Expect::Ask, // 工具只读放行，但目录越界
        ));
        v
    }

    // ── 交互判定 ───────────────────────────────────────────────────────────────

    /// 判定一条调用；若需审批，进入交互审批并落规则；最后复判回显。
    fn judge_interactive(&self, tool: &str, input: &Value, title: &str, stdin: &mut impl BufRead) {
        let j = self.judge(tool, input);
        println!("\n{} {}", cyan(&format!("[{tool}]")), bold(title));
        println!("    分类: {}", Self::class_label(j.effects.class));
        self.print_segments(&j.effects);
        if !j.out_of_scope.is_empty() {
            for p in &j.out_of_scope {
                println!("    {} {}", yellow("越界路径:"), p.display());
            }
        }

        match j.outcome() {
            Outcome::Auto => {
                println!("  → {} {}", green("✓ 自动放行"), self.note_for(&j));
                self.release(&j.decision);
            }
            Outcome::Deny => {
                println!("  → {} {}", red("✗ 拒绝"), self.note_for(&j));
                self.release(&j.decision);
            }
            Outcome::Ask => {
                println!("  → {}", yellow("⚠ 需要审批"));
                self.interactive_approval(tool, &j, stdin);
                let again = self.judge(tool, input);
                println!("    复判：{}", again.outcome().colored());
            }
        }
    }

    /// 释放 gate 里挂着的 pending（避免泄漏）。
    fn release(&self, decision: &PermissionDecision) {
        if let PermissionDecision::NeedsApproval { request_id, .. } = decision {
            self.gate
                .resolve(request_id, protocol::ApprovalDecision::AllowOnce);
        }
    }

    /// 逐段的白名单状态。**每次调用都实时查 store**（global/project 按 mtime 刷新、
    /// session 内存实时），所以上一次审批刚写的规则这次立刻可见。
    fn segment_statuses(&self, effects: &Effects) -> Vec<(String, SegStatus)> {
        effects
            .segments
            .iter()
            .map(|seg| {
                let status = if seg.is_readonly {
                    SegStatus::Readonly
                } else if seg.unmemorable {
                    SegStatus::Unmemorable
                } else {
                    match self
                        .store
                        .find_for_segments_diagnostic(
                            Some(PROBE_SESSION_ID),
                            self.workdir.as_deref(),
                            "Bash",
                            std::slice::from_ref(seg),
                        )
                        .map(|m| (m.effect, m.pattern))
                    {
                        Some((RuleEffect::Allow, pat)) => SegStatus::Whitelisted(pat),
                        _ => SegStatus::NeedsApproval,
                    }
                };
                (seg.fingerprint.clone(), status)
            })
            .collect()
    }

    fn print_segments(&self, effects: &Effects) {
        if effects.segments.is_empty() {
            return;
        }
        for (i, (fp, status)) in self.segment_statuses(effects).into_iter().enumerate() {
            println!("    [{}] {:<30} {}", i + 1, fp, status.badge());
        }
        if !effects.dangerous_kinds.is_empty() {
            println!(
                "    {} {}",
                red("⚠ 危险复合模式（每次必审、任何作用域都不可记）:"),
                effects.dangerous_kinds.join(", ")
            );
        }
    }

    /// 交互式审批一张弹窗：勾选要加入白名单的条目 + 选作用域，写入规则。
    fn interactive_approval(&self, tool: &str, j: &Judgement, stdin: &mut impl BufRead) {
        println!(
            "    操作 [{}=允许一次 / {}=加入白名单 / {}=拒绝]:",
            bold("回车"),
            bold("w"),
            bold("d")
        );
        print!("    > ");
        io::stdout().flush().ok();
        match read_line(stdin).trim() {
            "d" => {
                if let PermissionDecision::NeedsApproval { request_id, .. } = &j.decision {
                    self.gate
                        .resolve(request_id, protocol::ApprovalDecision::Deny);
                }
                println!("    {}", red("已拒绝"));
                return;
            }
            "w" => {} // 继续走白名单流程
            _ => {
                self.release(&j.decision);
                println!("    {}", dim("仅本次允许（未写规则）"));
                return;
            }
        }

        // 先把不可勾选的段亮出来：已白名单段（无需处理）、不可记忆段（红色禁选）。
        for (fp, status) in self.segment_statuses(&j.effects) {
            match status {
                SegStatus::Whitelisted(pat) => {
                    println!(
                        "    {} {:<30} {}",
                        green("✓"),
                        fp,
                        dim(&format!("已在白名单 «{pat}»，跳过"))
                    )
                }
                SegStatus::Unmemorable => {
                    println!(
                        "    {} {:<30} {}",
                        red("⛔"),
                        fp,
                        red("危险·不可记住，每次必审，不能勾选")
                    )
                }
                _ => {}
            }
        }

        let candidates = self.whitelist_candidates(tool, j);
        if candidates.is_empty() {
            self.release(&j.decision);
            println!(
                "    {}",
                dim("没有可加入白名单的条目（只读 / 已白名单 / 不可记忆）→ 仅本次允许")
            );
            return;
        }

        let chosen = toggle_select(&candidates, stdin);
        if chosen.is_empty() {
            self.release(&j.decision);
            println!("    {}", dim("未勾选任何条目 → 仅本次允许"));
            return;
        }

        let Some(scope) = self.prompt_scope(stdin) else {
            self.release(&j.decision);
            println!("    {}", dim("未选作用域 → 仅本次允许"));
            return;
        };

        if matches!(scope, PermissionScope::Project | PermissionScope::Global) {
            println!(
                "    {} {}",
                yellow("⚠ 将写入"),
                self.data_dir.join("permissions.json").display()
            );
        }
        for &i in &chosen {
            let pat = &candidates[i].pattern;
            match self.store.add(
                scope,
                Some(PROBE_SESSION_ID),
                self.workdir.as_deref(),
                RuleEffect::Allow,
                pat.clone(),
            ) {
                Ok(()) => println!(
                    "    {} [{}] «{}»",
                    green("+ 已加入白名单"),
                    scope_label(scope),
                    pat
                ),
                Err(e) => println!("    {} {} ({e})", red("写入失败"), pat),
            }
        }
        // 规则已覆盖，释放本次 pending。
        self.release(&j.decision);
    }

    /// 列出可加入白名单的条目：Bash → 会写段命令前缀；路径工具 → 目录 / 文件前缀。
    fn whitelist_candidates(&self, tool: &str, j: &Judgement) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        if tool == "Bash" || tool == "PowerShell" {
            for fp in self
                .gate
                .unapproved_memorable_writable_segments(tool, &j.effects)
            {
                let root = fp.split_whitespace().next().unwrap_or(&fp).to_string();
                let pattern = format!("{tool}({root})");
                if !out.iter().any(|c| c.pattern == pattern) {
                    out.push(Candidate {
                        label: format!("命令前缀 {root}"),
                        pattern,
                        preselected: true,
                    });
                }
            }
        } else {
            // 路径工具：收集 effects.paths ∪ 越界路径，给出「目录」「仅文件」两档粒度。
            let mut targets: Vec<PathBuf> = j.effects.paths.clone();
            for p in &j.out_of_scope {
                if !targets.contains(p) {
                    targets.push(p.clone());
                }
            }
            for p in &targets {
                if let Some(parent) = p.parent() {
                    let dir = format!("{}/", parent.display());
                    let pattern = format!("{tool}({dir})");
                    if !out.iter().any(|c| c.pattern == pattern) {
                        out.push(Candidate {
                            label: format!("目录 {dir}"),
                            pattern,
                            preselected: true,
                        });
                    }
                }
                let pattern = format!("{tool}({})", p.display());
                if !out.iter().any(|c| c.pattern == pattern) {
                    out.push(Candidate {
                        label: format!("仅文件 {}", p.display()),
                        pattern,
                        preselected: false,
                    });
                }
            }
        }
        out
    }

    /// 这次调用「记住」时会落的白名单 key（预选项的 pattern）。
    /// history 分析用它模拟「记住」覆盖：keys 全在已记集合里即本次免审批。
    fn call_keys(&self, tool: &str, j: &Judgement) -> Vec<String> {
        self.whitelist_candidates(tool, j)
            .into_iter()
            .filter(|c| c.preselected)
            .map(|c| c.pattern)
            .collect()
    }

    fn prompt_scope(&self, stdin: &mut impl BufRead) -> Option<PermissionScope> {
        println!(
            "    作用域 [{}=本会话 / {}=本项目 / {}=全局]:",
            bold("s"),
            bold("p"),
            bold("g")
        );
        print!("    > ");
        io::stdout().flush().ok();
        match read_line(stdin).trim() {
            "s" => Some(PermissionScope::Session),
            "p" => {
                if self.workdir.is_none() {
                    println!(
                        "    {}",
                        red("当前没有 workdir，项目作用域不可用 → 改用会话")
                    );
                    Some(PermissionScope::Session)
                } else {
                    Some(PermissionScope::Project)
                }
            }
            "g" => Some(PermissionScope::Global),
            _ => None,
        }
    }

    // ── session 抽取 ────────────────────────────────────────────────────────────

    fn run_session(&self, session_id: &str, stdin: &mut impl BufRead) {
        let session = match sessions::load(&self.data_dir, session_id) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}", red(&format!("加载 session «{session_id}» 失败：{e}")));
                return;
            }
        };
        let calls: Vec<(String, Value)> = session
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .filter(|tc| SESSION_TOOLS.contains(&tc.name.as_str()))
            .map(|tc| (tc.name.clone(), tc.input.clone()))
            .collect();
        println!(
            "{}",
            bold(&format!(
                "== session «{}» 抽出 {} 条工具调用，逐条审批 ==",
                session_id,
                calls.len()
            ))
        );
        println!("   workdir = {}", self.workspace.workdir().display());
        for (tool, input) in calls {
            let title = call_title(&tool, &input);
            self.judge_interactive(&tool, &input, &title, stdin);
        }
    }

    fn run_repl(&self, stdin: &mut impl BufRead) {
        println!(
            "\n{}",
            bold("== 交互模式：输入 Bash 命令回车判定，空行 / Ctrl-D 退出 ==")
        );
        loop {
            print!("\n{} ", cyan("bash>"));
            io::stdout().flush().ok();
            let line = match read_line_opt(stdin) {
                Some(l) => l,
                None => break,
            };
            if line.trim().is_empty() {
                break;
            }
            self.judge_interactive(
                "Bash",
                &json!({ "command": line.trim() }),
                line.trim(),
                stdin,
            );
        }
        println!("bye.");
    }
}

/// session 调用的一行标题：Bash 显示命令，路径工具显示路径。
fn call_title(tool: &str, input: &Value) -> String {
    if let Some(c) = input.get("command").and_then(Value::as_str) {
        return c.lines().next().unwrap_or(c).to_string();
    }
    for key in ["file_path", "path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            return format!("{tool} {p}");
        }
    }
    tool.to_string()
}

/// 复选框式多选：打印带 [x]/[ ] 的列表，反复读「序号切换 / a 全选 / n 全不选 / 回车确认」。
fn toggle_select(items: &[Candidate], stdin: &mut impl BufRead) -> Vec<usize> {
    let mut sel: Vec<bool> = items.iter().map(|c| c.preselected).collect();
    loop {
        println!("    {}", dim("勾选要加入白名单的条目："));
        for (i, c) in items.iter().enumerate() {
            let mark = if sel[i] {
                green("[x]")
            } else {
                "[ ]".to_string()
            };
            println!(
                "      {} {} {}  {}",
                mark,
                i + 1,
                c.label,
                dim(&format!("→ {}", c.pattern))
            );
        }
        print!(
            "    {} > ",
            dim("序号空格分隔=切换 / a=全选 / n=全不选 / 回车=确认")
        );
        io::stdout().flush().ok();
        match read_line(stdin).trim() {
            "" => break,
            "a" => sel.iter_mut().for_each(|b| *b = true),
            "n" => sel.iter_mut().for_each(|b| *b = false),
            list => {
                for n in list
                    .split_whitespace()
                    .filter_map(|t| t.parse::<usize>().ok())
                {
                    if n >= 1 && n <= sel.len() {
                        sel[n - 1] = !sel[n - 1];
                    }
                }
            }
        }
    }
    (0..items.len()).filter(|&i| sel[i]).collect()
}

fn scope_label(scope: PermissionScope) -> &'static str {
    match scope {
        PermissionScope::Once => "once",
        PermissionScope::Session => "session",
        PermissionScope::Project => "project",
        PermissionScope::Global => "global",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn read_line(stdin: &mut impl BufRead) -> String {
    read_line_opt(stdin).unwrap_or_default()
}

fn read_line_opt(stdin: &mut impl BufRead) -> Option<String> {
    let mut buf = String::new();
    match stdin.read_line(&mut buf) {
        Ok(0) => None,
        Ok(_) => Some(buf.trim_end_matches(['\n', '\r']).to_string()),
        Err(_) => None,
    }
}

// ── 历史回放分析 ────────────────────────────────────────────────────────────────

/// 脚本解释器命令根：被 safe_commands 当只读自动放行，但跑脚本是有副作用的。
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "node",
    "ruby",
    "perl",
    "php",
    "deno",
    "bun",
    "Rscript",
    "osascript",
];

/// 从 tool_call 的 result 反推历史结局：被拒结果由 deny_tool 统一拼成
/// `工具调用被拒绝: …`（锚定开头避免误命中 Bash 输出里恰好含「拒绝」的行）；
/// 其余有结果 = 实际执行了（=当时放行，含执行失败 exit≠0——历史选择仍是放行），
/// 无结果 = 取消/中断（未知）。
#[derive(Clone, Copy, PartialEq)]
enum Hist {
    Approved,
    Denied,
    Unknown,
}

fn hist_outcome(result: Option<&str>) -> Hist {
    match result {
        Some(r) if r.starts_with("工具调用被拒绝") => Hist::Denied,
        Some(_) => Hist::Approved,
        None => Hist::Unknown,
    }
}

#[derive(Default)]
struct Stats {
    total: usize,
    auto: usize,
    prompt_approved: usize,
    prompt_denied: usize,
    prompt_unknown: usize,
    saved_by_remember: usize,
    regression: usize,
    deny_ok: usize,
    /// 解释器命令被自动放行（root → 次数）
    interpreter_auto: HashMap<String, usize>,
    /// 写操作被某条规则静默放行（pattern → 次数）
    write_auto_rule: HashMap<String, usize>,
    /// 「每次都点同意」的高频命令（root/工具 → 次数）——加白名单的候选
    friction_approved: HashMap<String, usize>,
    /// 现在会拒、但历史执行过（回归）
    regressions: Vec<String>,
    /// 同一 turn 内（两条 user 消息之间）完全相同的命令被重复要求审批的次数。
    /// 这是「allow once 不粘」最直接的浪费：前一步刚批过，下一步模型再发同一条又弹。
    turn_repeats: usize,
    /// turn 内重复审批的命令 → 重复次数。
    turn_repeat_cmds: HashMap<String, usize>,
}

impl Stats {
    fn merge(&mut self, o: Stats) {
        self.total += o.total;
        self.auto += o.auto;
        self.prompt_approved += o.prompt_approved;
        self.prompt_denied += o.prompt_denied;
        self.prompt_unknown += o.prompt_unknown;
        self.saved_by_remember += o.saved_by_remember;
        self.regression += o.regression;
        self.deny_ok += o.deny_ok;
        for (k, v) in o.interpreter_auto {
            *self.interpreter_auto.entry(k).or_default() += v;
        }
        for (k, v) in o.write_auto_rule {
            *self.write_auto_rule.entry(k).or_default() += v;
        }
        for (k, v) in o.friction_approved {
            *self.friction_approved.entry(k).or_default() += v;
        }
        for (k, v) in o.turn_repeat_cmds {
            *self.turn_repeat_cmds.entry(k).or_default() += v;
        }
        self.regressions.extend(o.regressions);
        self.turn_repeats += o.turn_repeats;
    }
}

/// 重复审批的判同 key：Bash 用整条命令，路径工具用 工具+路径。
fn repeat_key(tool: &str, input: &Value) -> String {
    if let Some(c) = input.get("command").and_then(Value::as_str) {
        return format!("Bash::{}", c.trim());
    }
    for k in ["file_path", "path"] {
        if let Some(p) = input.get(k).and_then(Value::as_str) {
            return format!("{tool}::{p}");
        }
    }
    format!("{tool}::{input}")
}

/// 列出最近 n 个能加载的 session id（目录名形如 `YYYYMMDDHHMM-...`，按名字倒序=按时间倒序）。
fn list_recent_sessions(data_dir: &PathBuf, n: usize) -> Vec<String> {
    let dir = data_dir.join("sessions");
    let mut ids: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name.len() >= 13 && name.as_bytes()[12] == b'-')
            .collect(),
        Err(_) => Vec::new(),
    };
    ids.sort();
    ids.reverse();
    ids.into_iter()
        .filter(|id| {
            data_dir
                .join("sessions")
                .join(id)
                .join("session.jsonl")
                .exists()
        })
        .take(n)
        .collect()
}

/// 分析单个 session，返回 (标题, 统计)。每个 session 用独立 gate，session 作用域记忆隔离。
fn analyze_session(data_dir: &PathBuf, id: &str) -> Option<(String, Stats)> {
    let session = sessions::load(data_dir, id).ok()?;
    let mut probe = Probe::new(
        session.workdir.clone(),
        session.allowed_paths.clone().unwrap_or_default(),
    );
    probe.session_artifact_root = Some(data_dir.join("sessions").join(id));
    let mut stats = Stats::default();
    let mut remembered: HashSet<String> = HashSet::new();
    // 同一 turn 内已审批过的命令（每遇到一条 user 消息清空 = 进入新 turn）。
    let mut turn_seen: HashSet<String> = HashSet::new();

    for m in &session.messages {
        if m.role == sessions::Role::User && !m.is_system_notification() {
            turn_seen.clear();
        }
        for tc in &m.tool_calls {
            let hist = hist_outcome(tc.result.as_deref());
            let j = probe.judge(&tc.name, &tc.input);
            stats.total += 1;
            // turn 内重复审批检测：只看「当前系统会拦」的调用，同一 turn 第 2+ 次出现即浪费。
            if matches!(j.outcome(), Outcome::Ask) {
                let rk = repeat_key(&tc.name, &tc.input);
                if !turn_seen.insert(rk.clone()) {
                    stats.turn_repeats += 1;
                    let label = tc
                        .input
                        .get("command")
                        .and_then(Value::as_str)
                        .map(|c| c.lines().next().unwrap_or(c).to_string())
                        .unwrap_or_else(|| call_title(&tc.name, &tc.input));
                    *stats.turn_repeat_cmds.entry(label).or_default() += 1;
                }
            }
            match j.outcome() {
                Outcome::Auto => {
                    stats.auto += 1;
                    // 过度放行体检（只看 Bash）
                    if tc.name == "Bash" {
                        for seg in &j.effects.segments {
                            let root = seg.fingerprint.split_whitespace().next().unwrap_or("");
                            if INTERPRETERS.contains(&root) {
                                *stats.interpreter_auto.entry(root.to_string()).or_default() += 1;
                            }
                        }
                        if j.effects.segments.iter().any(|s| !s.is_readonly) {
                            if let Some(m) = probe.store.find_for_segments_diagnostic(
                                Some(PROBE_SESSION_ID),
                                probe.workdir.as_deref(),
                                "Bash",
                                &j.effects.segments,
                            ) {
                                *stats.write_auto_rule.entry(m.pattern).or_default() += 1;
                            }
                        }
                    }
                }
                Outcome::Deny => {
                    if hist == Hist::Approved {
                        stats.regression += 1;
                        stats.regressions.push(call_title(&tc.name, &tc.input));
                    } else {
                        stats.deny_ok += 1;
                    }
                }
                Outcome::Ask => {
                    let keys = probe.call_keys(&tc.name, &j);
                    let covered = !keys.is_empty() && keys.iter().all(|k| remembered.contains(k));
                    if covered {
                        stats.saved_by_remember += 1;
                        continue;
                    }
                    match hist {
                        Hist::Approved => {
                            stats.prompt_approved += 1;
                            for k in &keys {
                                remembered.insert(k.clone());
                            }
                            let label = friction_key(&tc.name, &j);
                            *stats.friction_approved.entry(label).or_default() += 1;
                        }
                        Hist::Denied => stats.prompt_denied += 1,
                        Hist::Unknown => stats.prompt_unknown += 1,
                    }
                }
            }
        }
    }
    Some((session.title.clone(), stats))
}

/// 高频「每次都同意」命令的归并 key：Bash 用首段命令根，路径工具用工具名。
fn friction_key(tool: &str, j: &Judgement) -> String {
    if tool == "Bash" {
        if let Some(seg) = j.effects.segments.iter().find(|s| !s.is_readonly) {
            return seg
                .fingerprint
                .split_whitespace()
                .next()
                .unwrap_or(tool)
                .to_string();
        }
    }
    tool.to_string()
}

fn run_history(n: usize) {
    let data_dir = default_data_dir();
    let ids = list_recent_sessions(&data_dir, n);
    if ids.is_empty() {
        eprintln!("{}", red("未找到任何 session"));
        return;
    }
    println!(
        "{}",
        bold(&format!(
            "== 历史回放：最近 {} 个 session 的工具调用，用历史放行/拒绝作为你的选择 ==",
            ids.len()
        ))
    );
    let mut total = Stats::default();
    for id in &ids {
        if let Some((title, s)) = analyze_session(&data_dir, id) {
            println!(
                "  {} {}  {}",
                cyan(id),
                dim(&truncate(&title, 30)),
                format!(
                    "调用{} 自动{} 打扰{}(记住省{}) 拒{}",
                    s.total,
                    green(&s.auto.to_string()),
                    yellow(&(s.prompt_approved + s.prompt_denied + s.prompt_unknown).to_string()),
                    s.saved_by_remember,
                    s.deny_ok + s.regression,
                )
            );
            total.merge(s);
        }
    }

    println!("\n{}", bold("总计"));
    println!("  工具调用总数      {}", total.total);
    println!("  当前自动放行      {}", green(&total.auto.to_string()));
    println!(
        "  会打扰你审批      {}（其中历史同意 {} / 历史拒绝 {} / 未知 {}）",
        yellow(&(total.prompt_approved + total.prompt_denied + total.prompt_unknown).to_string()),
        total.prompt_approved,
        total.prompt_denied,
        total.prompt_unknown,
    );
    println!(
        "  「记住」可省的重复审批 {}",
        green(&total.saved_by_remember.to_string())
    );
    println!(
        "  {} {}",
        bold("同一 turn 内重复审批（allow once 不粘）"),
        red(&total.turn_repeats.to_string())
    );
    println!(
        "  历史执行但现在会拒（回归）{}",
        red(&total.regression.to_string())
    );

    print_problem(
        "① 脚本解释器被自动放行（跑脚本却免审批，风险）",
        &total.interpreter_auto,
    );
    print_problem(
        "② 写操作被规则静默放行（这些规则在放行会写命令）",
        &total.write_auto_rule,
    );
    print_problem(
        "③ 高频「每次都点同意」命令（建议加入白名单）",
        &total.friction_approved,
    );
    print_problem(
        "⑤ 同一 turn 内被重复审批的命令（前一步刚批、下一步又弹）",
        &total.turn_repeat_cmds,
    );
    if !total.regressions.is_empty() {
        println!("\n{}", yellow("④ 历史执行过、现在会被拒的命令（回归点）"));
        for r in total.regressions.iter().take(15) {
            println!("    {}", truncate(r, 80));
        }
    }
}

fn print_problem(title: &str, map: &HashMap<String, usize>) {
    if map.is_empty() {
        return;
    }
    println!("\n{}", yellow(title));
    let mut items: Vec<(&String, &usize)> = map.iter().collect();
    items.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in items.into_iter().take(15) {
        println!("    {:>4}×  {}", v, k);
    }
}

// ── 跨 run「记住」复现 ───────────────────────────────────────────────────────────

/// 复现桌面端真实拓扑：app 全局共享一个 PermissionStore，但**每次 run 新建 HitlGate**
/// （session.rs run_with_runtime_inputs）。验证「记住(session/project/global)」点完之后，
/// 下一次 run（新 gate、同 store、同 session_id、同 workdir）里同一命令还会不会再弹。
fn run_repro() {
    let wd = PathBuf::from("/Users/ricardo/code/ricardo/rust/hebbian");
    let sid = "repro-session";

    for (scope, name) in [
        (PermissionScope::Session, "当前对话(session)"),
        (PermissionScope::Project, "当前项目(project)"),
        (PermissionScope::Global, "全局(global)"),
    ] {
        println!("\n{}", bold(&format!("== 作用域：{name} ==")));
        let dir = std::env::temp_dir().join(format!("heb-repro-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(PermissionStore::open(&dir).unwrap());

        for cmd in [
            "cargo build",
            "cd apps/desktop && pnpm build",
            "cd crates && git commit -am wip", // 危险复合 cd-git-compound
            "rm -rf build",                    // 不可记忆
        ] {
            // run #1：新 gate，命中审批，点「记住」到该作用域。
            let g1 = HitlGate::new(PermissionPolicy::default()).with_store(
                store.clone(),
                sid,
                Some(wd.clone()),
            );
            let eff1 = analyze_effects("Bash", &json!({ "command": cmd }));
            let extra = g1.unapproved_memorable_writable_segments("Bash", &eff1);
            let (pattern, extras) = split_patterns(&extra);
            let id = match g1.check("Bash", &eff1) {
                PermissionDecision::NeedsApproval { request_id, .. } => request_id,
                other => {
                    println!("  {cmd}: run#1 未触发审批（{other:?}）—— 跳过");
                    continue;
                }
            };
            g1.resolve(
                &id,
                protocol::ApprovalDecision::AllowAndRemember {
                    scope,
                    pattern: pattern.clone(),
                    extra_patterns: extras.clone(),
                },
            );

            // run #2：**新 gate**，同 store / sid / workdir，模拟下一次模型请求。
            let g2 = HitlGate::new(PermissionPolicy::default()).with_store(
                store.clone(),
                sid,
                Some(wd.clone()),
            );
            let eff2 = analyze_effects("Bash", &json!({ "command": cmd }));
            let r2 = g2.check("Bash", &eff2);
            let ok = matches!(r2, PermissionDecision::Approved);
            println!(
                "  {} {:<38} 记忆[{}] → run#2: {}",
                if ok {
                    green("✓")
                } else {
                    red("✗ 又弹了")
                },
                cmd,
                pattern.clone().unwrap_or_else(|| "<空>".into())
                    + &extras.iter().map(|e| format!(",{e}")).collect::<String>(),
                if ok {
                    green("自动放行")
                } else {
                    red("需审批")
                },
            );

            // 额外：模拟「重开对话 / 重启 app」——从磁盘重开 store，看规则还在不在。
            let store2 = Arc::new(PermissionStore::open(&dir).unwrap());
            let g3 = HitlGate::new(PermissionPolicy::default()).with_store(
                store2,
                sid,
                Some(wd.clone()),
            );
            let r3 = g3.check("Bash", &analyze_effects("Bash", &json!({ "command": cmd })));
            let ok3 = matches!(r3, PermissionDecision::Approved);
            println!(
                "       {} 重开 store(模拟重启/重开对话) → {}",
                if ok3 { green("✓") } else { red("✗") },
                if ok3 {
                    green("仍放行")
                } else {
                    red("又弹了")
                },
            );
        }
    }
}

/// 把候选段切成 (pattern, extra_patterns)：首段命令根做 pattern，其余进 extra。
fn split_patterns(segs: &[String]) -> (Option<String>, Vec<String>) {
    let mut roots: Vec<String> = Vec::new();
    for fp in segs {
        let root = fp.split_whitespace().next().unwrap_or(fp).to_string();
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let pattern = roots.first().cloned();
    let extras = roots.into_iter().skip(1).collect();
    (pattern, extras)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut workdir: Option<PathBuf> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--workdir" => workdir = it.next().map(PathBuf::from),
            other => positional.push(other.to_string()),
        }
    }

    let stdin = io::stdin();
    let mut locked = stdin.lock();
    let mode = positional.first().map(String::as_str).unwrap_or("");

    // session 模式优先用 session 自带的 workdir / allowed_paths 还原现场。
    if mode == "session" {
        let Some(id) = positional.get(1) else {
            eprintln!("用法：permission_probe session <SESSION_ID>");
            return;
        };
        let data_dir = default_data_dir();
        let (wd, allowed) = match sessions::load(&data_dir, id) {
            Ok(s) => (
                workdir.clone().or(s.workdir.clone()),
                s.allowed_paths.clone().unwrap_or_default(),
            ),
            Err(_) => (workdir.clone(), Vec::new()),
        };
        let probe = Probe::new(wd, allowed);
        probe.run_session(id, &mut locked);
        return;
    }

    if mode == "repro" {
        run_repro();
        return;
    }
    if mode == "history" {
        let n = positional
            .get(1)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(10);
        run_history(n);
        return;
    }

    let probe = Probe::new(workdir, Vec::new());
    match mode {
        "batch" => {
            probe.run_baseline();
        }
        "repl" => {
            probe.run_repl(&mut locked);
        }
        "" => {
            probe.run_baseline();
            if io::stdin().is_terminal() {
                probe.run_repl(&mut locked);
            }
        }
        other => {
            eprintln!("未知模式「{other}」。可用：batch / repl / session <ID> / history [N]");
        }
    }
}
