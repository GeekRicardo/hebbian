pub mod background;
pub mod bash;
pub mod bash_output;
pub mod bash_prefix;
pub mod edit;
pub mod edit_hashline;
pub mod exit_plan_mode;
pub mod grep;
pub mod hitl;
pub mod kill_shell;
pub mod mcp;
pub mod preview_act;
pub mod preview_mutate;
pub mod preview_style;
pub use mcp::McpToolReport;
pub mod read;
pub mod read_hashline;
pub mod read_memory;
pub mod registry;
pub mod safe_commands;
pub mod schedule_wakeup;
pub mod shell_parse;
pub mod skill;
pub mod task;
pub mod todo_write;
pub mod web_fetch;
pub mod web_search;
pub mod write_memory;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use common::attachments::MessageAttachment;
use model_gateway::types::{ToolDefinition, IMAGE_GENERATION_TOOL_NAME};
use serde_json::Value;

use common::{AppResult, CancelFlag};

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
///
/// `cancel`：dispatcher 注入的取消标志。`BashTool` 在前台等待循环中检测到它置位
/// 后立即 kill 子进程并返回已产出内容，不再等待命令跑完。
pub struct ToolCtx {
    pub call_id: String,
    pub progress: Option<Arc<dyn ToolProgress>>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub cancel: Option<CancelFlag>,
}

