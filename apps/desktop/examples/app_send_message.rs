use std::{
    env,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
};

use async_trait::async_trait;
use hebbian_lib::chat::{send_and_save_in_data_dir_with_client_factory, SendArgs};
use model_gateway::{
    client::{DynModelClient, ModelClient},
    config::{self, AuthMode, Provider, ProviderKind},
    protocols::openai,
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use platform::{attachments::MessageAttachment, storage::sessions, CancelFlag};
use serde_json::{json, Value};

#[derive(Debug)]
struct Cli {
    data_dir: PathBuf,
    session_id: Option<String>,
    message: String,
    stream: bool,
    enabled_tools: Vec<String>,
}

#[derive(Debug, Clone)]
struct CapturedRequest {
    provider_id: String,
    provider_kind: ProviderKind,
    auth_mode: AuthMode,
    stream: bool,
    body: Value,
}

struct InspectingClient {
    provider: Provider,
    model: String,
    inner: DynModelClient,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl InspectingClient {
    fn new(
        provider: Provider,
        model: String,
        inner: DynModelClient,
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
    ) -> Self {
        Self {
            provider,
            model,
            inner,
            captured,
        }
    }

    fn patch_and_capture(&self, mut req: ModelRequest, stream: bool) -> ModelRequest {
        req.model = self.model.clone();
        let body = match self.provider.kind {
            ProviderKind::Openai if matches!(self.provider.auth_mode, AuthMode::OauthCodex) => {
                openai::build_responses_body(&req, stream, true)
            }
            ProviderKind::Openai => openai::build_body(&req, stream),
            _ => json!({
                "diagnostic": "request body capture is implemented only for OpenAI providers",
                "model": req.model,
                "entries": req.entries.len(),
                "tools": req.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
                "stream": stream
            }),
        };
        self.captured.lock().unwrap().push(CapturedRequest {
            provider_id: self.provider.id.clone(),
            provider_kind: self.provider.kind,
            auth_mode: self.provider.auth_mode,
            stream,
            body,
        });
        req
    }
}

#[async_trait]
impl ModelClient for InspectingClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        self.inner
            .complete(self.patch_and_capture(req, false), cancel)
            .await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        self.inner
            .stream(self.patch_and_capture(req, true), cancel, on_event)
            .await
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tauri::async_runtime::block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli()?;
    let session_id = resolve_session_id(&cli.data_dir, cli.session_id.as_deref())?;
    let session = sessions::load(&cli.data_dir, &session_id)?;
    let provider = config::get(&cli.data_dir, &session.provider_id)?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));

    println!("app-side trigger");
    println!("data_dir: {}", cli.data_dir.display());
    println!("session_id: {session_id}");
    println!("provider_id: {}", provider.id);
    println!("provider_kind: {:?}", provider.kind);
    println!("auth_mode: {:?}", provider.auth_mode);
    println!("model: {}", session.model);
    println!("history_messages_before: {}", session.messages.len());
    println!("stream: {}", cli.stream);
    println!("enabled_tools: {:?}", cli.enabled_tools);
    println!("message: {}", cli.message);

    let captured_for_factory = Arc::clone(&captured);
    let args = SendArgs {
        session_id: session_id.clone(),
        user_content: cli.message.clone(),
        attachments: Vec::<MessageAttachment>::new(),
        stream: cli.stream,
        enabled_tools: cli.enabled_tools.clone(),
        cancel_flag: Arc::new(AtomicBool::new(false)),
        hitl: None,
    };

    let events_for_emit = Arc::clone(&events);
    let result = send_and_save_in_data_dir_with_client_factory(
        &cli.data_dir,
        args,
        move |event| {
            let line = serde_json::to_string(&event).unwrap_or_else(|_| format!("{event:?}"));
            println!("event: {line}");
            events_for_emit.lock().unwrap().push(line);
        },
        move |provider, model| {
            let inner = model_gateway::build_client(provider.clone())
                .map_err(|e| hebbian_lib::AppError::msg(format!("could not create client: {e}")))?;
            Ok(Arc::new(InspectingClient::new(
                provider,
                model,
                inner,
                Arc::clone(&captured_for_factory),
            )) as DynModelClient)
        },
    )
    .await;

    println!("events_seen: {}", events.lock().unwrap().len());
    let captured = captured.lock().unwrap().clone();
    print_captured_requests(&captured);

    match result {
        Ok(message) => {
            println!("assistant_saved: true");
            println!("assistant_id: {}", message.id);
            println!("assistant_chars: {}", message.content.chars().count());
            println!("assistant_preview: {}", truncate(&message.content, 600));
        }
        Err(error) => {
            println!("assistant_saved: error-path");
            println!("harness_error: {error}");
        }
    }

    let saved = sessions::load(&cli.data_dir, &session_id)?;
    println!("history_messages_after: {}", saved.messages.len());
    if let Some(last) = saved.messages.last() {
        println!("last_saved_role: {:?}", last.role);
        println!("last_saved_meta: {:?}", last.meta);
        println!("last_saved_preview: {}", truncate(&last.content, 600));
    }

    Ok(())
}

