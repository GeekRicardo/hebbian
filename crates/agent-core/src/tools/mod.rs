pub mod background;
pub mod bash;
pub mod bash_output;
pub mod edit;
pub mod exit_plan_mode;
pub mod grep;
pub mod hitl;
pub mod kill_shell;
pub mod mcp;
pub use mcp::McpToolReport;
pub mod read;
pub mod registry;
pub mod safe_commands;
pub mod schedule_wakeup;
pub mod shell_parse;
pub mod skill;
pub mod todo_write;
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

/// 工具进度回调：让长跑工具（前台 Bash 等待、流式 Fetch 等）在 ToolCallStarted
/// 与 ToolCallFinished 之间向 surface 推增量输出。chunk 通常是 UTF-8 文本，
/// 不需带换行；调用方按原样追加到对应 tool 卡片即可。
///
/// 实现方只负责往主事件流喂 `EventPayload::ToolCallOutputDelta`，dispatcher
/// 已经知道 dispatch_index / call_id，所以这里只暴露 chunk 字符串接口。
pub trait ToolProgress: Send + Sync {
    fn emit(&self, chunk: String);
}

/// Tool::execute 的上下文（架构 §4.4.1）。除工具自身的 input 外，dispatcher
/// 还需把 call 元信息 + 流式 progress 通道 + run/session 标识塞进来。
/// **默认 noop**：单测、CLI 直接 invoke 工具时构造一个空 ctx 即可。
///
/// `session_id` / `run_id`：BashTool 在 register 后台 task 时需要它们调
/// `WakeupScheduler::arm_bg_task`——让 task 终态时自动通知模型（架构 §4.12.5）。
/// 老 ToolCtx::noop() 路径不带，BashTool 自动 arm 时检测到 None 就跳过。
pub struct ToolCtx {
    pub call_id: String,
    pub progress: Option<Arc<dyn ToolProgress>>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
}

impl ToolCtx {
    /// 不带任何 progress 通道的空 ctx——给单测 / CLI / 不需要流式的工具用。
    pub fn noop() -> Self {
        Self {
            call_id: String::new(),
            progress: None,
            session_id: None,
            run_id: None,
        }
    }

    pub fn emit_chunk(&self, chunk: impl Into<String>) {
        if let Some(p) = self.progress.as_ref() {
            let s = chunk.into();
            if !s.is_empty() {
                p.emit(s);
            }
        }
    }
}

/// 极简 Tool 接口（架构 §4.4.1）：只描述「我是什么 + 我怎么干」。
///
/// 权限分类、路径解析、命令指纹等上下文相关信息由 dispatcher 旁的
/// [`crate::effects::analyze_effects`] 集中处理；Tool trait 不再持有这些
/// 默认实现。
///
/// **流式工具**（如 Bash 前台等待）覆盖 [`execute_streaming`]：在 await 期间
/// 通过 `ctx.emit_chunk(...)` 向 surface 推 `ToolCallOutputDelta`，返回值仍
/// 是聚合后的完整文本（写入 ToolCallFinished.result + 推回模型）。
/// 非流式工具不用覆盖——默认实现直接委托给 [`execute`]，忽略 ctx。
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> AppResult<String>;

    async fn execute_streaming(&self, _ctx: ToolCtx, input: Value) -> AppResult<String> {
        self.execute(input).await
    }
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
        Box::new(todo_write::TodoWriteTool),
        Box::new(web_search::WebSearchTool),
        Box::new(web_fetch::WebFetchTool),
        Box::new(exit_plan_mode::ExitPlanModeTool),
    ]
}

pub async fn default_tools_with_mcp(
    workspace: Arc<Workspace>,
    skill_dirs: &[(skill::SkillSource, PathBuf)],
    bg_log_dir: Option<PathBuf>,
    phase: crate::wakeup::PhaseChannel,
    shells: background::BackgroundShells,
    data_dir: Option<PathBuf>,
    session_id: Option<String>,
    read_state_tracker: Option<Arc<ReadStateTracker>>,
    mcp_config: crate::mcp::config::McpConfig,
) -> Vec<Box<dyn Tool>> {
    let mut tools = default_tools(
        workspace,
        skill_dirs,
        bg_log_dir,
        phase,
        shells,
        data_dir,
        session_id,
        read_state_tracker,
    );
    tools.extend(mcp::discover_tools(&mcp_config).await);
    tools
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
    "TodoWrite",
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
    let tools = vec![
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
    ];
    tools
}

pub fn tool_manifest_with_mcp(config: &crate::mcp::config::McpConfig) -> Vec<ToolInfo> {
    let mut tools = tool_manifest();
    tools.extend(mcp::manifest(config));
    tools
}