impl ToolCtx {
    /// 不带任何 progress 通道的空 ctx——给单测 / CLI / 不需要流式的工具用。
    pub fn noop() -> Self {
        Self {
            call_id: String::new(),
            progress: None,
            session_id: None,
            run_id: None,
            cancel: None,
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

/// 工具的完整输出（架构 §4.4.1）：文本 + 多模态附件。
///
/// 绝大多数工具只产文本，返回 `text.into()` 即可，`attachments` 默认空。
/// 产出图片等多模态内容的工具（首期仅 `Read` 读图片）把 base64 附件挂到
/// `attachments`，由 dispatcher 透传进 `ToolResult.attachments`，再经协议层
/// 编码进模型上下文（强模型原生图片块 / 弱模型 VisionBridge 转文字）。
///
/// 与 progress chunk 的区别：progress 是 surface-only 观察通道（不进模型上下文），
/// 这里的 attachments **进**模型上下文。
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self {
            text,
            attachments: Vec::new(),
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
///
/// **多模态工具**（如 Read 读图片）覆盖 [`execute_rich`]：返回 [`ToolOutput`]
/// 携带附件。默认实现把 `execute_streaming` 的文本包成纯文本 ToolOutput——
/// 文本工具不用动。dispatcher 统一走 `execute_rich`。
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> AppResult<String>;

    async fn execute_streaming(&self, _ctx: ToolCtx, input: Value) -> AppResult<String> {
        self.execute(input).await
    }

    async fn execute_rich(&self, ctx: ToolCtx, input: Value) -> AppResult<ToolOutput> {
        Ok(self.execute_streaming(ctx, input).await?.into())
    }
}

/// 构造内置 + 用户可选工具：
/// - 内置：Bash / BashOutput / KillShell / Read / Edit / Grep / Skill（与 ask 一起每次自动注入）
/// - 用户可选：web_search / web_fetch（按 enabled_tools 过滤）
///
/// `BashTool` / `BashOutputTool` / `KillShellTool` 共享同一个 [`background::BgTaskRegistry`]
/// 注册表：超时或 `run_in_background=true` 时进程转后台，其余两个工具按 task_id 增量查询 / 终止。
///
/// `bg_log_dir` 为本 session 的后台输出落盘目录（架构 §4.12.3）。生产路径通常是
/// `~/.hebbian/sessions/<sid>/bg/`；CLI 单跑 / 单测可传 `None`，BgTaskRegistry
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
    shells: background::BgTaskRegistry,
    data_dir: Option<PathBuf>,
    session_id: Option<String>,
    read_state_tracker: Option<Arc<ReadStateTracker>>,
    shell: Option<String>,
    edit_backend: crate::storage::settings::EditBackend,
) -> Vec<Box<dyn Tool>> {
    let mut skills = skill::load_skills(skill_dirs);
    // disabled 的 skill 不暴露给模型（架构 §6.1.3 用户级 UX）
    if let Some(dd) = data_dir.as_ref() {
        crate::storage::skills::apply_disabled(dd, &mut skills);
    }
    let skills: Vec<_> = skills.into_iter().filter(|s| s.enabled).collect();

    // 加载并合并 subagent 定义（架构 §4.4.11.5）：项目级 enabled 覆盖全局
    // workspace.workdir 是项目根；data_dir 是 ~/.hebbian/
    let subagents: Vec<_> = data_dir
        .as_ref()
        .map(|dd| {
            crate::storage::subagents::load_for_workdir(dd, Some(workspace.workdir()))
                .into_iter()
                .filter(|d| d.enabled)
                .collect()
        })
        .unwrap_or_default();

    // 记忆工具上下文（架构 §4.14）：data_dir 下面会被 Read 工具 move 走，先 clone；
    // project_workdir 仅在 workdir 是具体项目（非 home / 非根）时为 Some——决定
    // WriteMemory 能否写 project 作用域。
    let mem_data_dir = data_dir.clone();
    let project_workdir = memory_project_workdir(workspace.workdir());

    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(bash::BashTool::new(
            workspace.clone(),
            shells.clone(),
            bg_log_dir,
            shell,
        )),
        Box::new(bash_output::BashOutputTool::new(shells.clone())),
        Box::new(kill_shell::KillShellTool::new(shells.clone())),
        Box::new(schedule_wakeup::ScheduleWakeupTool::new(phase)),
    ];

    // Read 与 Edit 必须配套切换：hashline patch 里的行号/hash 基于 hashline Read 的输出
    use crate::storage::settings::EditBackend;
    match edit_backend {
        EditBackend::StringReplace => {
            tools.push(Box::new(read::ReadTool::new(
                data_dir,
                session_id,
                read_state_tracker.clone(),
            )));
            tools.push(Box::new(edit::EditTool::new(
                workspace.clone(),
                read_state_tracker,
            )));
        }
        EditBackend::Hashline => {
            tools.push(Box::new(read_hashline::ReadHashlineTool::new(
                data_dir,
                session_id,
                read_state_tracker.clone(),
            )));
            tools.push(Box::new(edit_hashline::EditHashlineTool::new(
                workspace.clone(),
                read_state_tracker,
            )));
        }
    }

    tools.extend([
        Box::new(grep::GrepTool::new(workspace)) as Box<dyn Tool>,
        Box::new(skill::SkillTool::new(skills)),
        Box::new(todo_write::TodoWriteTool),
        Box::new(web_search::WebSearchTool),
        Box::new(web_fetch::WebFetchTool),
        Box::new(exit_plan_mode::ExitPlanModeTool),
        // 仅内置浏览器「元素对话」旁支会话用（enabled_tools 含 PreviewStyle 才暴露，
        // 不进 BUILTIN_TOOL_NAMES，普通会话看不到）。
        Box::new(preview_style::PreviewStyleTool),
        Box::new(preview_mutate::PreviewMutateTool),
        Box::new(preview_act::PreviewActTool),
        Box::new(read_memory::ReadMemoryTool::new(
            mem_data_dir.clone(),
            project_workdir.clone(),
        )),
        Box::new(write_memory::WriteMemoryTool::new(
            mem_data_dir,
            project_workdir,
        )),
    ]);

    // Task 工具仅在加载到至少一个启用的 subagent 定义时注入（架构 §13 决策）：
    // 没有定义时把工具暴露给模型只会污染上下文（模型调用必返回"找不到 subagent"错误）。
    if !subagents.is_empty() {
        tools.push(Box::new(task::TaskTool::new(subagents)));
    }
    tools
}

pub async fn default_tools_with_mcp(
    workspace: Arc<Workspace>,
    skill_dirs: &[(skill::SkillSource, PathBuf)],
    bg_log_dir: Option<PathBuf>,
    phase: crate::wakeup::PhaseChannel,
    shells: background::BgTaskRegistry,
    data_dir: Option<PathBuf>,
    session_id: Option<String>,
    read_state_tracker: Option<Arc<ReadStateTracker>>,
    shell: Option<String>,
    edit_backend: crate::storage::settings::EditBackend,
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
        shell,
        edit_backend,
    );
    tools.extend(mcp::discover_tools(&mcp_config).await);
    tools
}

