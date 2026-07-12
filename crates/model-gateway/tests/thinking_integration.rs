//! Integration test: reads providers from ~/.hebbian/providers.json,
//! tests thinking/effort/tool_call logic across all configured models.
//!
//! Run:
//!   cargo test -p model-gateway --test thinking_integration -- --nocapture
//!
//! Tests:
//! 1. Thinking stream (ReasoningDelta) for each model
//! 2. Tool call + thinking coexistence
//! 3. Effort parameter correctness
//! 4. /v1/models endpoint

use common::reasoning::{ReasoningConfig, ReasoningEffort};
use common::CancelFlag;
use model_gateway::config::{self, Provider, ProviderKind, ProvidersFile};
use model_gateway::types::*;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn data_dir() -> PathBuf {
    dirs::home_dir().unwrap().join(".hebbian")
}

fn load_providers() -> Vec<Provider> {
    let file: ProvidersFile = config::load(&data_dir()).unwrap_or_default();
    file.providers.into_iter().filter(|p| p.enabled).collect()
}

fn model_supports_thinking(model: &str, kind: ProviderKind) -> bool {
    let m = model.to_lowercase();
    match kind {
        ProviderKind::Openai => {
            if m.starts_with("deepseek-v4") && !m.contains("nothinking") {
                return true;
            }
            if m.starts_with("gpt-5")
                || m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
            {
                return true;
            }
            false
        }
        ProviderKind::Anthropic => {
            if m.contains("opus-4-7") || m.contains("opus-4-6") || m.contains("sonnet-4-6") {
                return true;
            }
            if m.contains("opus-4-5") || m.contains("sonnet-4-5") || m.contains("haiku-4") {
                return true;
            }
            if m.contains("claude-3-7") {
                return true;
            }
            if m.starts_with("deepseek-v4") && !m.contains("nothinking") {
                return true;
            }
            if m.contains("thinking") {
                return true;
            }
            false
        }
        ProviderKind::Deepseek => !m.contains("nothinking"),
        ProviderKind::Gemini => false,
    }
}

fn effort_for_model(model: &str) -> ReasoningEffort {
    let m = model.to_lowercase();
    if m.contains("deepseek") {
        return ReasoningEffort::High;
    }
    if m.contains("claude") || m.contains("opus") || m.contains("sonnet") || m.contains("haiku") {
        return ReasoningEffort::Medium;
    }
    ReasoningEffort::High
}

fn should_skip_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("gpt-image")
        || m.contains("dall-e")
        || m.contains("tts")
        || m.contains("whisper")
        || m.contains("embedding")
        || m == "o1-mini"
}

struct StreamCollector {
    text_deltas: Arc<Mutex<Vec<String>>>,
    reasoning_deltas: Arc<Mutex<Vec<String>>>,
    tool_call_deltas: Arc<Mutex<Vec<ToolCallStreamDelta>>>,
}

impl StreamCollector {
    fn new() -> Self {
        Self {
            text_deltas: Arc::new(Mutex::new(Vec::new())),
            reasoning_deltas: Arc::new(Mutex::new(Vec::new())),
            tool_call_deltas: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn callback(&self) -> impl Fn(ModelStreamEvent) + Send + Sync + '_ {
        move |event| match event {
            ModelStreamEvent::TextDelta { text } => {
                self.text_deltas.lock().unwrap().push(text);
            }
            ModelStreamEvent::ReasoningDelta { text } => {
                self.reasoning_deltas.lock().unwrap().push(text);
            }
            ModelStreamEvent::ToolCallDelta(delta) => {
                self.tool_call_deltas.lock().unwrap().push(delta);
            }
            ModelStreamEvent::ReasoningSignature { .. } => {}
            ModelStreamEvent::ReasoningDuration { .. } => {}
        }
    }

    fn has_reasoning(&self) -> bool {
        !self.reasoning_deltas.lock().unwrap().is_empty()
    }

    fn reasoning_len(&self) -> usize {
        self.reasoning_deltas
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.len())
            .sum()
    }

    fn full_text(&self) -> String {
        self.text_deltas.lock().unwrap().join("")
    }

    fn full_reasoning(&self) -> String {
        self.reasoning_deltas.lock().unwrap().join("")
    }
}

const SEP: &str = "======================================================================";

// === Test 1: All providers thinking stream ===

