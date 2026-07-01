//! 斜杠命令：解析渠道文本中的 `/xxx` 命令，路由到 CoreClient 同步 API。

use std::path::Path;

use agent_core::core_client::CoreClient;

use crate::owner_state::OwnerState;

pub enum CommandResult {
    Reply(String),
    NotCommand,
}

pub fn dispatch(
    text: &str,
    state: &mut OwnerState,
    core: &dyn CoreClient,
    data_dir: &Path,
    channel: &str,
    account_id: &str,
) -> CommandResult {
    let text = text.trim();
    if !text.starts_with('/') {
        return CommandResult::NotCommand;
    }

    let mut parts = text.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim();

    match cmd {
        "/projects" => cmd_projects(core),
        "/threads" => cmd_threads(core, state),
        "/use" => cmd_use(core, args, state, data_dir, channel, account_id),
        "/history" => cmd_history(core, state, args),
        "/providers" => cmd_providers(core),
        "/models" => cmd_models(core, args, state),
        "/new" => cmd_new(core, args, state, data_dir, channel, account_id),
        "/status" => cmd_status(state),
        "/help" => cmd_help(),
        _ => CommandResult::NotCommand,
    }
}

fn cmd_projects(core: &dyn CoreClient) -> CommandResult {
    match core.list_projects() {
        Ok(projects) => {
            if projects.is_empty() {
                return CommandResult::Reply(
                    "暂无项目。在 Desktop 里添加项目后这里就能看到。".into(),
                );
            }
            let mut lines = vec!["📂 项目列表：".to_string()];
            for project in projects {
                lines.push(format!("  {} — {}", project.id, project.name));
            }
            CommandResult::Reply(lines.join("\n"))
        }
        Err(err) => CommandResult::Reply(format!("❌ 获取项目失败：{err}")),
    }
}

/// 当前 project 过滤后的对话列表，序号语义与 `/threads` 显示一致，供 `/use` 复用。
fn filtered_sessions(
    core: &dyn CoreClient,
    state: &OwnerState,
) -> Result<Vec<agent_core::storage::sessions::SessionMeta>, String> {
    let sessions = core.list_sessions().map_err(|err| err.to_string())?;
    Ok(if let Some(project_id) = &state.project_id {
        sessions
            .into_iter()
            .filter(|session| session.project_id.as_deref() == Some(project_id.as_str()))
            .collect()
    } else {
        sessions
    })
}

fn cmd_threads(core: &dyn CoreClient, state: &OwnerState) -> CommandResult {
    let filtered = match filtered_sessions(core, state) {
        Ok(filtered) => filtered,
        Err(err) => return CommandResult::Reply(format!("❌ 获取对话失败：{err}")),
    };

    if filtered.is_empty() {
        return CommandResult::Reply("暂无对话。用 /new 创建一个。".into());
    }

    let mut lines = vec!["💬 对话列表：".to_string()];
    for (index, session) in filtered.iter().take(20).enumerate() {
        let marker = if state.active_session_id.as_deref() == Some(session.id.as_str()) {
            " ◀ 当前"
        } else {
            ""
        };
        let short_id = session.id.chars().take(8).collect::<String>();
        lines.push(format!(
            "  {}. [{}] {}{}",
            index + 1,
            short_id,
            session.title,
            marker
        ));
    }
    if filtered.len() > 20 {
        lines.push(format!("  ...共 {} 条，只显示最近 20 条", filtered.len()));
    }
    lines.push("用 /use <序号> 切换到某条对话。".into());
    CommandResult::Reply(lines.join("\n"))
}

/// `/use <序号|短id|完整id>`：把某条已有对话设为当前活跃，同步 provider/model/project。
fn cmd_use(
    core: &dyn CoreClient,
    args: &str,
    state: &mut OwnerState,
    data_dir: &Path,
    channel: &str,
    account_id: &str,
) -> CommandResult {
    let key = args.trim();
    if key.is_empty() {
        return CommandResult::Reply("用法：/use <序号|对话id>，序号见 /threads。".into());
    }

    let filtered = match filtered_sessions(core, state) {
        Ok(filtered) => filtered,
        Err(err) => return CommandResult::Reply(format!("❌ 获取对话失败：{err}")),
    };
    if filtered.is_empty() {
        return CommandResult::Reply("暂无对话。用 /new 创建一个。".into());
    }

    let target = match key.parse::<usize>() {
        Ok(index) if index >= 1 && index <= filtered.len() => Some(&filtered[index - 1]),
        Ok(_) => {
            return CommandResult::Reply(format!("❌ 序号超范围，共 {} 条对话。", filtered.len()))
        }
        Err(_) => filtered
            .iter()
            .find(|session| session.id == key || session.id.starts_with(key)),
    };

    let session = match target {
        Some(session) => session,
        None => return CommandResult::Reply(format!("❌ 找不到对话 {key}，用 /threads 看列表。")),
    };

    state.active_session_id = Some(session.id.clone());
    state.provider_id = Some(session.provider_id.clone());
    state.model = Some(session.model.clone());
    state.project_id = session.project_id.clone();
    if let Err(err) = state.save(data_dir, channel, account_id) {
        return CommandResult::Reply(format!("❌ 保存渠道状态失败：{err}"));
    }

    let short_id = session.id.chars().take(8).collect::<String>();
    CommandResult::Reply(format!(
        "✅ 已切换到对话\n  [{short_id}] {}\n  Provider: {}\n  Model: {}",
        session.title, session.provider_id, session.model
    ))
}