/// 内置工具名（每次 ModelRequest 自动注入；不在 UI 工具菜单中暴露）。
/// 顺序与 `default_tools` 中的注册顺序对齐。
/// `Task` 走条件注入（仅当存在启用的 subagent 定义时），不列入这里。
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "Bash",
    "BashOutput",
    "KillShell",
    "ScheduleWakeup",
    "Read",
    "Edit",
    "Grep",
    "Skill",
    "TodoWrite",
    "ReadMemory",
    "WriteMemory",
];

/// 记忆工具名（架构 §4.14）。subagent 过滤据此剔除——本期 subagent 不给记忆能力。
pub const MEMORY_TOOL_NAMES: &[&str] = &[
    read_memory::READ_MEMORY_TOOL_NAME,
    write_memory::WRITE_MEMORY_TOOL_NAME,
];

/// 当前对话绑定的项目 workdir：workdir 是具体项目目录时返回 `Some`，是 home / 文件系统根
/// （即「没在某个项目里」）时返回 `None`。记忆工具与注入据此决定 project 作用域可用性。
pub fn memory_project_workdir(workdir: &std::path::Path) -> Option<PathBuf> {
    let is_home = dirs::home_dir().as_deref() == Some(workdir);
    let is_root = workdir.parent().is_none();
    if is_home || is_root {
        None
    } else {
        Some(workdir.to_path_buf())
    }
}

/// 条件注入工具名：`default_tools` 仅在前置条件满足时把这些工具注册进 registry
/// （例如 Task 仅在存在启用的 subagent 定义时注入）。dispatch 层把它们一律加进
/// 工具白名单——registry 没有的名字会被 [`registry::ToolRegistry::definitions`]
/// 自然忽略，所以这里多列不会带来副作用，但少列会让条件注入的工具发不到模型。
pub const CONDITIONAL_TOOL_NAMES: &[&str] = &["Task"];

pub fn is_builtin_tool(name: &str) -> bool {
    name == ASK_TOOL_NAME
        || BUILTIN_TOOL_NAMES.contains(&name)
        || CONDITIONAL_TOOL_NAMES.contains(&name)
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

/// `ask` 工具的 schema：让 agent 主动向用户提问。
///
/// 两种入参形态二选一：
/// - **单题**：`question` + `options`（2-5 个）+ 可选 `multi`
/// - **多题**：`questions`（1-5 道，每道独立 `title` / `description` / `options` / `multi`）
pub fn ask_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_TOOL_NAME.to_string(),
        description: "向用户提问以澄清需求或获取决策。务必给出 2-5 个**实质性**候选选项 \
                      （label 控制在 12 字以内）：每个选项都必须是用户可能直接选中的具体答案。\
                      **禁止**出现「其他」「让我重新描述」「以上都不是」「自由回答」「再想想」 \
                      之类的兜底/元选项。需要让用户多选时把 `multi` 设为 true。\
                      \n\n一次需要问多道关联问题时填 `questions` 数组（最多 5 道，每道独立 \
                      `title` / 可选 `description` / `options` / `multi`），用户在同一弹窗里逐题回答。"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "单题模式：提给用户的问题。简短直接，避免冗长背景。"
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 5,
                    "description": "单题模式：候选选项 2-5 个。",
                    "items": ask_option_schema()
                },
                "multi": {
                    "type": "boolean",
                    "default": false,
                    "description": "单题模式：是否允许多选。true=允许勾选多个选项；缺省 false（单选）。"
                },
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 5,
                    "description": "多题模式：把多道子题一次性发给用户，按顺序展示。\
                                    非空时本字段优先，单题字段被忽略。",
                    "items": {
                        "type": "object",
                        "required": ["title", "options"],
                        "properties": {
                            "title": {
                                "type": "string",
                                "description": "子题标题。简短直接。"
                            },
                            "description": {
                                "type": "string",
                                "description": "子题说明，给用户更多上下文。可选。"
                            },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 5,
                                "items": ask_option_schema()
                            },
                            "multi": {
                                "type": "boolean",
                                "default": false,
                                "description": "该子题是否允许多选。"
                            }
                        }
                    }
                }
            }
        }),
    }
}