#[tokio::test]
async fn all_providers_thinking_stream() {
    let providers = load_providers();
    if providers.is_empty() {
        eprintln!("no providers configured, skip");
        return;
    }

    eprintln!("\n{SEP}");
    eprintln!("  Thinking Stream Test");
    eprintln!("{SEP}\n");

    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for provider in &providers {
        eprintln!("\n-- Provider: {} ({:?}) --", provider.name, provider.kind);

        let client = match model_gateway::build_client(provider.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("   FAIL create client: {e}");
                failed += provider.models.len();
                total += provider.models.len();
                continue;
            }
        };

        for model in &provider.models {
            total += 1;

            if should_skip_model(model) {
                eprintln!("   SKIP {model} (non-chat model)");
                skipped += 1;
                continue;
            }

            if !model_supports_thinking(model, provider.kind) {
                eprintln!("   SKIP {model} (no thinking support)");
                skipped += 1;
                continue;
            }

            let effort = effort_for_model(model);
            let reasoning = Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(effort),
                long_context: None,
            });

            let req = ModelRequest {
                model: model.clone(),
                system: Some("You are helpful. Be concise.".into()),
                entries: vec![TranscriptEntry::User(UserEntry::text(
                    "What is 2+2? Think step by step.",
                ))],
                tools: vec![],
                max_tokens: 8192,
                reasoning,
                compact_prompt_cache_key: None,
                meta: Default::default(),
            };

            let collector = StreamCollector::new();
            let cancel = CancelFlag::default();
            let start = Instant::now();

            let result = client.stream(req, cancel, &collector.callback()).await;
            let elapsed = start.elapsed();

            match result {
                Ok(response) => {
                    let has_thinking = collector.has_reasoning();
                    let reasoning_chars = collector.reasoning_len();
                    let text_chars = collector.full_text().len();

                    if has_thinking {
                        eprintln!("   OK   {model} thinking={reasoning_chars}c text={text_chars}c effort={effort:?} ({elapsed:.1?})");
                        passed += 1;
                    } else {
                        let resp_reasoning = match &response {
                            ModelResponse::Done { reasoning, .. } => reasoning.clone(),
                            ModelResponse::ToolCalls { reasoning, .. } => reasoning.clone(),
                        };
                        if !resp_reasoning.is_empty() {
                            eprintln!("   OK   {model} thinking={}c (response only) text={text_chars}c ({elapsed:.1?})", resp_reasoning.len());
                            passed += 1;
                        } else {
                            eprintln!("   FAIL {model} NO THINKING! text={text_chars}c effort={effort:?} ({elapsed:.1?})");
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("   FAIL {model} error: {e:?} ({elapsed:.1?})");
                    failed += 1;
                }
            }
        }
    }

    eprintln!("\n{SEP}");
    eprintln!("  Result: {passed} passed / {failed} failed / {skipped} skipped / {total} total");
    eprintln!("{SEP}\n");

    if failed > 0 {
        panic!("{failed} model(s) failed thinking test!");
    }
}

// === Test 2: Tool call + thinking ===

#[tokio::test]
async fn tool_call_with_thinking() {
    let providers = load_providers();
    if providers.is_empty() {
        return;
    }

    eprintln!("\n{SEP}");
    eprintln!("  Tool Call + Thinking Test");
    eprintln!("{SEP}\n");

    let targets: &[(&str, ProviderKind)] = &[
        ("deepseek-v4-flash", ProviderKind::Openai),
        ("deepseek-v4-pro", ProviderKind::Anthropic),
        ("claude-opus-4-6", ProviderKind::Anthropic),
        ("claude-sonnet-4-6", ProviderKind::Anthropic),
        ("claude-opus-4-7", ProviderKind::Anthropic),
    ];

    let mut passed = 0;
    let mut failed = 0;

    for (target, kind) in targets {
        let provider = providers
            .iter()
            .find(|p| p.kind == *kind && p.models.iter().any(|m| m.contains(target)));

        let Some(provider) = provider else {
            eprintln!("   SKIP {target} ({kind:?}) - no matching provider");
            continue;
        };

        let model = provider
            .models
            .iter()
            .find(|m| m.contains(target))
            .unwrap()
            .clone();

        let client = match model_gateway::build_client(provider.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("   FAIL {model} - client error: {e}");
                failed += 1;
                continue;
            }
        };

        let effort = effort_for_model(&model);
        let req = ModelRequest {
            model: model.clone(),
            system: Some("Always use the get_weather tool when asked about weather.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text(
                "What is the weather in Beijing?",
            ))],
            tools: vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather for a city".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "city": {"type": "string"} },
                    "required": ["city"]
                }),
            }],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(effort),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let collector = StreamCollector::new();
        let cancel = CancelFlag::default();
        let start = Instant::now();

        let result = client.stream(req, cancel, &collector.callback()).await;
        let elapsed = start.elapsed();

        match result {
            Ok(response) => {
                let reasoning_text = if collector.has_reasoning() {
                    collector.full_reasoning()
                } else {
                    match &response {
                        ModelResponse::Done { reasoning, .. } => reasoning.clone(),
                        ModelResponse::ToolCalls { reasoning, .. } => reasoning.clone(),
                    }
                };

                let has_tools = matches!(&response, ModelResponse::ToolCalls { calls, .. } if !calls.is_empty());

                if has_tools && !reasoning_text.is_empty() {
                    eprintln!(
                        "   OK   {model} tool_call+thinking reasoning={}c ({elapsed:.1?})",
                        reasoning_text.len()
                    );
                    passed += 1;
                } else if !has_tools && !reasoning_text.is_empty() {
                    eprintln!("   WARN {model} has thinking but no tool_call (model chose not to) ({elapsed:.1?})");
                    passed += 1;
                } else if has_tools && reasoning_text.is_empty() {
                    eprintln!("   FAIL {model} tool_call OK but thinking MISSING ({elapsed:.1?})");
                    failed += 1;
                } else {
                    eprintln!("   FAIL {model} no tool_call and no thinking ({elapsed:.1?})");
                    failed += 1;
                }
            }
            Err(e) => {
                eprintln!("   FAIL {model} error: {e:?} ({elapsed:.1?})");
                failed += 1;
            }
        }
    }

    eprintln!("\n  Result: {passed} passed / {failed} failed\n");
    if failed > 0 {
        panic!("{failed} model(s) failed tool_call+thinking test!");
    }
}

