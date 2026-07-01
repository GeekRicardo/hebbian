//! 工具调用 XML 漏进正文的检测与清洗（架构 §4.3.3）。
//!
//! 长会话、工具密集时，模型偶发把工具调用「幻觉」成纯文本：本该走结构化
//! tool_use 通道的 `<invoke name="...">...<parameter>...` 整块漏进 assistant
//! 正文 content，且常带一个游离的 `call` / `court` 前导 token、标签还可能残缺。
//! 此时上游解析出 0 个 tool_call，agent_loop 退化成普通回答收尾。
//!
//! 真正的危害是**自我强化**：这段裸 XML 一旦回喂给下一轮模型，模型会把它当成
//! 「上一轮工具就是这么调的」范例继续模仿，雪球越滚越大。根治只需保证**发给模型
//! 的请求**里不含这种残骸——展示层留脏无所谓。
//!
//! 因此本模块是一个纯函数，挂在两处「文本将要进入 transcript」的入口：
//! 1. agent_loop 的 `Done` 分支——push 前清洗，命中则注入纠错提示并续跑
//! 2. `Transcript::from_session` 加载历史——兜底清洗重启续聊读回的脏样本

use std::sync::LazyLock;

use regex::Regex;

/// 匹配漏出残骸的**起点**：（可选游离前导 `call` / `court` token + ）`<invoke ...>` /
/// `<function_calls>` 开标签。只定位起点、不吃整块——因为这种残骸是「模型切到工具
/// 模式」的产物，一旦出现，其后内容全属于这次失败的工具调用尝试（含嵌套 wrapper、
/// issue 里常见的重复块、被 max_tokens 截断的残缺标签），从起点截断到结尾即可一并清掉，
/// 比逐块匹配闭合标签稳健得多。
///
/// - `(?is)`：`.` 跨行 + 大小写不敏感
/// - 前导 `call` / `court` 仅在紧贴开标签（中间只允许空白 / 单个换行）时才一并吞掉，
///   避免误伤正文里恰好出现的这两个词
static LEAK_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)\n?[ \t]*(?:call|court)?[ \t]*\n?[ \t]*<\s*(?:function_calls|invoke)\b")
        .expect("tool-xml-leak 正则必然合法")
});

/// 检测文本里是否含工具调用 XML 残骸。
pub fn has_tool_xml_leak(text: &str) -> bool {
    LEAK_START.is_match(text)
}

/// 清洗结果。`detected` 为 true 时 `text` 是已抠掉残骸的版本。
pub struct Sanitized {
    pub text: String,
    pub detected: bool,
}

/// 从首个残骸起点截断到结尾，返回清洗后的文本与是否命中。未命中时原样返回。
pub fn sanitize_tool_xml_leak(text: &str) -> Sanitized {
    match LEAK_START.find(text) {
        None => Sanitized {
            text: text.to_string(),
            detected: false,
        },
        Some(m) => Sanitized {
            text: text[..m.start()].trim_end().to_string(),
            detected: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 来自真实 session 202606160757-eeb33d38 第 211 条：游离 `court` + 残缺 `<invoke>`。
    const REAL_LEAK: &str = "现在调度器：周期性运行优化。\n\ncourt\n<invoke name=\"Edit\">\n<parameter name=\"file_path\">/tmp/a.ts</parameter>\n<parameter name=\"old_string\"></parameter>\n</invoke>";

    #[test]
    fn detects_real_world_leak() {
        assert!(has_tool_xml_leak(REAL_LEAK));
        let out = sanitize_tool_xml_leak(REAL_LEAK);
        assert!(out.detected);
        assert_eq!(out.text, "现在调度器：周期性运行优化。");
        assert!(!out.text.contains("<invoke"));
        assert!(!out.text.contains("court"));
    }

    #[test]
    fn detects_function_calls_wrapper() {
        let s = "好的。\n<function_calls>\n<invoke name=\"Bash\">\n<parameter name=\"command\">ls</parameter>\n</invoke>\n</function_calls>";
        let out = sanitize_tool_xml_leak(s);
        assert!(out.detected);
        assert_eq!(out.text, "好的。");
    }

    #[test]
    fn detects_truncated_unclosed_tag() {
        // 标签被 max_tokens 截断，没有闭合——也要识别并抠到结尾。
        let s = "执行命令。\n<invoke name=\"Bash\">\n<parameter name=\"command\">npm ru";
        let out = sanitize_tool_xml_leak(s);
        assert!(out.detected);
        assert_eq!(out.text, "执行命令。");
    }

    #[test]
    fn keeps_clean_text_untouched() {
        let s = "这是一段正常回答，讨论了 invoke 这个动词和 call 这个名词，但没有 XML 标签。";
        let out = sanitize_tool_xml_leak(s);
        assert!(!out.detected);
        assert_eq!(out.text, s);
    }

    #[test]
    fn does_not_eat_standalone_call_word() {
        // 「call」单独出现、后面不跟 XML 开标签，不应被当成残骸前导吞掉。
        let s = "Please call me back later.";
        assert!(!has_tool_xml_leak(s));
    }

    #[test]
    fn truncates_repeated_blocks_from_first_start() {
        // issue #68354：同一块常重复多次。从首个起点截到尾，一并清掉。
        let s = "好的。\ncourt\n<invoke name=\"Bash\">\n<parameter name=\"command\">ls</parameter>\n</invoke>\ncourt\n<invoke name=\"Bash\">\n<parameter name=\"command\">ls</parameter>\n</invoke>";
        let out = sanitize_tool_xml_leak(s);
        assert!(out.detected);
        assert_eq!(out.text, "好的。");
    }
}
