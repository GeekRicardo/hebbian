//! `//goal` 命令的 judge：模型想结束 turn 时，判 transcript 是否满足用户设的完成条件。
//!
//! 架构 §4.8.3 / §8。复用 [`crate::automode`] 的 judge 调用范式，但用会话主 client+主模型。

use std::sync::Arc;

use serde::Deserialize;
use tracing::warn;

use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};

/// goal judge 的 system prompt（编译进二进制，跨会话稳定）。
pub const GOAL_JUDGE_SYSTEM: &str = include_str!("../prompts/goal_judge.md");

/// judge 裁决结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalVerdict {
    /// 条件已满足，附证据。
    Achieved(String),
    /// 条件永远无法满足，附原因。
    Impossible(String),
    /// 尚未满足，附「还差什么」——注入续跑。
    NotYet(String),
}

/// judge 返回的 JSON 形态。
#[derive(Debug, Deserialize)]
struct RawVerdict {
    ok: bool,
    #[serde(default)]
    impossible: bool,
    #[serde(default)]
    reason: String,
}

/// 解析 judge 模型返回的文本为 [`GoalVerdict`]。
/// 解析失败 fail-safe 为 `NotYet`——绝不误判达成，宁可多续跑一轮。
fn parse_verdict(raw: &str) -> GoalVerdict {
    // 容错：从文本里抠出第一个 {...} JSON 片段（judge 可能裹了多余文字）。
    let json_slice = raw.find('{').and_then(|start| {
        raw.rfind('}')
            .filter(|&end| end > start)
            .map(|end| &raw[start..=end])
    });
    let Some(slice) = json_slice else {
        return GoalVerdict::NotYet(format!("judge 返回无法解析：{}", trim(raw, 120)));
    };
    match serde_json::from_str::<RawVerdict>(slice) {
        Ok(v) if v.impossible => GoalVerdict::Impossible(v.reason),
        Ok(v) if v.ok => GoalVerdict::Achieved(v.reason),
        Ok(v) => GoalVerdict::NotYet(v.reason),
        Err(e) => GoalVerdict::NotYet(format!("judge JSON 解析失败：{e}")),
    }
}

/// 调一次模型作为 goal judge（架构 §4.8.3）。
///
/// 用会话主 client + 主模型（与 AutoMode 的专属 judge 不同——goal 裁决质量比成本重要，
/// 且不引入额外配置）。`recent_transcript` 传最近若干轮，judge 据此找完成证据。
pub async fn judge_goal(
    client: &Arc<dyn ModelClient>,
    model: &str,
    condition: &str,
    recent_transcript: &[TranscriptEntry],
    cancel: common::CancelFlag,
    dump: Option<&crate::model_io_dump::ModelIoDump>,
    run_id: &str,
    turn: u32,
) -> GoalVerdict {
    let prompt = format_judge_prompt(condition, recent_transcript);
    let request = ModelRequest {
        model: model.to_string(),
        system: Some(GOAL_JUDGE_SYSTEM.to_string()),
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: Vec::new(),
        max_tokens: 400,
        reasoning: None,
            meta: model_gateway::types::ModelCallMeta {
            tag: model_gateway::types::ModelCallTag::Goal,
            ..Default::default()
        },
    };
    // complete 会消费 request；要 dump 就先快照一份。
    let dump_request = dump.map(|_| request.clone());
    let started = std::time::Instant::now();
    let result = client.complete(request, cancel).await;
    // goal judge 的 LLM 请求记入 model_io.jsonl（kind="judge"，与 AutoMode 判官同标签）。
    if let (Some(dump), Some(req)) = (dump, dump_request) {
        dump.record(crate::model_io_dump::DumpEntry {
            ts: crate::model_io_dump::iso_now(),
            run_id: run_id.to_string(),
            turn,
            model: client.provider_id().to_string(),
            request: crate::model_io_dump::request_to_json(&req, client.provider_id()),
            response: crate::model_io_dump::response_to_json(&result),
            duration_ms: started.elapsed().as_millis() as u64,
            kind: "judge".to_string(),
        });
    }
    match result {
        Ok(resp) => parse_verdict(&extract_text(&resp)),
        // judge 调用本身失败 / 被取消 → fail-safe NotYet（不误判达成，也不熔断）。
        // 真正的 cancel 由主 loop 的 CancelFlag 兜底停止续跑。
        Err(ModelError::Cancelled) => GoalVerdict::NotYet("goal judge 被取消".into()),
        Err(err) => {
            warn!(%err, "goal judge 调用失败，本轮按未达成处理");
            GoalVerdict::NotYet(format!("goal judge 调用失败：{err}"))
        }
    }
}