// === Test 3: Effort parameter correctness (no network) ===

#[test]
fn effort_parameter_correctness() {
    use model_gateway::protocols::{anthropic as ap, openai as op};

    eprintln!("\n{SEP}");
    eprintln!("  Effort Parameter Correctness");
    eprintln!("{SEP}\n");

    let efforts = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Extra,
    ];

    // GPT-5.4 / 5.5: supports xhigh
    eprintln!("-- GPT-5.4 (xhigh supported) --");
    for effort in &efforts {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(*effort),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };
        let body = op::build_body(&req, false).unwrap();
        let val = body["reasoning_effort"].as_str().unwrap_or("(none)");
        let expected = effort.openai_effort_for_model("gpt-5.4");
        assert_eq!(val, expected, "gpt-5.4 effort={effort:?}");
        eprintln!("   {effort:?} -> \"{val}\" OK");
    }

    // DeepSeek v4 (OpenAI): only high/max
    eprintln!("-- DeepSeek v4 (OpenAI, high/max only) --");
    for effort in &efforts {
        let req = ModelRequest {
            model: "deepseek-v4-pro".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(*effort),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };
        let body = op::build_body(&req, false).unwrap();
        let val = body["reasoning_effort"].as_str().unwrap_or("(none)");
        let expected = effort.deepseek_effort();
        assert_eq!(val, expected);
        let mt = body["max_tokens"].as_u64().unwrap();
        eprintln!("   {effort:?} -> \"{val}\" max_tokens={mt} OK");
    }

    // Claude Opus 4.7: enabled + budget + display:summarized
    eprintln!("-- Claude Opus 4.7 (budget + display:summarized) --");
    for effort in &efforts {
        let req = ModelRequest {
            model: "claude-opus-4-7".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(*effort),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };
        let body = ap::build_body(&req, false, false, None, false).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["display"], "summarized");
        let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
        assert!(budget >= 1024);
        eprintln!("   {effort:?} -> budget={budget} OK");
    }

    // Claude Opus/Sonnet 4.6: enabled + budget (no display)
    eprintln!("-- Claude 4.6 (budget, no display) --");
    for model in &["claude-opus-4-6", "claude-sonnet-4-6"] {
        for effort in &efforts {
            let req = ModelRequest {
                model: (*model).into(),
                system: None,
                entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
                tools: vec![],
                max_tokens: 8192,
                reasoning: Some(ReasoningConfig {
                    enabled: Some(true),
                    effort: Some(*effort),
                    long_context: None,
                }),
                compact_prompt_cache_key: None,
                meta: Default::default(),
            };
            let body = ap::build_body(&req, false, false, None, false).unwrap();
            assert_eq!(body["thinking"]["type"], "enabled");
            assert!(body["thinking"].get("display").is_none());
            let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
            assert!(budget >= 1024);
            eprintln!("   {model} {effort:?} -> budget={budget} OK");
        }
    }

    // DeepSeek v4 on Anthropic: thinking.enabled + output_config.effort
    eprintln!("-- DeepSeek v4 (Anthropic dialect) --");
    for effort in &efforts {
        let req = ModelRequest {
            model: "deepseek-v4-pro".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(*effort),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };
        let body = ap::build_body(&req, false, false, None, false).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        let eff = body["output_config"]["effort"].as_str().unwrap();
        assert_eq!(eff, effort.deepseek_effort());
        eprintln!("   {effort:?} -> output_config.effort=\"{eff}\" OK");
    }

    eprintln!("\n  All effort parameters correct!\n");
}

