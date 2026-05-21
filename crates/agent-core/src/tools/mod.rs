pub mod background;
pub mod bash;
pub mod bash_output;
pub mod edit;
pub mod exit_plan_mode;
pub mod grep;
pub mod hitl;
pub mod kill_shell;
pub mod read;
pub mod registry;
pub mod safe_commands;
pub mod schedule_wakeup;
pub mod shell_parse;
pub mod skill;
pub mod wait_for_task;
pub mod web_fetch;
pub mod web_search;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use model_gateway::types::{ToolDefinition, IMAGE_GENERATION_TOOL_NAME};
use serde_json::Value;

use common::AppResult;

use crate::read_state::ReadStateTracker;
use crate::workspace::Workspace;

/// 内置 ask 工具的名称。
pub const ASK_TOOL_NAME: &str = "Ask";

/// 极简 Tool 接口（架构 §4.4.1）：只描述「我是什么 + 我怎么干」。
///
/// 权限分类、路径解析、命令指纹等上下文相关信息由 dispatcher 旁的
/// [`crate::effects::analyze_effects`] 集中处理；Tool trait 不再持有这些
/// 默认实现。
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> AppResult<String>;
}

/// 构造内置 + 用户可选工具：
/// - 内置：Bash / BashOutput / KillShell / Read / Edit / Grep / Skill（与 ask 一起每次自动注入）
/// - 用户可选：web_search / web_fetch（按 enabled_tools 过滤）
///
/// `BashTool` / `BashOutputTool` / `KillShellTool` 共享同一个 [`background::BackgroundShells`]
/// 注册表：超时或 `run_in_background=true` 时进程转后台，其余两个工具按 task_id 增量查询 / 终止。
///
/// `bg_log_dir` 为本 session 的后台输出落盘目录（架构 §4.12.3）。生产路径通常是
/// `~/.hebbian/sessions/<sid>/bg/`；CLI 单跑 / 单测可传 `None`，BackgroundShells
/// 会回落到 tail-only。
///
/// `read_state_tracker` 是 session 级 Read 状态追踪表（架构 §4.4.10）：
/// Read 工具读完写入、Edit 工具读取做前置校验。CLI 单跑 / 单测可传 `None`，
/// 此时 Edit 工具的"必须先 Read"约束会被跳过（行为与历史 Write 工具兼容）。
pub fn default_tools(
    workspace: Arc<Workspace>,
    skill_dirs: &[(skill::SkillSource, PathBuf)],
    bg_log_dir: Option<PathBuf>,
    phase: crate::wakeup::PhaseChannel,
    shells: background::BackgroundShells,
    data_dir: Option<PathBuf>,
    session_id: Option<String>,
    read_state_tracker: Option<Arc<ReadStateTracker>>,
) -> Vec<Box<dyn Tool>> {
    let mut skills = skill::load_skills(skill_dirs);
    // disabled 的 skill 不暴露给模型（架构 §6.1.3 用户级 UX）
    if let Some(dd) = data_dir.as_ref() {
        crate::storage::skills::apply_disabled(dd, &mut skills);
    }
    let skills: Vec<_> = skills.into_iter().filter(|s| s.enabled).collect();
    vec![
        Box::new(bash::BashTool::new(
            workspace.clone(),
            shells.clone(),
            bg_log_dir,
        )),
        Box::new(bash_output::BashOutputTool::new(shells.clone())),
        Box::new(kill_shell::KillShellTool::new(shells.clone())),
        Box::new(wait_for_task::WaitForTaskTool::new(
            shells.clone(),
            phase.clone(),
        )),
        Box::new(schedule_wakeup::ScheduleWakeupTool::new(phase)),
        Box::new(read::ReadTool::new(
            data_dir,
            session_id,
            read_state_tracker.clone(),
        )),
        Box::new(edit::EditTool::new(workspace.clone(), read_state_tracker)),
        Box::new(grep::GrepTool::new(workspace)),
        Box::new(skill::SkillTool::new(skills)),
        Box::new(web_search::WebSearchTool),
        Box::new(web_fetch::WebFetchTool),
        Box::new(exit_plan_mode::ExitPlanModeTool),
    ]
}

