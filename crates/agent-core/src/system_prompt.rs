//! 系统提示词的唯一来源。
//!
//! 两层：
//!
//! 1. [`BASE_SYSTEM_PROMPT`]：Hebbian 内置的 agent 身份 + 行为准则。**整个仓库唯一的常量**，
//!    跨会话恒定，方便 prompt cache 命中。模型 schema 之外的"策略"全在这里说，工具 schema
//!    本身的字段说明保留在各个 `Tool::description` 上，不在 system 里重复。
//! 2. 用户 persona（[`compose_system_prompt`] 的入参）：用户在 `prompts.json` 里选的角色风格，
//!    每次会话可不同。拼在 base 之后。
//!
//! 另外 [`EnvironmentSnapshot`] 渲染 `<environment>` 块，**不进 system**，走第一条
//! user message 头部，避免 system 段内容随机器/时间漂移。
//!
//! 设计原则：system 段必须是字节稳定的前缀；任何随时间或会话变化的事实都走 user message。
//! 这样 prompt cache 跨会话也能命中。

use std::path::{Path, PathBuf};

use crate::workspace::Workspace;

/// Hebbian 的基础系统提示词。
///
/// 章节顺序：identity → 沟通 → 客观性 → 工具策略 → 行动可逆性 → 工程任务 → 验收
/// → Git → 安全 → 输出 → 环境。
/// 不区分纯聊天 / 写代码模式：模型自己能根据对话内容判断当前任务，不会硬套不相干的章节。
pub const BASE_SYSTEM_PROMPT: &str = include_str!("../prompts/base_system.md");

/// 拼出最终的 system 段：base + 可选用户 persona。
///
/// persona 为空 / 全空白时只返回 base，避免末尾出现空 section。
pub fn compose_system_prompt(persona: Option<&str>) -> String {
    let persona = persona.map(str::trim).filter(|s| !s.is_empty());
    match persona {
        Some(p) => format!("{BASE_SYSTEM_PROMPT}\n\n# 用户角色\n\n{p}"),
        None => BASE_SYSTEM_PROMPT.to_string(),
    }
}

/// 一次会话开始时的环境快照。注入到第一条 user message 头部。
#[derive(Debug, Clone)]
pub struct EnvironmentSnapshot {
    pub workdir: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
    pub platform: &'static str,
    pub shell: Option<String>,
    pub date: String,
    /// 当前运行模式（架构 §4.4.3）。`None` = 不渲染，模型按默认行为推理。
    pub run_mode: Option<&'static str>,
    /// 当前后台 shell 列表（架构 §4.12.7）。非空时在 `<environment>` 旁
    /// 渲染一个 `<background_tasks>` 子块，让模型立刻看到。
    /// 元组：(task_id, state_label, command, elapsed_secs)
    pub background_tasks: Vec<BackgroundTaskSummary>,
}

/// `<background_tasks>` 渲染所需的最小信息。
#[derive(Debug, Clone)]
pub struct BackgroundTaskSummary {
    pub task_id: String,
    pub state: String,
    pub command: String,
    pub elapsed_secs: u64,
}

impl EnvironmentSnapshot {
    /// 从 workspace + 当前进程环境拼出快照。
    /// 只读 `initial_allowed_paths`：runtime_pending 通过 `<workspace-update>` 单独通知。
    pub fn from_workspace(workspace: &Workspace) -> Self {
        Self {
            workdir: workspace.workdir().to_path_buf(),
            allowed_paths: workspace.initial_allowed_paths().to_vec(),
            platform: std::env::consts::OS,
            shell: detect_shell(),
            date: today_iso(),
            run_mode: None,
            background_tasks: Vec::new(),
        }
    }

    /// builder-style：设置当前 run_mode。
    pub fn with_run_mode(mut self, mode: crate::run_mode::RunMode) -> Self {
        self.run_mode = Some(mode.as_str());
        self
    }

    /// builder-style：把当前后台 shell 列表塞进来（架构 §4.12.7）。
    pub fn with_background_tasks(mut self, tasks: Vec<BackgroundTaskSummary>) -> Self {
        self.background_tasks = tasks;
        self
    }

    /// 渲染成 `<environment>` XML 块，末尾保留空行便于和正文分隔。
    pub fn render(&self) -> String {
        let mut s = render_environment_xml(
            &self.workdir,
            &self.allowed_paths,
            self.platform,
            self.shell.as_deref(),
            &self.date,
            self.run_mode,
        );
        if !self.background_tasks.is_empty() {
            s.push_str("<background_tasks>\n");
            for t in &self.background_tasks {
                s.push_str(&format!(
                    "  - {} [{}] {}s `{}`\n",
                    t.task_id, t.state, t.elapsed_secs, t.command,
                ));
            }
            s.push_str("</background_tasks>\n\n");
        }
        s
    }
}