fn parse_cli() -> Result<Cli, Box<dyn std::error::Error>> {
    let mut data_dir = env::var_os("HEBBIAN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_data_dir);
    let mut session_id = env::var("HEBBIAN_SESSION_ID").ok();
    let mut message = "Search today's weather in Nanshan, Shenzhen.".to_string();
    let mut stream = true;
    let mut enabled_tools = vec!["web_search".to_string(), "web_fetch".to_string()];

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--data-dir" => data_dir = PathBuf::from(required_value(&mut args, "--data-dir")?),
            "--session-id" => session_id = Some(required_value(&mut args, "--session-id")?),
            "--message" => message = required_value(&mut args, "--message")?,
            "--stream" => stream = true,
            "--no-stream" => stream = false,
            "--no-tools" => enabled_tools.clear(),
            "--enabled-tools" | "--tools" => {
                enabled_tools = split_tools(&required_value(&mut args, "--enabled-tools")?)
            }
            other if other.starts_with("--data-dir=") => {
                data_dir = PathBuf::from(value_after_equals(other, "--data-dir="));
            }
            other if other.starts_with("--session-id=") => {
                session_id = Some(value_after_equals(other, "--session-id=").to_string());
            }
            other if other.starts_with("--message=") => {
                message = value_after_equals(other, "--message=").to_string();
            }
            other if other.starts_with("--enabled-tools=") => {
                enabled_tools = split_tools(value_after_equals(other, "--enabled-tools="));
            }
            other if other.starts_with("--tools=") => {
                enabled_tools = split_tools(value_after_equals(other, "--tools="));
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    Ok(Cli {
        data_dir,
        session_id,
        message,
        stream,
        enabled_tools,
    })
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("dev.ricardo.hebbian")
}

fn required_value(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value").into())
}

fn value_after_equals<'a>(arg: &'a str, prefix: &str) -> &'a str {
    arg.strip_prefix(prefix).unwrap_or_default()
}

fn split_tools(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn print_help() {
    println!(
        "Usage: cargo run -p hebbian --example app_send_message -- [options]\n\
         \n\
         Options:\n\
           --data-dir <path>       App data dir. Defaults to HEBBIAN_DATA_DIR or platform data dir.\n\
           --session-id <id>       Session to mutate. Defaults to HEBBIAN_SESSION_ID or latest session.\n\
           --message <text>        User message to append and send.\n\
           --enabled-tools <csv>   Enabled tool names. Default: web_search,web_fetch.\n\
           --no-tools              Send without tools.\n\
           --no-stream             Use non-streaming model call.\n"
    );
}

fn resolve_session_id(
    data_dir: &Path,
    explicit: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(id) = explicit {
        return Ok(id.to_string());
    }
    let latest = sessions::list(data_dir)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no sessions found under {}", data_dir.display()))?;
    Ok(latest.id)
}

fn print_captured_requests(requests: &[CapturedRequest]) {
    println!("captured_requests: {}", requests.len());
    for (request_index, request) in requests.iter().enumerate() {
        println!(
            "request[{request_index}].provider_id: {}",
            request.provider_id
        );
        println!(
            "request[{request_index}].provider_kind: {:?}",
            request.provider_kind
        );
        println!(
            "request[{request_index}].auth_mode: {:?}",
            request.auth_mode
        );
        println!("request[{request_index}].stream: {}", request.stream);
        if let Some(input) = request.body.get("input").and_then(Value::as_array) {
            println!("request[{request_index}].shape: responses");
            for (input_index, item) in input.iter().enumerate() {
                print_responses_input_item(request_index, input_index, item);
            }
            continue;
        }
        if let Some(messages) = request.body.get("messages").and_then(Value::as_array) {
            println!("request[{request_index}].shape: chat_completions");
            for (message_index, item) in messages.iter().enumerate() {
                print_chat_message_item(request_index, message_index, item);
            }
            continue;
        }
        println!(
            "request[{request_index}].body: {}",
            one_line_json(&request.body)
        );
    }
}

fn print_responses_input_item(request_index: usize, input_index: usize, item: &Value) {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let name_valid = !name.trim().is_empty() && name != "<missing>";
    let summary = json!({
        "request": request_index,
        "input": input_index,
        "type": item_type,
        "role": item.get("role").and_then(Value::as_str),
        "call_id": item.get("call_id").and_then(Value::as_str),
        "has_name_key": item.get("name").is_some(),
        "name": name,
        "name_valid": name_valid,
        "output_chars": item.get("output").and_then(Value::as_str).map(|s| s.chars().count()),
        "raw": printable_json(item),
    });
    println!(
        "request[{request_index}].input[{input_index}]: {}",
        serde_json::to_string_pretty(&summary).unwrap()
    );
    if matches!(item_type, "function_call" | "function_call_output") && !name_valid {
        println!("missing_name_candidate: request[{request_index}].input[{input_index}]");
    }
}

fn print_chat_message_item(request_index: usize, message_index: usize, item: &Value) {
    let summary = json!({
        "request": request_index,
        "message": message_index,
        "role": item.get("role").and_then(Value::as_str),
        "tool_call_id": item.get("tool_call_id").and_then(Value::as_str),
        "tool_calls": item.get("tool_calls").and_then(Value::as_array).map(Vec::len),
        "raw": printable_json(item),
    });
    println!(
        "request[{request_index}].messages[{message_index}]: {}",
        serde_json::to_string_pretty(&summary).unwrap()
    );
}

fn printable_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), printable_json_field(key, value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(printable_json).collect()),
        Value::String(text) => Value::String(truncate(text, 500)),
        _ => value.clone(),
    }
}

fn printable_json_field(key: &str, value: &Value) -> Value {
    match (key, value) {
        ("output" | "arguments" | "text" | "content", Value::String(text)) => {
            Value::String(truncate(text, 500))
        }
        _ => printable_json(value),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

fn one_line_json(value: &Value) -> String {
    serde_json::to_string(&printable_json(value)).unwrap_or_else(|_| "<json error>".to_string())
}