/// `/history [n]`：当前对话最近 n 条消息（默认 5，上限 20）。
fn cmd_history(core: &dyn CoreClient, state: &OwnerState, args: &str) -> CommandResult {
    let session_id = match &state.active_session_id {
        Some(id) => id,
        None => return CommandResult::Reply("还没有活跃对话。用 /new 或 /use 选一个。".into()),
    };
    let limit = args.trim().parse::<usize>().unwrap_or(5).clamp(1, 20);

    let session = match core.load_session(session_id) {
        Ok(session) => session,
        Err(err) => return CommandResult::Reply(format!("❌ 读取对话失败：{err}")),
    };

    let rendered: Vec<String> = session
        .messages
        .iter()
        .filter(|message| {
            matches!(
                message.role,
                agent_core::storage::sessions::Role::User
                    | agent_core::storage::sessions::Role::Assistant
            ) && !message.content.trim().is_empty()
        })
        .rev()
        .take(limit)
        .map(|message| {
            let who = match message.role {
                agent_core::storage::sessions::Role::User => "🧑 你",
                _ => "🤖 AI",
            };
            let body = truncate_chars(message.content.trim(), 200);
            format!("{who}：{body}")
        })
        .collect();

    if rendered.is_empty() {
        return CommandResult::Reply("当前对话还没有消息。".into());
    }

    let mut lines = vec![format!("📜 最近 {} 条：", rendered.len())];
    lines.extend(rendered.into_iter().rev());
    CommandResult::Reply(lines.join("\n\n"))
}

/// 按字符数截断，超长加省略号（避免在多字节边界切断）。
fn truncate_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let mut out: String = chars.into_iter().take(max).collect();
    out.push('…');
    out
}

fn cmd_providers(core: &dyn CoreClient) -> CommandResult {
    match core.list_providers() {
        Ok(file) => {
            if file.providers.is_empty() {
                return CommandResult::Reply("暂无供应商。在 Desktop 设置里添加。".into());
            }
            let mut lines = vec!["🔌 供应商列表：".to_string()];
            for provider in file.providers {
                let default = provider
                    .default_model
                    .as_ref()
                    .map(|model| format!("（默认模型：{model}）"))
                    .unwrap_or_default();
                let marker = if file.default_provider_id.as_deref() == Some(provider.id.as_str()) {
                    " ◀ 默认"
                } else {
                    ""
                };
                lines.push(format!(
                    "  {} — {:?}{}{}",
                    provider.id, provider.kind, default, marker
                ));
            }
            CommandResult::Reply(lines.join("\n"))
        }
        Err(err) => CommandResult::Reply(format!("❌ 获取供应商失败：{err}")),
    }
}

fn cmd_models(core: &dyn CoreClient, args: &str, state: &OwnerState) -> CommandResult {
    let provider_id = if args.is_empty() {
        match &state.provider_id {
            Some(id) => id.clone(),
            None => return CommandResult::Reply("请指定 provider：/models <provider_id>".into()),
        }
    } else {
        args.to_string()
    };

    let file = match core.list_providers() {
        Ok(file) => file,
        Err(err) => return CommandResult::Reply(format!("❌ 获取供应商失败：{err}")),
    };
    let provider = match file
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
    {
        Some(provider) => provider,
        None => return CommandResult::Reply(format!("❌ 供应商 {provider_id} 不存在")),
    };

    let mut models = provider.models.clone();
    if let Some(fetched) = &provider.fetched_models {
        for model in fetched {
            if !models.contains(model) {
                models.push(model.clone());
            }
        }
    }
    if let Some(default) = &provider.default_model {
        if !models.contains(default) {
            models.insert(0, default.clone());
        }
    }

    if models.is_empty() {
        return CommandResult::Reply(format!(
            "供应商 {provider_id} 下暂无模型缓存。请在 Desktop 里刷新模型列表。"
        ));
    }

    let mut lines = vec![format!("🤖 {provider_id} 下的模型：")];
    for model in models.iter().take(30) {
        let marker = if provider.default_model.as_deref() == Some(model.as_str()) {
            " ◀ 默认"
        } else {
            ""
        };
        lines.push(format!("  {model}{marker}"));
    }
    if models.len() > 30 {
        lines.push(format!("  ...共 {} 个，只显示前 30 个", models.len()));
    }
    CommandResult::Reply(lines.join("\n"))
}