/// 把环境块前置到 user content。用于第一条 user message。
pub fn prepend_environment(text: String, snapshot: &EnvironmentSnapshot) -> String {
    let mut s = snapshot.render();
    s.push_str(&text);
    s
}

/// 把 `<background_tasks>` 块单独前置到 user content（架构 §4.12.7）。
/// 用于非首条 user message——首条由 `prepend_environment` 内嵌在
/// `<environment>` 旁，这里覆盖后续每一条。tasks 为空时调用方应跳过本函数。
pub fn prepend_background_tasks(text: String, tasks: &[BackgroundTaskSummary]) -> String {
    if tasks.is_empty() {
        return text;
    }
    let mut s = String::from("<background_tasks>\n");
    for t in tasks {
        s.push_str(&format!(
            "  - {} [{}] {}s `{}`\n",
            t.task_id, t.state, t.elapsed_secs, t.command,
        ));
    }
    s.push_str("</background_tasks>\n\n");
    s.push_str(&text);
    s
}

fn render_environment_xml(
    workdir: &Path,
    allowed_paths: &[PathBuf],
    platform: &str,
    shell: Option<&str>,
    date: &str,
    run_mode: Option<&str>,
) -> String {
    let mut s = String::from("<environment>\n");
    s.push_str(&format!("  <cwd>{}</cwd>\n", workdir.display()));
    for d in allowed_paths {
        s.push_str(&format!("  <allowed_path>{}</allowed_path>\n", d.display()));
    }
    s.push_str(&format!("  <platform>{platform}</platform>\n"));
    if let Some(sh) = shell {
        s.push_str(&format!("  <shell>{sh}</shell>\n"));
    }
    s.push_str(&format!("  <date>{date}</date>\n"));
    if let Some(mode) = run_mode {
        s.push_str(&format!("  <run_mode>{mode}</run_mode>\n"));
    }
    s.push_str("</environment>\n\n");
    s
}

fn detect_shell() -> Option<String> {
    std::env::var("SHELL").ok().and_then(|p| {
        Path::new(&p)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
    })
}

fn today_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_without_persona_returns_base() {
        let s = compose_system_prompt(None);
        assert_eq!(s, BASE_SYSTEM_PROMPT);
    }

    #[test]
    fn compose_appends_persona_section() {
        let s = compose_system_prompt(Some("你是代码搭档"));
        assert!(s.starts_with(BASE_SYSTEM_PROMPT));
        assert!(s.contains("# 用户角色"));
        assert!(s.contains("你是代码搭档"));
    }

    #[test]
    fn compose_treats_empty_persona_as_none() {
        assert_eq!(compose_system_prompt(Some("   ")), BASE_SYSTEM_PROMPT);
        assert_eq!(compose_system_prompt(Some("")), BASE_SYSTEM_PROMPT);
    }

    #[test]
    fn base_covers_core_sections() {
        // 烟雾测试：所有关键章节都在
        let s = BASE_SYSTEM_PROMPT;
        for h in [
            "# 沟通",
            "# 客观性与诚实",
            "# 工具策略",
            "# 行动的可逆性",
            "# 写代码",
            "# 完成与验收",
            "# Git 与版本控制",
            "# 安全",
            "# 输出",
            "# 环境上下文",
        ] {
            assert!(s.contains(h), "missing section: {h}");
        }
    }

    #[test]
    fn environment_xml_includes_workdir_and_dirs() {
        let xml = render_environment_xml(
            Path::new("/tmp/work"),
            &[PathBuf::from("/tmp/extra")],
            "darwin",
            Some("zsh"),
            "2026-05-10",
            None,
        );
        assert!(xml.starts_with("<environment>"));
        assert!(xml.contains("<cwd>/tmp/work</cwd>"));
        assert!(xml.contains("<allowed_path>/tmp/extra</allowed_path>"));
        assert!(xml.contains("<platform>darwin</platform>"));
        assert!(xml.contains("<shell>zsh</shell>"));
        assert!(xml.contains("<date>2026-05-10</date>"));
        assert!(xml.ends_with("</environment>\n\n"));
    }

    #[test]
    fn prepend_environment_attaches_to_user_text() {
        let snap = EnvironmentSnapshot {
            workdir: PathBuf::from("/tmp"),
            allowed_paths: Vec::new(),
            platform: "darwin",
            shell: None,
            date: "2026-05-10".into(),
            run_mode: None,
            background_tasks: Vec::new(),
        };
        let out = prepend_environment("hello".into(), &snap);
        assert!(out.starts_with("<environment>"));
        assert!(out.ends_with("hello"));
    }
}