fn extract_text(resp: &ModelResponse) -> String {
    match resp {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text.clone(),
    }
}

fn format_judge_prompt(condition: &str, recent_transcript: &[TranscriptEntry]) -> String {
    let recent: Vec<String> = recent_transcript
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(summarize_entry)
        .collect();
    format!(
        "完成条件（用户设定）：\n{condition}\n\n\
         对话记录（旧→新）：\n{}\n\n\
         按 system prompt 的格式输出一行 JSON。",
        recent.join("\n")
    )
}

fn summarize_entry(entry: &TranscriptEntry) -> String {
    match entry {
        TranscriptEntry::User(u) => format!("- user: {}", trim(&u.text, 300)),
        TranscriptEntry::Assistant(a) => format!("- assistant: {}", trim(&a.text, 300)),
        TranscriptEntry::ToolResults(results) => {
            let s: Vec<String> = results
                .iter()
                .map(|t| format!("{}={}", t.name, trim(&t.content, 120)))
                .collect();
            format!("- tool_results: {}", s.join(" / "))
        }
    }
}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_achieved() {
        let v = parse_verdict(r#"{"ok": true, "reason": "测试全绿见 tool_result"}"#);
        assert_eq!(v, GoalVerdict::Achieved("测试全绿见 tool_result".into()));
    }

    #[test]
    fn parse_not_yet() {
        let v = parse_verdict(r#"{"ok": false, "reason": "还有 2 个测试失败"}"#);
        assert_eq!(v, GoalVerdict::NotYet("还有 2 个测试失败".into()));
    }

    #[test]
    fn parse_impossible() {
        let v = parse_verdict(r#"{"ok": false, "impossible": true, "reason": "依赖的外部 API 已下线"}"#);
        assert_eq!(v, GoalVerdict::Impossible("依赖的外部 API 已下线".into()));
    }

    #[test]
    fn parse_garbage_falls_back_to_not_yet() {
        assert!(matches!(parse_verdict("我觉得差不多了"), GoalVerdict::NotYet(_)));
        assert!(matches!(parse_verdict(""), GoalVerdict::NotYet(_)));
        // `}` 先于 `{`：切片边界倒置，必须 fail-safe 而非 panic
        assert!(matches!(parse_verdict("} x {"), GoalVerdict::NotYet(_)));
        // JSON 存在但类型不符：解析失败分支
        assert!(matches!(parse_verdict(r#"{"ok": "yes"}"#), GoalVerdict::NotYet(_)));
    }

    #[test]
    fn parse_contradictory_prefers_impossible() {
        // ok 与 impossible 同时为 true（模型抽风）→ 落到更保守的 Impossible，绝不误判达成
        assert!(matches!(
            parse_verdict(r#"{"ok": true, "impossible": true}"#),
            GoalVerdict::Impossible(_)
        ));
    }

    #[test]
    fn parse_json_wrapped_in_prose() {
        let v = parse_verdict("分析后：\n{\"ok\": true, \"reason\": \"done\"}\n以上");
        assert_eq!(v, GoalVerdict::Achieved("done".into()));
    }
}