fn cmd_new(
    core: &dyn CoreClient,
    args: &str,
    state: &mut OwnerState,
    data_dir: &Path,
    channel: &str,
    account_id: &str,
) -> CommandResult {
    let mut project_id = state.project_id.clone();
    let mut provider_id = state.provider_id.clone();
    let mut model = state.model.clone();

    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "--project" | "-p" => {
                index += 1;
                project_id = tokens.get(index).map(|value| value.to_string());
            }
            "--provider" => {
                index += 1;
                provider_id = tokens.get(index).map(|value| value.to_string());
            }
            "--model" | "-m" => {
                index += 1;
                model = tokens.get(index).map(|value| value.to_string());
            }
            _ => {}
        }
        index += 1;
    }

    let providers = match core.list_providers() {
        Ok(file) => file,
        Err(err) => return CommandResult::Reply(format!("❌ 获取供应商失败：{err}")),
    };

    if provider_id.is_none() {
        provider_id = providers.default_provider_id.clone();
    }
    if model.is_none() {
        if let Some(provider_id) = &provider_id {
            model = providers
                .providers
                .iter()
                .find(|provider| provider.id == *provider_id)
                .and_then(|provider| provider.default_model.clone());
        }
    }

    let provider_id = match provider_id {
        Some(id) => id,
        None => return CommandResult::Reply("❌ 未指定 provider，用 /new --provider <id>".into()),
    };
    let model = match model {
        Some(model) => model,
        None => return CommandResult::Reply("❌ 未指定 model，用 /new --model <name>".into()),
    };

    let mut session = match agent_core::storage::sessions::create_with_source(
        data_dir,
        provider_id.clone(),
        model.clone(),
        None,
        None,
        "channel".into(),
    ) {
        Ok(session) => session,
        Err(err) => return CommandResult::Reply(format!("❌ 创建对话失败：{err}")),
    };

    if let Some(project_id) = &project_id {
        session.project_id = Some(project_id.clone());
        if let Ok(projects) = core.list_projects() {
            if let Some(project) = projects.iter().find(|project| project.id == *project_id) {
                session.workdir = project.workdir().cloned();
                let allowed_paths = project.allowed_paths();
                if !allowed_paths.is_empty() {
                    session.allowed_paths = Some(allowed_paths);
                }
            }
        }
        match agent_core::storage::sessions::save(data_dir, session.clone()) {
            Ok(saved) => session = saved,
            Err(err) => return CommandResult::Reply(format!("❌ 保存对话失败：{err}")),
        }
    }

    let _ = agent_core::storage::sessions_dir::ensure_session_dirs(data_dir, &session.id);

    state.active_session_id = Some(session.id.clone());
    state.provider_id = Some(provider_id.clone());
    state.model = Some(model.clone());
    state.project_id = project_id.clone();
    if let Err(err) = state.save(data_dir, channel, account_id) {
        return CommandResult::Reply(format!("❌ 保存渠道状态失败：{err}"));
    }

    let short_id = session.id.chars().take(8).collect::<String>();
    CommandResult::Reply(format!(
        "✅ 新对话已创建\n  ID: {short_id}\n  Provider: {provider_id}\n  Model: {model}{}",
        project_id
            .as_ref()
            .map(|id| format!("\n  Project: {id}"))
            .unwrap_or_default()
    ))
}

fn cmd_status(state: &OwnerState) -> CommandResult {
    let lines = [
        "📊 当前状态：".to_string(),
        format!(
            "  Session: {}",
            state.active_session_id.as_deref().unwrap_or("无")
        ),
        format!(
            "  Provider: {}",
            state.provider_id.as_deref().unwrap_or("无")
        ),
        format!("  Model: {}", state.model.as_deref().unwrap_or("无")),
        format!("  Project: {}", state.project_id.as_deref().unwrap_or("无")),
    ];
    CommandResult::Reply(lines.join("\n"))
}

fn cmd_help() -> CommandResult {
    CommandResult::Reply(
        "📖 可用命令：\n\
         /new [--project <id>] [--provider <id>] [--model <name>]  新建对话\n\
         /threads         列出对话（当前项目下）\n\
         /use <序号|id>   切换到某条已有对话\n\
         /history [n]     当前对话最近 n 条消息（默认 5）\n\
         /cancel          停止当前对话正在跑的 AI\n\
         /projects        列出所有项目\n\
         /providers       列出供应商\n\
         /models [id]     列出模型\n\
         /status          当前状态\n\
         /help            显示帮助\n\n\
         直接发文字 → 跟当前对话的 AI 聊天"
            .into(),
    )
}
