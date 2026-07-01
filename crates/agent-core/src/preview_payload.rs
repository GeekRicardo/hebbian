//! 「显示原始 JSON」预览：复刻 agent_loop 进入模型调用前的全部拼装动作
//! （workspace XML、内置工具、用户启用的工具、session 历史 transcript），
//! 但**不真正发起请求、不修改 session**。输出统一为 OpenAI 风格 `{model, messages, tools, ...}`。
//!
//! 纯业务逻辑，无 surface 依赖：desktop / hebweb 共用同一份预览实现。

use std::path::{Path, PathBuf};

use common::{attachments::MessageAttachment, AppResult};
use serde_json::Value;

use crate::permissions::PermissionStore;
use crate::storage::sessions::{self, Message, MessagePart, Role};
use crate::storage::settings as global_settings;
use crate::system_prompt::{compose_system_prompt, EnvironmentSnapshot};
use crate::tools::skill::default_skill_dirs;
use crate::tools::{
    ask_only_definitions, hosted_tool_definitions, registry::ToolRegistry, BUILTIN_TOOL_NAMES,
    CONDITIONAL_TOOL_NAMES,
};
use crate::workspace::Workspace;

/// 构造一份「真实发给模型的 payload」预览,用于 UI 的「显示原始 JSON」。
///
/// 复刻 agent_loop 进入模型调用之前的所有拼装动作:workspace XML、内置工具、
/// 用户启用的工具、session 历史 transcript,但**不真正发起请求、不修改 session**。
/// 输出统一为 OpenAI 风格的 `{model, messages, tools, ...}`,前端用 JsonView 渲染。
pub async fn build_preview_payload(
    data_dir: &Path,
    session_id: &str,
    upto_message_id: Option<&str>,
) -> AppResult<Value> {
    let session = sessions::load(data_dir, session_id)?;
    let settings = global_settings::load(data_dir);

    let workdir = session
        .workdir
        .clone()
        .or_else(|| settings.conversation.workdir.clone())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let initial_allowed_paths = session
        .allowed_paths
        .clone()
        .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
    let workspace = Workspace::with_runtime_state(
        workdir.clone(),
        initial_allowed_paths.clone(),
        session.runtime_allowed_paths.clone(),
        session.pending_runtime_allowed_paths.clone(),
    );

    let configured_skill_dirs = session
        .skill_dirs
        .clone()
        .unwrap_or_else(|| settings.conversation.skill_dirs.clone());
    let skill_dirs: Vec<(crate::tools::skill::SkillSource, PathBuf)> =
        if configured_skill_dirs.is_empty() {
            default_skill_dirs(data_dir, &workdir)
        } else {
            configured_skill_dirs
                .into_iter()
                .map(|p| (crate::tools::skill::SkillSource::Global, p))
                .collect()
        };

    // preview 用同样的优先级链：session 非空 → session；否则全局
    let session_enabled_tools = {
        let s = session.enabled_tools.clone().unwrap_or_default();
        if s.is_empty() {
            settings.conversation.enabled_tools.clone()
        } else {
            s
        }
    };

    // 工具定义:ask + 内置 + 用户开的本地工具 + provider hosted 工具。
    // 预览路径不会真发命令,bg_log_dir + phase 都用占位 None / 空 channel。
    // BgTaskRegistry 用临时本地实例（预览只生成 tool schema，不真跑命令）。
    let registry = ToolRegistry::new(
        crate::tools::default_tools_with_mcp(
            workspace.clone(),
            &skill_dirs,
            None,
            crate::wakeup::new_phase_channel(),
            crate::tools::background::BgTaskRegistry::new(),
            None,
            None,
            None,
            settings.general.shell.clone(),
            settings.general.edit_backend,
            crate::storage::mcp::load(data_dir).with_cwd(workspace.workdir().to_path_buf()),
        )
        .await,
    );
    let mut tool_defs = ask_only_definitions();
    let mut all_filter: Vec<String> = BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    all_filter.extend(CONDITIONAL_TOOL_NAMES.iter().map(|s| s.to_string()));
    all_filter.extend(session_enabled_tools.iter().cloned());
    tool_defs.extend(registry.definitions(&all_filter));
    tool_defs.extend(registry.mcp_definitions());
    if !session_enabled_tools.is_empty() {
        tool_defs.extend(hosted_tool_definitions(&session_enabled_tools));
    }

    // system = BASE prompt + 用户 persona + rules（与 agent_loop 一致）
    let combined_system = {
        let mut s = compose_system_prompt(session.system_prompt.as_deref());
        let used_global_rules_for_system = session
            .global_rules
            .clone()
            .unwrap_or_else(|| settings.conversation.global_rules.clone());
        let rules_content_for_system = crate::rules::resolve_injection_files(
            &used_global_rules_for_system,
            session.rules_files.as_deref(),
            &workdir,
            &initial_allowed_paths,
        );
        let rules_block_for_system = crate::rules::format_injection(&rules_content_for_system);
        if !rules_block_for_system.is_empty() {
            s.push('\n');
            s.push_str(&rules_block_for_system);
        }
        s
    };

    // 首条 user message 头部要追加 <environment> 块（与 Session::append_user 一致），
    // preview 时按同一逻辑还原，确保「显示 JSON」与实际发给模型的 payload 一致。
    let extra_paths_preview = PermissionStore::open(data_dir)
        .map(|s| s.effective_paths(Some(&workdir)))
        .unwrap_or_default();
    let env_snapshot =
        EnvironmentSnapshot::from_workspace(&workspace).with_extra_paths(extra_paths_preview);
    let env_block = env_snapshot.render();

    let mut first_user_pending = true;

    let mut messages: Vec<Value> = vec![serde_json::json!({
        "role": "system",
        "content": combined_system,
    })];
    for m in &session.messages {
        match m.role {
            Role::Marker | Role::System => {}
            Role::User => {
                let mut value = preview_user_content(m);
                if first_user_pending {
                    prepend_environment_to_preview(&mut value, &env_block);
                    first_user_pending = false;
                }
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": value,
                }))
            }
            Role::Assistant => preview_push_assistant(&mut messages, m),
        }
        if upto_message_id.is_some_and(|id| m.id == id) {
            break;
        }
    }

    let tools: Vec<Value> = tool_defs
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect();

    Ok(serde_json::json!({
        "model": session.model,
        "messages": messages,
        "tools": tools,
        "_workspace": {
            "workdir": workdir.display().to_string(),
            "initial_allowed_paths": initial_allowed_paths,
            "runtime_allowed_paths": session.runtime_allowed_paths,
            "pending_runtime_allowed_paths": session.pending_runtime_allowed_paths,
            "skill_dirs": skill_dirs.iter().map(|(_, p)| p.display().to_string()).collect::<Vec<_>>(),
        }
    }))
}

