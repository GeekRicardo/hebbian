//! 工具调用卡片的文案：把「工具名 + 入参」翻成一句人话。
//!
//! 逐条对齐原前端 `MessageBubble.tsx` 的 `defaultActionLabel` / `callSummary`——
//! 卡片上读到的应该是「读取文件 main.rs:120」而不是「Read /很长的/绝对/路径/main.rs」。
//! 纯函数，方便单测钉住每条映射。

use serde_json::Value;

/// 取字符串入参，空串当没有。
fn arg<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 路径末段。工具卡片上显示全路径太长，原前端也是只取文件名。
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn is_shell_command_tool(name: &str) -> bool {
    matches!(name, "Bash" | "PowerShell" | "InteractiveBash")
}

fn is_task_list_tool(name: &str) -> bool {
    matches!(name, "TodoWrite" | "TaskList")
}

/// 这次调用「在做什么」——卡片上的动作名。
pub fn action_label(name: &str) -> &'static str {
    if is_shell_command_tool(name) {
        return "运行命令";
    }
    if is_task_list_tool(name) {
        return "任务列表";
    }
    match name {
        "BashOutput" => "读取后台命令输出",
        "BashInput" => "发送后台命令输入",
        "KillShell" => "停止后台命令",
        "Read" => "读取文件",
        "ReadMemory" => "读取记忆",
        "WriteMemory" => "记下",
        "Write" => "写入文件",
        "Edit" => "编辑文件",
        "Grep" => "搜索代码",
        "Glob" => "匹配文件",
        "Skill" => "读取技能说明",
        "Ask" => "用户提问记录",
        "WebSearch" => "网络搜索",
        "Fetch" => "抓取网页内容",
        "image_generation" => "生成图片",
        "ExitPlanMode" => "提交计划",
        _ => "自定义工具",
    }
}

/// 卡片中段那句「在做什么」。
///
/// **模型自己在入参里写的 `description` 优先**（Bash 的推荐用法就是带一句简短意图，
/// 比通用动词具体得多），没有才回退到 [`action_label`]。这一层我第一版整个漏了。
pub fn call_description(name: &str, input: &Value) -> String {
    if let Some(desc) = arg(input, "description") {
        return desc.to_string();
    }
    action_label(name).to_string()
}

/// 这次调用「作用在什么上」——动作名后面那截摘要。
pub fn call_summary(name: &str, input: &Value) -> String {
    if is_shell_command_tool(name) {
        return arg(input, "command").unwrap_or("运行命令").to_string();
    }
    if is_task_list_tool(name) {
        return todo_summary(input);
    }
    match name {
        "BashOutput" => arg(input, "task_id").unwrap_or("读取后台命令输出").to_string(),
        "BashInput" => arg(input, "task_id").unwrap_or("发送后台命令输入").to_string(),
        "KillShell" => arg(input, "task_id").unwrap_or("停止后台命令").to_string(),
        "Read" => match arg(input, "file_path") {
            // Read 带 offset 时显示 `文件名:行号`，与原前端一致。
            Some(file) => match arg(input, "offset") {
                Some(offset) => format!("{}:{offset}", basename(file)),
                None => basename(file).to_string(),
            },
            None => "读取文件".to_string(),
        },
        "Write" | "Edit" => match arg(input, "file_path") {
            Some(file) => basename(file).to_string(),
            None => if name == "Edit" { "编辑文件" } else { "写入文件" }.to_string(),
        },
        "ReadMemory" => arg(input, "id").unwrap_or("读取记忆").to_string(),
        "WriteMemory" => arg(input, "summary")
            .or_else(|| arg(input, "key"))
            .unwrap_or("记下一条")
            .to_string(),
        "Grep" => arg(input, "pattern").unwrap_or("搜索代码").to_string(),
        "Glob" => arg(input, "pattern").unwrap_or("匹配文件").to_string(),
        "Skill" => arg(input, "name")
            .or_else(|| arg(input, "skill"))
            .unwrap_or("读取技能")
            .to_string(),
        "Ask" => arg(input, "question").unwrap_or("用户提问记录").to_string(),
        "WebSearch" => arg(input, "query").unwrap_or("网络搜索").to_string(),
        "Fetch" => arg(input, "url").unwrap_or("抓取网页内容").to_string(),
        "image_generation" => arg(input, "prompt").unwrap_or("生成图片").to_string(),
        "ExitPlanMode" => arg(input, "plan_markdown").unwrap_or("提交计划").to_string(),
        // 兜底顺序与原前端一致
        _ => arg(input, "prompt")
            .or_else(|| arg(input, "query"))
            .or_else(|| arg(input, "file_path"))
            .unwrap_or("自定义工具调用")
            .to_string(),
    }
}

