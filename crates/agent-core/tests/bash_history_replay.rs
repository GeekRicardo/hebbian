//! 真实对话 Bash 历史的代表性命令回归：验证只读命令（含多行脚本）不被误判
//! Destructive 反复审批，同时确认会写/危险结构仍正确判审批。
//!
//! 样本取自 ~/.hebbian/sessions 真实 model_io.jsonl（session 202606230807-76761e74
//! 等）回放统计——691 条唯一命令中误伤为 0，本测试固化其中的关键形态。

use agent_core::effects::{analyze_effects, EffectClass};
use serde_json::json;

fn class_of(cmd: &str) -> (EffectClass, Vec<String>) {
    let e = analyze_effects("Bash", &json!({ "command": cmd }));
    (e.class, e.dangerous_kinds)
}

#[test]
fn multiline_readonly_scripts_not_destructive() {
    // 多行纯只读脚本——此前被换行误判 ast-too-complex 反复审批，现应放行。
    let readonly_scripts = [
        "echo \"=== changelog 末尾 ===\"\ntail -22 docs/changelog.md\nls foo 2>/dev/null",
        "echo a\necho b\ncat README.md",
        "grep -rn pattern src\nwc -l src/*.rs\nhead -5 Cargo.toml",
        "ls -la\npwd\ngit status",
    ];
    for cmd in readonly_scripts {
        let (class, kinds) = class_of(cmd);
        assert!(
            matches!(class, EffectClass::ReadOnly),
            "多行只读脚本应放行，实际 {class:?} kinds={kinds:?}\ncmd={cmd:?}"
        );
    }
}

#[test]
fn writing_and_dangerous_scripts_still_need_approval() {
    // 会写 / 危险结构仍必须审批——安全性不因换行修复而降级。
    let need_approval = [
        // 含真实写段
        ("echo a\ncat x > out.txt", "重定向写"),
        ("ls\nrm -rf build", "rm 删除"),
        ("pwd\nmkdir newdir", "mkdir 写"),
        // 命令替换 / 子 shell：换行修复不放宽这些
        ("ls\nD=$(ls -dt foo | head -1); echo $D", "命令替换"),
        ("echo a\ncurl https://example.com", "非白名单 curl"),
    ];
    for (cmd, why) in need_approval {
        let (class, kinds) = class_of(cmd);
        assert!(
            matches!(class, EffectClass::Destructive),
            "{why} 应审批，实际 {class:?} kinds={kinds:?}\ncmd={cmd:?}"
        );
    }
}