/// 把 `<environment>` 块前置到 preview 的 user content 上。
/// content 是 string 时直接拼前缀；是 array（含 attachments）时拼到首个 text block 前，
/// 没有 text block 就插一个新的 text block 在最前。
fn prepend_environment_to_preview(value: &mut Value, env_block: &str) {
    if env_block.is_empty() {
        return;
    }
    match value {
        Value::String(s) => {
            *s = format!("{env_block}{s}");
        }
        Value::Array(blocks) => {
            if let Some(first_text) = blocks
                .iter_mut()
                .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
            {
                if let Some(text) = first_text.get_mut("text").and_then(|v| v.as_str()) {
                    let merged = format!("{env_block}{text}");
                    first_text["text"] = Value::String(merged);
                }
            } else {
                blocks.insert(0, serde_json::json!({"type": "text", "text": env_block}));
            }
        }
        _ => {}
    }
}

fn preview_user_content(m: &Message) -> Value {
    if m.attachments.is_empty() {
        return Value::String(m.content.clone());
    }
    let mut blocks: Vec<Value> = Vec::new();
    if !m.content.is_empty() {
        blocks.push(serde_json::json!({"type": "text", "text": m.content}));
    }
    for a in &m.attachments {
        match a {
            MessageAttachment::Image {
                media_type, data, ..
            } => blocks.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", media_type, data) },
            })),
            MessageAttachment::TextFile {
                name,
                media_type,
                content,
            } => blocks.push(serde_json::json!({
                "type": "text",
                "text": format!(
                    "<file name=\"{name}\" media_type=\"{media_type}\">\n{content}\n</file>"
                ),
            })),
        }
    }
    Value::Array(blocks)
}

fn preview_push_assistant(out: &mut Vec<Value>, m: &Message) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_results: Vec<Value> = Vec::new();

    let push_call = |list: &mut Vec<Value>, id: &str, name: &str, args: String| {
        list.push(serde_json::json!({
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": args },
        }));
    };

    if !m.parts.is_empty() {
        for p in &m.parts {
            match p {
                MessagePart::Text { text } => text_parts.push(text.clone()),
                MessagePart::Reasoning { text, .. } => reasoning_parts.push(text.clone()),
                MessagePart::ToolCall {
                    id,
                    name,
                    input,
                    arguments,
                    result,
                    ..
                } => {
                    let args = if !arguments.is_empty() {
                        arguments.clone()
                    } else {
                        serde_json::to_string(input).unwrap_or_else(|_| "{}".into())
                    };
                    push_call(&mut tool_calls, id, name, args);
                    if let Some(res) = result {
                        tool_results.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": res,
                        }));
                    }
                }
            }
        }
    } else if !m.tool_calls.is_empty() {
        if !m.content.is_empty() {
            text_parts.push(m.content.clone());
        }
        for tc in &m.tool_calls {
            let args = serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".into());
            push_call(&mut tool_calls, &tc.id, &tc.name, args);
            if let Some(res) = &tc.result {
                tool_results.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "content": res,
                }));
            }
        }
    } else if !m.content.is_empty() {
        text_parts.push(m.content.clone());
    }

    let mut assistant = serde_json::json!({
        "role": "assistant",
        "content": text_parts.join(""),
    });
    let map = assistant.as_object_mut().expect("json object");
    if !reasoning_parts.is_empty() {
        map.insert("reasoning".into(), Value::String(reasoning_parts.join("")));
    }
    if !tool_calls.is_empty() {
        map.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    out.push(assistant);
    out.extend(tool_results);
}