/// TodoWrite 的摘要是统计而不是原文：`N 项 · M 完成 · K 进行中`。
fn todo_summary(input: &Value) -> String {
    let todos = input.get("todos").and_then(|v| v.as_array());
    let Some(todos) = todos.filter(|t| !t.is_empty()) else {
        return "任务列表".to_string();
    };
    let status_is = |item: &Value, want: &str| {
        item.get("status").and_then(|v| v.as_str()) == Some(want)
    };
    let done = todos.iter().filter(|t| status_is(t, "completed")).count();
    let active = todos.iter().filter(|t| status_is(t, "in_progress")).count();
    let mut segs = vec![format!("{} 项", todos.len()), format!("{done} 完成")];
    if active > 0 {
        segs.push(format!("{active} 进行中"));
    }
    segs.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_family_shares_one_label() {
        for name in ["Bash", "PowerShell", "InteractiveBash"] {
            assert_eq!(action_label(name), "运行命令");
            assert_eq!(call_summary(name, &json!({"command": "ls -al"})), "ls -al");
        }
    }

    #[test]
    fn read_shows_basename_and_offset() {
        let input = json!({"file_path": "/a/b/main.rs", "offset": "120"});
        assert_eq!(action_label("Read"), "读取文件");
        // 只取文件名，带 offset 时拼行号
        assert_eq!(call_summary("Read", &input), "main.rs:120");
        assert_eq!(
            call_summary("Read", &json!({"file_path": "/a/b/main.rs"})),
            "main.rs"
        );
    }

    #[test]
    fn edit_and_write_show_basename_only() {
        let input = json!({"file_path": "/very/long/path/config.toml"});
        assert_eq!(call_summary("Edit", &input), "config.toml");
        assert_eq!(call_summary("Write", &input), "config.toml");
    }

    #[test]
    fn todo_summary_counts_instead_of_quoting() {
        let input = json!({"todos": [
            {"status": "completed"}, {"status": "completed"},
            {"status": "in_progress"}, {"status": "pending"},
        ]});
        assert_eq!(call_summary("TodoWrite", &input), "4 项 · 2 完成 · 1 进行中");
        // 没有进行中的就不显示那一段
        let input = json!({"todos": [{"status": "completed"}]});
        assert_eq!(call_summary("TodoWrite", &input), "1 项 · 1 完成");
    }

    #[test]
    fn unknown_tool_falls_back_in_documented_order() {
        assert_eq!(action_label("MysteryTool"), "自定义工具");
        // prompt > query > file_path
        assert_eq!(
            call_summary("MysteryTool", &json!({"query": "q", "file_path": "f"})),
            "q"
        );
        assert_eq!(
            call_summary("MysteryTool", &json!({"file_path": "f"})),
            "f"
        );
        assert_eq!(call_summary("MysteryTool", &json!({})), "自定义工具调用");
    }

    #[test]
    fn description_prefers_model_written_intent() {
        // 模型写了 description 就用它，而不是通用的「运行命令」
        let input = json!({"command": "du -sh target/", "description": "查看编译产物占用"});
        assert_eq!(call_description("Bash", &input), "查看编译产物占用");
        // 没写才回退到动作名
        assert_eq!(
            call_description("Bash", &json!({"command": "ls"})),
            "运行命令"
        );
    }

    /// 空串入参要当作「没有」，否则卡片上会出现一段空白摘要。
    #[test]
    fn empty_string_args_are_treated_as_missing() {
        assert_eq!(call_summary("Grep", &json!({"pattern": "  "})), "搜索代码");
    }
}
