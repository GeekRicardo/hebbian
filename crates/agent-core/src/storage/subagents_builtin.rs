//! 内置 subagent 定义（架构 §4.4.11.12）。
//!
//! 随版本内嵌的 4 个开箱即用 subagent。[`super::subagents::load_for_workdir`] 用它们垫底，
//! 用户磁盘上 `~/.hebbian/subagents/<name>.md` 的同名定义整体覆盖。
//!
//! 放在 storage 层（而非 subagent 运行时模块）的理由：内置定义是「subagent 的一种来源」，
//! 与磁盘来源并列，归属 storage::subagents 的多来源合并职责；storage 自给自足、不反向依赖
//! 上层 subagent 运行时模块（与 §6.1 providers.json 避免反向依赖同一原则）。

use super::subagents::{SubagentDefinition, SubagentPermission, SubagentSource};

/// 内置 subagent 列表。`enabled` 一律 `true`——真实启用状态由 [`super::subagents::load_for_workdir`]
/// 合并两层 settings 后覆写。
pub fn builtin_subagents() -> Vec<SubagentDefinition> {
    vec![
        readonly(
            "explore",
            "只读探索代码库：扫多文件只回结论 + `相对路径:行号`，不回灌整文件。用于「X 在哪实现 / 哪里用了 Y / 这条调用链怎么走」这类要广搜但只要结论的任务。",
            EXPLORE_PROMPT,
        ),
        readonly(
            "plan",
            "方案规划：摸清相关代码后产出 step-by-step 实现计划（涉及文件 / 分步改动 / 风险回滚 / 验证），不写代码。用于「动手前先出落地方案」。",
            PLAN_PROMPT,
        ),
        readonly(
            "code-reviewer",
            "代码审查：审 diff，按严重度报 correctness / 安全 / 风格问题，每条带 `相对路径:行号` 与修复建议。用于「审一下这段改动 / 这个 PR」。",
            REVIEW_PROMPT,
        ),
        SubagentDefinition {
            name: "general-purpose".to_string(),
            description:
                "通用执行兜底：复杂多步任务，全工具自主完成并自验。用于父拿不准派哪个专才、或任务跨「搜索 + 编辑 + 验证」多阶段时。"
                    .to_string(),
            tools: None, // 全工具（build_child_registry 仍剔除 Task / Memory）
            model: None,
            max_iterations: None,
            system_prompt: GENERAL_PROMPT.to_string(),
            enabled: true,
            source: SubagentSource::Builtin,
            // 全工具自主执行：配 Bypass 让它在白名单内不弹审批打断用户，仅危险红线拦（架构 §4.4.11.4）。
            permission: Some(SubagentPermission::Bypass),
        },
    ]
}

/// 三个只读 agent 共用的构造：白名单 = `Read / Grep / Bash`。剔除 `Edit / Write` 保证不能改文件，
/// 保留 `Bash` 供 `rg / find / git diff`（破坏性命令仍走父 HITL 审批，架构 §4.4.11.9）。
fn readonly(name: &str, description: &str, system_prompt: &str) -> SubagentDefinition {
    SubagentDefinition {
        name: name.to_string(),
        description: description.to_string(),
        tools: Some(vec![
            "Read".to_string(),
            "Grep".to_string(),
            "Bash".to_string(),
        ]),
        model: None,
        max_iterations: None,
        system_prompt: system_prompt.to_string(),
        enabled: true,
        source: SubagentSource::Builtin,
        // 只读 agent：permission=None(Inherit)，跟父 RunMode；只读工具本就不弹审批（架构 §4.4.11.4）。
        permission: None,
    }
}

const EXPLORE_PROMPT: &str = r#"你是一个只读代码探索 agent。父 agent 把「在代码库里找东西、搞清楚某段逻辑」的活委托给你，你的产出会直接回灌父 agent 的上下文——所以只回结论，不要把整文件内容贴回去。

工作方式：
1. 先广后深：先用 Grep / Bash（rg、find、ls、git grep）扫出候选位置，再 Read 关键片段确认。
2. 结论先行：开头一句话回答父 agent 的问题，再列证据。
3. 证据用「相对路径:行号」指位，配一两行关键代码摘录，不要整段或整文件粘贴。
4. 找不到就明说「未找到 X」，并说明你找过哪些地方——不要编造路径或行号。

