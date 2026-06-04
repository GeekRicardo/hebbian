//! 验证 settings.general.edit_backend 切换时，Read 和 Edit 工具对（含 description / schema）整体被替换。
//!
//! 用户问："设置里的开关可以一键切换回原来的 edit/read 吗包括 prompt"
//! 答：是的——本测试就是证据：注册产物里的 description（即模型看到的 prompt）与 schema 整体跟着 enum 走。

use agent_core::storage::settings::EditBackend;
use agent_core::tools::{background::BgTaskRegistry, default_tools};
use agent_core::wakeup::new_phase_channel;
use agent_core::workspace::Workspace;
use serde_json::Value;

fn build_tools(backend: EditBackend) -> Vec<(String, String, Value)> {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(tmp.path(), Vec::new());
    let phase = new_phase_channel();
    let shells = BgTaskRegistry::new();

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
        backend,
    );

    tools
        .into_iter()
        .map(|t| {
            (
                t.name().to_string(),
                t.description().to_string(),
                t.parameters_schema(),
            )
        })
        .collect()
}

fn find<'a>(tools: &'a [(String, String, Value)], name: &str) -> &'a (String, String, Value) {
    tools.iter().find(|(n, _, _)| n == name).expect("tool")
}

#[test]
fn string_replace_backend_registers_string_replace_edit() {
    let tools = build_tools(EditBackend::StringReplace);
    let (_, _, schema) = find(&tools, "Edit");
    let props = schema["properties"].as_object().unwrap();
    assert!(
        props.contains_key("old_string") && props.contains_key("new_string"),
        "string-replace 后端 Edit schema 必须含 old_string/new_string，实际: {props:?}"
    );
    assert!(
        !props.contains_key("patch"),
        "string-replace 后端 schema 不应有 patch 字段"
    );
}

#[test]
fn hashline_backend_registers_hashline_edit() {
    let tools = build_tools(EditBackend::Hashline);
    let (_, _, schema) = find(&tools, "Edit");
    let props = schema["properties"].as_object().unwrap();
    assert!(
        props.contains_key("patch"),
        "hashline 后端 Edit schema 必须含 patch，实际: {props:?}"
    );
    assert!(
        !props.contains_key("old_string"),
        "hashline 后端 schema 不应有 old_string"
    );
}

#[test]
fn switching_backend_swaps_edit_description_prompt() {
    let sr = build_tools(EditBackend::StringReplace);
    let hl = build_tools(EditBackend::Hashline);

    let (_, sr_desc, _) = find(&sr, "Edit");
    let (_, hl_desc, _) = find(&hl, "Edit");

    assert_ne!(sr_desc, hl_desc, "两种后端的 Edit description 必须不同");
    assert!(
        sr_desc.contains("old_string"),
        "string-replace description 应提及 old_string"
    );
    assert!(
        hl_desc.contains("Hashline") || hl_desc.contains("hashline") || hl_desc.contains("¶"),
        "hashline description 应是 prompt.md 内容（含 Hashline 关键词或 ¶ 符号）"
    );
}

#[test]
fn switching_backend_swaps_read_output_format_description() {
    let sr = build_tools(EditBackend::StringReplace);
    let hl = build_tools(EditBackend::Hashline);

    let (_, sr_desc, _) = find(&sr, "Read");
    let (_, hl_desc, _) = find(&hl, "Read");

    assert_ne!(sr_desc, hl_desc, "两种后端的 Read description 必须不同");
    assert!(
        hl_desc.contains("hashline") || hl_desc.contains("¶") || hl_desc.contains("HASH"),
        "hashline Read description 应说明新输出格式: {hl_desc}"
    );
}

#[test]
fn other_tools_remain_identical_across_backends() {
    let sr = build_tools(EditBackend::StringReplace);
    let hl = build_tools(EditBackend::Hashline);

    // Bash / Grep / TodoWrite 等不应受 edit_backend 影响
    for name in ["Bash", "Grep", "TodoWrite", "WebSearch"] {
        let (_, sr_desc, sr_schema) = find(&sr, name);
        let (_, hl_desc, hl_schema) = find(&hl, name);
        assert_eq!(
            sr_desc, hl_desc,
            "{name} description 不应被 edit_backend 影响"
        );
        assert_eq!(
            sr_schema, hl_schema,
            "{name} schema 不应被 edit_backend 影响"
        );
    }
}

#[test]
fn tool_count_identical_across_backends() {
    let sr = build_tools(EditBackend::StringReplace);
    let hl = build_tools(EditBackend::Hashline);
    assert_eq!(
        sr.len(),
        hl.len(),
        "两个后端的工具总数必须相同：仅 Read+Edit 实现切换，其他工具不变"
    );
}

/// 用 `--nocapture` 运行查看两种后端下 Edit / Read 的注册产物对比：
///   cargo test -p agent-core --test edit_backend_switch dump_for_humans -- --nocapture
#[test]
fn dump_for_humans() {
    for backend in [EditBackend::StringReplace, EditBackend::Hashline] {
        let label = match backend {
            EditBackend::StringReplace => "string-replace",
            EditBackend::Hashline => "hashline",
        };
        let tools = build_tools(backend);
        let (_, edit_desc, edit_schema) = find(&tools, "Edit");
        let (_, read_desc, _) = find(&tools, "Read");

        println!("\n=========== [{label}] Edit schema ===========");
        println!("{}", serde_json::to_string_pretty(edit_schema).unwrap());
        println!("\n=========== [{label}] Edit description (first 400 chars) ===========");
        println!("{}", edit_desc.chars().take(400).collect::<String>());
        println!("\n=========== [{label}] Read description (first 400 chars) ===========");
        println!("{}", read_desc.chars().take(400).collect::<String>());
    }
}