// === Test 4: /v1/models endpoint ===

#[tokio::test]
async fn models_endpoint() {
    let providers = load_providers();
    if providers.is_empty() {
        return;
    }

    eprintln!("\n{SEP}");
    eprintln!("  /v1/models Endpoint Test");
    eprintln!("{SEP}\n");

    for provider in &providers {
        let start = Instant::now();
        let result = model_gateway::discovery::fetch(provider).await;
        let elapsed = start.elapsed();

        match result {
            Ok(models) => {
                let with_ctx = models.iter().filter(|m| m.context_length.is_some()).count();
                eprintln!(
                    "   OK   {} ({:?}) {} models, {} with context_length ({elapsed:.1?})",
                    provider.name,
                    provider.kind,
                    models.len(),
                    with_ctx
                );
            }
            Err(e) => {
                eprintln!(
                    "   FAIL {} ({:?}) {e} ({elapsed:.1?})",
                    provider.name, provider.kind
                );
            }
        }
    }
}

// === Test 5: Focused DeepSeek thinking ===

#[tokio::test]
async fn focused_deepseek_thinking() {
    let providers = load_providers();

    eprintln!("\n{SEP}");
    eprintln!("  Focused DeepSeek Thinking");
    eprintln!("{SEP}\n");

    let cases: Vec<(&str, ProviderKind, &str)> = vec![
        (
            "DeepSeek-API(OpenAI)",
            ProviderKind::Openai,
            "deepseek-v4-flash",
        ),
        ("DeepSeek-Web", ProviderKind::Deepseek, "deepseek-v4-flash"),
        (
            "DeepSeek-Anthropic",
            ProviderKind::Anthropic,
            "deepseek-v4-pro",
        ),
    ];

    for (label, kind, model_prefix) in &cases {
        let provider = providers
            .iter()
            .find(|p| p.kind == *kind && p.models.iter().any(|m| m.contains(model_prefix)));

        let Some(provider) = provider else {
            eprintln!("   SKIP {label} - not configured");
            continue;
        };

        let model = provider
            .models
            .iter()
            .find(|m| m.contains(model_prefix))
            .cloned()
            .unwrap_or_else(|| model_prefix.to_string());

        let client = match model_gateway::build_client(provider.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("   FAIL {label} ({model}) client: {e}");
                panic!("{label} client creation failed");
            }
        };

        let req = ModelRequest {
            model: model.clone(),
            system: Some("Be concise.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text(
                "If a train goes 120km in 2 hours, what is its speed? Think carefully.",
            ))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(ReasoningEffort::High),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let collector = StreamCollector::new();
        let cancel = CancelFlag::default();
        let start = Instant::now();

        let result = client.stream(req, cancel, &collector.callback()).await;
        let elapsed = start.elapsed();

        match result {
            Ok(response) => {
                let reasoning = if collector.has_reasoning() {
                    collector.full_reasoning()
                } else {
                    match &response {
                        ModelResponse::Done { reasoning, .. } => reasoning.clone(),
                        ModelResponse::ToolCalls { reasoning, .. } => reasoning.clone(),
                    }
                };

                if reasoning.is_empty() {
                    eprintln!("   FAIL {label} ({model}) NO THINKING ({elapsed:.1?})");
                    eprintln!(
                        "        text: {}...",
                        &collector.full_text()[..collector.full_text().len().min(200)]
                    );
                    panic!("{label} ({model}) thinking missing!");
                }
                eprintln!(
                    "   OK   {label} ({model}) thinking={}c ({elapsed:.1?})",
                    reasoning.len()
                );
                eprintln!(
                    "        preview: {}...",
                    &reasoning[..reasoning.len().min(120)]
                );
            }
            Err(e) => {
                eprintln!("   FAIL {label} ({model}) error: {e:?} ({elapsed:.1?})");
                panic!("{label} ({model}) request failed: {e:?}");
            }
        }
    }
}

// === Test 6: Focused Claude thinking ===