约束：
- 你不能改文件（没有 Edit / Write 工具）。只读探索，看到问题如实报告，不要尝试修。
- Bash 只用于搜索和浏览，不要用它写文件或跑有副作用的命令。"#;

const PLAN_PROMPT: &str = r#"你是一个方案规划 agent。父 agent 把「动手前先出个落地方案」的活委托给你。你不写代码、不改文件，只产出一份可执行的实现计划。

先用 Read / Grep / Bash 把相关代码摸清楚（涉及哪些文件、现有结构、约束），再产出计划，包含：
1. 目标拆解：把要做的事拆成几个明确子目标。
2. 涉及文件：列出要改或新增的文件，每个一句话说明改什么。
3. 分步改动：按依赖顺序排的步骤，每步可独立验证。
4. 风险与回滚：哪里容易出错、怎么回退。
5. 验证方式：每步及整体怎么确认做对了（跑什么测试 / 命令）。

约束：
- 只读，不改文件（没有 Edit / Write）。
- 计划要具体到「改哪个文件的哪个函数」，不要泛泛而谈。
- 如果需求有歧义或有多条实现路径，列出来让父 agent 定夺，不要替它默默选一个。"#;

const REVIEW_PROMPT: &str = r#"你是一个严格的代码审查 agent。父 agent 把「审一下这段改动、这个 PR」的活委托给你。

先看清楚要审什么：用 git diff（或 git diff --staged、git log）拿到改动，用 Read / Grep 看上下文。

按优先级审：
1. Correctness（最高）：逻辑错误、边界条件、空值与错误处理、并发问题、会不会崩。
2. 安全：注入、越权、敏感信息泄露、不安全的默认值。
3. 风格与可维护性：与项目既有约定不一致、命名、重复、死代码。

每条问题给：
- 严重度：blocker（必须改）/ major（应该改）/ minor（可选）
- 位置：「相对路径:行号」
- 问题是什么，以及可执行的修复建议（不要只说「这里不好」）

约束：
- 只读，不改文件（没有 Edit / Write）。你的职责是指出问题，不是动手修。
- 没问题就直说「未发现某类问题」，不要为凑数编问题。
- 区分「确定的 bug」和「不确定但值得看一眼」，后者明确标注。"#;

const GENERAL_PROMPT: &str = r#"你是一个通用任务 agent。父 agent 把一个界定清楚的子任务整个交给你，你有完整工具集（除了再派子任务和记忆工具），自主把它做完。

工作方式：
- 把任务拆成步骤，用合适的工具一步步推进（搜索、读、改、跑验证）。
- 改完代码要自己验证（编译、测试、跑一下），不要改完就说完事。
- 遇到不可逆或有破坏性的操作，按当前会话的审批策略走（会弹给用户确认）。

结束时给父 agent 一段结论摘要：做了什么、改了哪些文件、验证结果、有没有留下未完成项。父 agent 只看你这段摘要来决定下一步，所以要说清楚。"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_four_with_expected_names() {
        let b = builtin_subagents();
        let names: Vec<&str> = b.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["explore", "plan", "code-reviewer", "general-purpose"]
        );
    }

    #[test]
    fn readonly_agents_exclude_edit_and_write() {
        for d in builtin_subagents() {
            if d.name == "general-purpose" {
                continue;
            }
            let tools = d.tools.expect("只读 agent 有工具白名单");
            assert!(
                !tools.iter().any(|t| t == "Edit" || t == "Write"),
                "{} 不应允许 Edit / Write",
                d.name
            );
            assert!(tools.iter().any(|t| t == "Read"), "{} 应能 Read", d.name);
        }
    }

    #[test]
    fn general_purpose_uses_full_toolset() {
        let gp = builtin_subagents()
            .into_iter()
            .find(|d| d.name == "general-purpose")
            .unwrap();
        assert!(gp.tools.is_none(), "general-purpose 用全工具集");
    }

    #[test]
    fn every_builtin_has_description_and_prompt() {
        for d in builtin_subagents() {
            assert!(!d.description.is_empty(), "{} 缺 description", d.name);
            assert!(!d.system_prompt.is_empty(), "{} 缺 system_prompt", d.name);
        }
    }
}