fn ask_option_schema() -> serde_json::Value {
    serde_json::json!({
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
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use tempfile::TempDir;

    fn make_registry_with_subagent(tmp: &TempDir) -> ToolRegistry {
        let data_dir = tmp.path().to_path_buf();
        let subagents_dir = data_dir.join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        std::fs::write(
            subagents_dir.join("echo.md"),
            "---\ndescription: \"test echo\"\n---\necho body",
        )
        .unwrap();

        let workspace = crate::workspace::Workspace::new(tmp.path().to_path_buf(), vec![]);
        let phase: crate::wakeup::PhaseChannel = std::sync::Arc::new(std::sync::Mutex::new(None));
        let shells = background::BgTaskRegistry::new();
        let tools = default_tools(
            workspace,
            &[],
            None,
            phase,
            shells,
            Some(data_dir),
            None,
            None,
            None,
            crate::storage::settings::EditBackend::default(),
        );
        ToolRegistry::new(tools)
    }

    #[test]
    fn conditional_tools_pass_through_dispatch_filter() {
        // 回归：dispatch 用 BUILTIN_TOOL_NAMES 当白名单时，没把条件注入工具放进去，
        // 导致 default_tools 已注册的 Task 被 registry.definitions 过滤掉，
        // 模型从未看到 Task。修复后必须保证 BUILTIN+CONDITIONAL 一起喂给 filter。
        let tmp = TempDir::new().unwrap();
        let registry = make_registry_with_subagent(&tmp);

        let mut filter: Vec<String> = BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
        filter.extend(CONDITIONAL_TOOL_NAMES.iter().map(|s| s.to_string()));

        let names: Vec<String> = registry
            .definitions(&filter)
            .into_iter()
            .map(|d| d.name)
            .collect();

        assert!(
            names.contains(&"Task".to_string()),
            "Task 必须出现在 dispatch filter 后的工具定义里，实际：{names:?}"
        );
    }

    #[test]
    fn conditional_tool_names_includes_task() {
        // Task 走条件注入：必须在 CONDITIONAL_TOOL_NAMES 里，否则 dispatch 永远过滤掉它。
        assert!(CONDITIONAL_TOOL_NAMES.contains(&"Task"));
        assert!(!BUILTIN_TOOL_NAMES.contains(&"Task"));
    }

    #[test]
    fn task_present_due_to_builtin_subagents() {
        // builtin 内置 subagent（架构 §4.4.11.12）让 subagents 永不为空：即使用户没写任何
        // 磁盘定义，default_tools 也会注册 Task（D9.1 决策推翻了早先「无定义→无 Task」的假设）。
        let tmp = TempDir::new().unwrap();
        let workspace = crate::workspace::Workspace::new(tmp.path().to_path_buf(), vec![]);
        let phase: crate::wakeup::PhaseChannel = std::sync::Arc::new(std::sync::Mutex::new(None));
        let shells = background::BgTaskRegistry::new();
        let tools = default_tools(
            workspace,
            &[],
            None,
            phase,
            shells,
            Some(tmp.path().to_path_buf()),
            None,
            None,
            None,
            crate::storage::settings::EditBackend::default(),
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"Task"),
            "builtin subagent 在时应注册 Task：{names:?}"
        );
    }
}