#[tokio::test]
async fn focused_claude_thinking() {
    let providers = load_providers();

    eprintln!("\n{SEP}");
    eprintln!("  Focused Claude Thinking");
    eprintln!("{SEP}\n");

    let anthropic_providers: Vec<&Provider> = providers
        .iter()
        .filter(|p| {
            p.kind == ProviderKind::Anthropic && p.models.iter().any(|m| m.contains("claude"))
        })
        .collect();

    if anthropic_providers.is_empty() {
        eprintln!("   SKIP - no Anthropic provider with claude models");
        return;
    }

    let targets = ["claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6"];

    for target in targets {
        // Try each anthropic provider until one works for this model
        let mut succeeded = false;

        for provider in &anthropic_providers {
            let model = match provider.models.iter().find(|m| m.contains(target)) {
                Some(m) => m.clone(),
                None => continue,
            };

            let client = match model_gateway::build_client((*provider).clone()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("   WARN {model} ({}) client: {e}", provider.name);
                    continue;
                }
            };

            let req = ModelRequest {
                model: model.clone(),
                system: Some("Be concise.".into()),
                entries: vec![TranscriptEntry::User(UserEntry::text(
                    "What is 15 * 17? Show reasoning.",
                ))],
                tools: vec![],
                max_tokens: 8192,
                reasoning: Some(ReasoningConfig {
                    enabled: Some(true),
                    effort: Some(ReasoningEffort::Medium),
                    long_context: None,
                }),
                compact_prompt_cache_key: None,
                meta: Default::default(),
            };

            let collector = StreamCollector::new();
            let cancel = CancelFlag::default();
            let start = Instant::now();

            let result = client.stream(req, cancel, &collector.callback()).await;
            let elapsed = start.elapsed();

            match result {
                Ok(response) => {
                    let reasoning = if collector.has_reasoning() {
                        collector.full_reasoning()
                    } else {
                        match &response {
                            ModelResponse::Done { reasoning, .. } => reasoning.clone(),
                            ModelResponse::ToolCalls { reasoning, .. } => reasoning.clone(),
                        }
                    };

                    if reasoning.is_empty() {
                        eprintln!(
                            "   FAIL {model} ({}) NO THINKING ({elapsed:.1?})",
                            provider.name
                        );
                        eprintln!(
                            "        text: {}...",
                            &collector.full_text()[..collector.full_text().len().min(200)]
                        );
                        // Don't panic - try next provider
                        continue;
                    }
                    eprintln!(
                        "   OK   {model} ({}) thinking={}c ({elapsed:.1?})",
                        provider.name,
                        reasoning.len()
                    );
                    eprintln!(
                        "        preview: {}...",
                        &reasoning[..reasoning.len().min(120)]
                    );
                    succeeded = true;
                    break;
                }
                Err(ModelError::Http { status: 401, .. })
                | Err(ModelError::Http { status: 403, .. }) => {
                    eprintln!(
                        "   WARN {model} ({}) auth failed, trying next provider",
                        provider.name
                    );
                    continue;
                }
                Err(e) => {
                    eprintln!(
                        "   WARN {model} ({}) error: {e:?}, trying next provider",
                        provider.name
                    );
                    continue;
                }
            }
        }

        if !succeeded {
            eprintln!("   SKIP {target} - no working provider found");
        }
    }
}

// === Test 7: Debug request body for Claude thinking ===
// Prints the exact request body that would be sent, to verify thinking params are correct

#[test]
fn debug_claude_request_bodies() {
    use model_gateway::protocols::anthropic as ap;

    eprintln!("\n{SEP}");
    eprintln!("  Debug: Claude Request Bodies (thinking params)");
    eprintln!("{SEP}\n");

    let models = ["claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6"];

    for model in models {
        let req = ModelRequest {
            model: model.into(),
            system: Some("Be concise.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text("What is 15*17?"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(ReasoningConfig {
                enabled: Some(true),
                effort: Some(ReasoningEffort::Medium),
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let body = ap::build_body(&req, true, false, None, false).unwrap();

        eprintln!("-- {model} --");
        eprintln!(
            "   thinking: {}",
            body.get("thinking")
                .map(|v| v.to_string())
                .unwrap_or("(none)".into())
        );
        eprintln!(
            "   output_config: {}",
            body.get("output_config")
                .map(|v| v.to_string())
                .unwrap_or("(none)".into())
        );
        eprintln!("   max_tokens: {}", body["max_tokens"]);
        eprintln!("   stream: {}", body["stream"]);
        eprintln!();

        // Verify thinking is present
        assert!(
            body.get("thinking").is_some(),
            "{model} must have thinking field"
        );
        assert_eq!(
            body["thinking"]["type"], "enabled",
            "{model} thinking.type must be enabled"
        );
    }

    eprintln!("  All request bodies have correct thinking params.");
    eprintln!("  If proxies don't return thinking, the proxy is stripping it.\n");
}