/// 内置工具名（每次 ModelRequest 自动注入；不在 UI 工具菜单中暴露）。
/// 顺序与 `default_tools` 中的注册顺序对齐。
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "Bash",
    "BashOutput",
    "KillShell",
    "WaitForTask",
    "ScheduleWakeup",
    "Read",
    "Edit",
    "Grep",
    "Skill",
];

pub fn is_builtin_tool(name: &str) -> bool {
    name == ASK_TOOL_NAME || BUILTIN_TOOL_NAMES.contains(&name)
}

/// 由 agent_loop 直接处理、不需要 Tool trait 实现的"虚拟工具"。
/// `enabled_tools` 包含其名字时注入定义到 ModelRequest.tools。
/// 这里只放**用户可选**的 hosted 工具（如 image_generation 由模型 provider 端运行）。
pub fn hosted_tool_definitions(filter: &[String]) -> Vec<ToolDefinition> {
    let mut defs = Vec::new();
    if filter.iter().any(|name| name == IMAGE_GENERATION_TOOL_NAME) {
        defs.push(ToolDefinition {
            name: IMAGE_GENERATION_TOOL_NAME.to_string(),
            description: "生成或编辑图片".into(),
            parameters: serde_json::json!({"type": "object"}),
        });
    }
    defs
}

/// 内置工具定义：每次 ModelRequest 都自动注入，不在 UI 工具菜单里出现，
/// 用户也无法关闭。
///
/// 内置工具特征：
/// - 是「agent 能力」的一部分，不该让用户误以为关掉会有性能收益
/// - 与 HITL 紧密耦合（统一走 `HitlGate`：ask 走提问通路，Bash/Edit 等走审批通路）
///
/// `ask` 的定义在这里硬编码；其他内置工具（Bash/Read/Edit/Grep/Skill）
/// 由 `ToolRegistry` 持有实现，`registry.builtin_definitions()` 读取它们的 schema。
pub fn ask_only_definitions() -> Vec<ToolDefinition> {
    vec![ask_tool_definition()]
}

/// `ask` 工具的 schema：让 agent 主动向用户提问，2-5 个候选选项。
pub fn ask_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_TOOL_NAME.to_string(),
        description: "向用户提问以澄清需求或获取决策。务必同时给出 2-5 个**实质性**候选选项 \
                      （label 控制在 12 字以内）：每个选项都必须是用户可能直接选中的具体答案。\
                      **禁止**出现「其他」「让我重新描述」「以上都不是」「自由回答」「再想想」 \
                      之类的兜底/元选项。需要让用户多选时把 `multi` 设为 true。"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "required": ["question", "options"],
            "properties": {
                "question": {
                    "type": "string",
                    "description": "提给用户的问题。简短直接，避免冗长背景。"
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 5,
                    "items": {
                        "type": "object",
                        "required": ["label"],
                        "properties": {
                            "label": {
                                "type": "string",
                                "description": "选项的简短文字（按钮文字），1-12 字。"
                            },
                            "description": {
                                "type": "string",
                                "description": "可选的详细说明。"
                            }
                        }
                    }
                },
                "multi": {
                    "type": "boolean",
                    "default": false,
                    "description": "是否允许用户多选。true=允许勾选多个选项；缺省 false（单选）。"
                }
            }
        }),
    }
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub icon: String,
}

/// 暴露给 UI 的工具菜单。**内置工具**（ask / Bash / Read / Edit / Grep / Skill）
/// 默认开启且不可见，**不出现**在这个列表中。
pub fn tool_manifest() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "WebSearch".into(),
            description: "DuckDuckGo 网络搜索".into(),
            icon: "search".into(),
        },
        ToolInfo {
            name: "Fetch".into(),
            description: "抓取网页内容".into(),
            icon: "globe".into(),
        },
        ToolInfo {
            name: IMAGE_GENERATION_TOOL_NAME.into(),
            description: "生成图片".into(),
            icon: "image".into(),
        },
    ]
}
