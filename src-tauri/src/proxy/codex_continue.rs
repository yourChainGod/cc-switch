//! Codex Responses reasoning continuation/folding for native `/v1/responses` SSE.
//!
//! This module is deliberately self-contained: provider selection, routing and
//! failover still happen through `RequestForwarder::forward_with_retry`.

use super::{
    forwarder::{ActiveConnectionGuard, RequestForwarder},
    hyper_client::ProxyResponse,
    provider_router::ProviderAttempt,
    sse::{append_utf8_safe, strip_sse_field, take_sse_block},
};
use crate::app_config::AppType;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use http::{
    header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING},
    Extensions, HeaderMap, HeaderValue, Method,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const DEFAULT_STEP: u64 = 518;
const DEFAULT_MAX_CONTINUATIONS: usize = 8;
const DEFAULT_MARKER: &str =
    "We need continue thinking. Do not summarize; continue from the previous reasoning state.";
const ENCRYPTED_INCLUDE: &str = "reasoning.encrypted_content";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexContinueConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_max_continuations")]
    pub max_continuations: usize,
    #[serde(default = "default_step")]
    pub step: u64,
    #[serde(default = "default_marker")]
    pub marker: String,
}

impl CodexContinueConfig {
    pub(crate) fn from_settings_with_env(settings: Self) -> Self {
        Self {
            enabled: env_bool_override("CCSWITCH_CODEX_CONTINUE", settings.enabled),
            max_continuations: env_usize_override(
                "CCSWITCH_CODEX_CONTINUE_MAX",
                settings.max_continuations,
            ),
            step: env_u64_override("CCSWITCH_CODEX_CONTINUE_STEP", settings.step).max(3),
            marker: std::env::var("CCSWITCH_CODEX_CONTINUE_MARKER")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(settings.marker),
        }
    }

    pub(crate) fn from_env() -> Self {
        Self::from_settings_with_env(Self::default())
    }
}

impl Default for CodexContinueConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            max_continuations: default_max_continuations(),
            step: default_step(),
            marker: default_marker(),
        }
    }
}

fn default_enabled() -> bool {
    false
}

fn default_max_continuations() -> usize {
    DEFAULT_MAX_CONTINUATIONS
}

fn default_step() -> u64 {
    DEFAULT_STEP
}

fn default_marker() -> String {
    DEFAULT_MARKER.to_string()
}

fn env_bool_override(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" => false,
            "1" | "true" | "on" | "yes" => true,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_usize_override(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u64_override(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// Request-shape gate (config-independent, zero-cost): only streaming requests
/// that carry a reasoning object AND explicit `store: false` are eligible.
///
/// - `reasoning` must be an object (`{"effort": ...}`): the field is never a
///   boolean in real Responses traffic, and non-reasoning requests must not be
///   folded (they gain nothing and would only pay the include injection).
/// - `store: false` is required because `include: reasoning.encrypted_content`
///   is rejected by OpenAI when store is enabled, and continuation replay only
///   works for full-input-replay clients (Codex CLI style). `previous_response_id`
///   server-state clients (store=true) are excluded by construction.
pub(crate) fn request_shape_eligible(body: &Value) -> bool {
    body.get("stream").and_then(Value::as_bool).unwrap_or(false)
        && matches!(body.get("reasoning"), Some(Value::Object(_)))
        && matches!(body.get("store"), Some(Value::Bool(false)))
}

/// Request gate: only native streaming Responses requests with reasoning enabled.
///
/// Provider/endpoint gates are handled by `handlers.rs`: this must run only in
/// the native Responses branch, never in Chat conversion or compact.
pub(crate) fn should_enable_for_request(body: &Value, config: &CodexContinueConfig) -> bool {
    config.enabled && request_shape_eligible(body)
}

pub(crate) fn is_truncation_pattern(reasoning_tokens: Option<u64>, step: u64) -> bool {
    let Some(tokens) = reasoning_tokens else {
        return false;
    };
    let step = step.max(3);
    // 等价于 (tokens + 2) % step == 0，改写为取模比较以避免 tokens 接近
    // u64::MAX 时的加法溢出（usage 字段来自上游，不可信）。
    tokens >= step - 2 && tokens % step == step - 2
}

/// 宽容的 u64 提取：部分 OpenAI 兼容中转把 usage 数字序列化成浮点
/// （如 516.0），`Value::as_u64` 会返回 None；这里额外接受非负整数值浮点。
fn value_as_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f <= u64::MAX as f64)
            .map(|f| f as u64)
    })
}

fn reasoning_tokens(usage: Option<&Value>) -> Option<u64> {
    value_as_u64(
        usage?
            .get("output_tokens_details")?
            .get("reasoning_tokens")?,
    )
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SseFrame {
    Event(Value),
    Done,
}

#[derive(Default, Debug)]
pub(crate) struct IncrementalSseParser {
    buffer: String,
    utf8_remainder: Vec<u8>,
}

impl IncrementalSseParser {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        if bytes.is_empty() {
            return Vec::new();
        }

        append_utf8_safe(&mut self.buffer, &mut self.utf8_remainder, bytes);
        let mut out = Vec::new();
        while let Some(block) = take_sse_block(&mut self.buffer) {
            if let Some(frame) = parse_sse_block(&block) {
                out.push(frame);
            }
        }
        out
    }

    pub(crate) fn finish(&mut self) -> Vec<SseFrame> {
        if !self.utf8_remainder.is_empty() {
            self.buffer
                .push_str(&String::from_utf8_lossy(&self.utf8_remainder));
            self.utf8_remainder.clear();
        }
        let trailing = std::mem::take(&mut self.buffer);
        parse_sse_block(&trailing).into_iter().collect()
    }
}

fn parse_sse_block(block: &str) -> Option<SseFrame> {
    let mut data_lines = Vec::new();
    for line in block.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = strip_sse_field(line.trim_end_matches('\r'), "data") {
            data_lines.push(data.to_string());
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    let payload = data_lines.join("\n");
    if payload.trim() == "[DONE]" {
        return Some(SseFrame::Done);
    }

    serde_json::from_str::<Value>(&payload)
        .ok()
        .map(SseFrame::Event)
}

fn sse_event(event: &Value) -> Bytes {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    Bytes::from(format!("event: {event_type}\ndata: {data}\n\n"))
}

fn sse_done() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

fn set_sequence(event: &mut Value, seq: &mut u64) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert("sequence_number".to_string(), json!(*seq));
        *seq += 1;
    }
}

fn next_sequence(seq: &mut u64) -> u64 {
    let current = *seq;
    *seq += 1;
    current
}

fn set_output_index(event: &mut Value, output_index: usize) {
    if let Some(obj) = event.as_object_mut() {
        if obj.contains_key("output_index") {
            obj.insert("output_index".to_string(), json!(output_index));
        }
    }
}

fn event_type(event: &Value) -> &str {
    event.get("type").and_then(Value::as_str).unwrap_or("")
}

fn terminal_event(event: &Value) -> bool {
    matches!(
        event_type(event),
        "response.completed" | "response.incomplete" | "response.failed"
    )
}

fn output_index(event: &Value) -> Option<Value> {
    event.get("output_index").cloned()
}

fn output_item_type(event: &Value) -> Option<&str> {
    event
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
}

fn has_encrypted_content(item: &Value) -> bool {
    item.get("encrypted_content")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

fn usage_from_terminal(event: &Value) -> Option<&Value> {
    event.get("response").and_then(|r| r.get("usage"))
}

fn sum_usage(acc: &mut Map<String, Value>, usage: Option<&Value>) {
    let Some(usage) = usage else {
        return;
    };

    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        if let Some(v) = usage.get(key).and_then(value_as_u64) {
            let cur = acc.get(key).and_then(Value::as_u64).unwrap_or(0);
            acc.insert(key.to_string(), json!(cur.saturating_add(v)));
        }
    }

    if let Some(v) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(value_as_u64)
    {
        let entry = acc
            .entry("input_tokens_details".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(obj) = entry.as_object_mut() {
            let cur = obj
                .get("cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            obj.insert("cached_tokens".to_string(), json!(cur.saturating_add(v)));
        }
    }

    if let Some(v) = usage
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(value_as_u64)
    {
        let entry = acc
            .entry("output_tokens_details".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(obj) = entry.as_object_mut() {
            let cur = obj
                .get("reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            obj.insert("reasoning_tokens".to_string(), json!(cur.saturating_add(v)));
        }
    }
}

#[derive(Default, Debug)]
struct FoldedUsage {
    proxy_billed_usage: Map<String, Value>,
    saw_usage: bool,
    first_input_tokens: Option<u64>,
    first_cached_tokens: Option<u64>,
    total_reasoning_tokens: u64,
    final_output_tokens: Option<u64>,
    final_reasoning_tokens: u64,
}

impl FoldedUsage {
    fn add_round_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };

        let input_tokens = usage.get("input_tokens").and_then(value_as_u64);
        let output_tokens = usage.get("output_tokens").and_then(value_as_u64);
        // 一个数字字段都提不出来（如非数字/畸形 usage）时视为无 usage，
        // 避免 saw_usage=true 导致 public_usage 用全 0 覆盖上游真实 usage。
        if input_tokens.is_none() && output_tokens.is_none() {
            return;
        }

        sum_usage(&mut self.proxy_billed_usage, Some(usage));

        if !self.saw_usage {
            self.first_input_tokens = input_tokens;
            self.first_cached_tokens = usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(value_as_u64);
        }

        let round_reasoning = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(value_as_u64)
            .unwrap_or(0);
        self.total_reasoning_tokens = self.total_reasoning_tokens.saturating_add(round_reasoning);
        self.final_output_tokens = output_tokens;
        self.final_reasoning_tokens = round_reasoning;
        self.saw_usage = true;
    }

    fn public_usage(&self) -> Map<String, Value> {
        if !self.saw_usage {
            return Map::new();
        }

        let public_input = self.first_input_tokens.unwrap_or(0);
        let final_output = self
            .final_output_tokens
            .unwrap_or(self.final_reasoning_tokens);
        let final_visible_output = final_output.saturating_sub(self.final_reasoning_tokens);
        let public_output = self
            .total_reasoning_tokens
            .saturating_add(final_visible_output);

        let mut usage = Map::new();
        usage.insert("input_tokens".to_string(), json!(public_input));
        usage.insert("output_tokens".to_string(), json!(public_output));
        usage.insert(
            "total_tokens".to_string(),
            json!(public_input.saturating_add(public_output)),
        );

        if let Some(cached_tokens) = self.first_cached_tokens {
            usage.insert(
                "input_tokens_details".to_string(),
                json!({ "cached_tokens": cached_tokens }),
            );
        }

        usage.insert(
            "output_tokens_details".to_string(),
            json!({ "reasoning_tokens": self.total_reasoning_tokens }),
        );
        usage
    }
}

#[derive(Clone, Copy)]
struct MetadataUsage<'a> {
    public_usage: &'a Map<String, Value>,
    proxy_billed_usage: &'a Map<String, Value>,
    truncation_step: u64,
}

fn metadata_with_continue(
    mut response: Value,
    rounds: &[Value],
    stopped_reason: Option<&str>,
    usage: MetadataUsage<'_>,
    proxy_rounds: usize,
) -> Value {
    let Some(resp) = response.as_object_mut() else {
        return response;
    };

    let metadata = resp
        .entry("metadata".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !metadata.is_object() {
        *metadata = Value::Object(Map::new());
    }
    let md = metadata.as_object_mut().expect("metadata object");
    md.insert("proxy_rounds".to_string(), Value::Array(rounds.to_vec()));
    md.insert(
        "ccswitch_codex_continue".to_string(),
        json!({
            "enabled": true,
            "proxy_rounds": proxy_rounds,
            "stopped_reason": stopped_reason,
            "provider_failover_allowed": true,
            "continuation_via_forward_with_retry": true,
            "truncation_step": usage.truncation_step,
            "truncation_formula": "reasoning_tokens >= step - 2 && (reasoning_tokens + 2) % step == 0",
            "public_usage_formula": "first_round_input + all_round_reasoning + final_round_visible_output",
            "proxy_billed_usage": Value::Object(usage.proxy_billed_usage.clone()),
        }),
    );
    if !usage.public_usage.is_empty() {
        resp.insert(
            "usage".to_string(),
            Value::Object(usage.public_usage.clone()),
        );
    }
    response
}

struct TerminalReconstruction<'a> {
    base_response: Option<&'a Value>,
    final_output: &'a [Value],
    rounds: &'a [Value],
    stopped_reason: Option<&'a str>,
    usage: MetadataUsage<'a>,
    proxy_rounds: usize,
}

fn reconstruct_terminal(
    terminal: Option<Value>,
    reconstruction: TerminalReconstruction<'_>,
    seq: &mut u64,
) -> Value {
    let terminal_type = terminal
        .as_ref()
        .and_then(|ev| ev.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("response.incomplete")
        .to_string();

    let terminal_response = terminal
        .as_ref()
        .and_then(|ev| ev.get("response"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut response = reconstruction
        .base_response
        .cloned()
        .unwrap_or_else(|| terminal_response.clone());

    if let Some(resp) = response.as_object_mut() {
        if let Some(status) = terminal_response.get("status").cloned() {
            resp.insert("status".to_string(), status);
        } else if terminal_type == "response.incomplete" {
            resp.insert("status".to_string(), json!("incomplete"));
        }
        if let Some(details) = terminal_response.get("incomplete_details").cloned() {
            resp.insert("incomplete_details".to_string(), details);
        }
        // response.failed 携带的 error 对象必须透传，客户端靠它展示失败原因
        if let Some(error) = terminal_response.get("error").cloned() {
            resp.insert("error".to_string(), error);
        }
        // usage 无法从终止事件提取时保留上游原始 usage（见 metadata_with_continue
        // 仅在 public_usage 非空时覆盖）
        if let Some(usage) = terminal_response.get("usage").cloned() {
            resp.insert("usage".to_string(), usage);
        }
        resp.insert(
            "output".to_string(),
            Value::Array(reconstruction.final_output.to_vec()),
        );
    }

    response = metadata_with_continue(
        response,
        reconstruction.rounds,
        reconstruction.stopped_reason,
        reconstruction.usage,
        reconstruction.proxy_rounds,
    );

    json!({
        "type": terminal_type,
        "response": response,
        "sequence_number": next_sequence(seq)
    })
}

fn synthetic_incomplete(
    base_response: Option<&Value>,
    final_output: &[Value],
    rounds: &[Value],
    reason: &str,
    usage: MetadataUsage<'_>,
    proxy_rounds: usize,
    seq: &mut u64,
) -> Value {
    let mut response = base_response.cloned().unwrap_or_else(|| json!({}));
    if let Some(resp) = response.as_object_mut() {
        resp.insert("status".to_string(), json!("incomplete"));
        resp.insert(
            "incomplete_details".to_string(),
            json!({
                "reason": reason,
            }),
        );
        resp.insert("output".to_string(), Value::Array(final_output.to_vec()));
    }
    response = metadata_with_continue(response, rounds, Some(reason), usage, proxy_rounds);

    json!({
        "type": "response.incomplete",
        "response": response,
        "sequence_number": next_sequence(seq)
    })
}

fn commentary_marker(marker: &str) -> Value {
    json!({
        "type": "message",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": marker,
        }],
        "phase": "commentary",
    })
}

fn input_as_vec(input: Option<&Value>) -> Vec<Value> {
    match input {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Null) | None => Vec::new(),
        // API 合法的字符串简写 input:"..." 需要包成 typed message item，
        // 裸字符串放进数组会被上游 400 拒绝。
        Some(Value::String(text)) => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
        })],
        Some(other) => vec![other.clone()],
    }
}

fn merge_include(existing: Option<&Value>) -> Value {
    let mut out = Vec::<Value>::new();
    if let Some(Value::Array(items)) = existing {
        for item in items {
            if !out.iter().any(|v| v == item) {
                out.push(item.clone());
            }
        }
    }
    if !out.iter().any(|v| v.as_str() == Some(ENCRYPTED_INCLUDE)) {
        out.push(json!(ENCRYPTED_INCLUDE));
    }
    Value::Array(out)
}

pub(crate) fn prepare_initial_payload(base_body: &Value) -> Value {
    let mut body = base_body.clone();
    if !body.is_object() {
        body = json!({});
    }
    let Some(obj) = body.as_object_mut() else {
        return body;
    };

    obj.insert("include".to_string(), merge_include(obj.get("include")));
    body
}

pub(crate) fn build_continuation_payload(base_body: &Value, replay_tail: &[Value]) -> Value {
    let mut body = base_body.clone();
    if !body.is_object() {
        body = json!({});
    }
    let orig_input = input_as_vec(body.get("input"));
    let Some(obj) = body.as_object_mut() else {
        return body;
    };

    let mut input = orig_input;
    input.extend_from_slice(replay_tail);
    obj.insert("stream".to_string(), json!(true));
    obj.insert("input".to_string(), Value::Array(input));
    obj.insert("include".to_string(), merge_include(obj.get("include")));
    obj.remove("previous_response_id");
    body
}

pub(crate) struct FoldedProxyResponseArgs {
    pub(crate) first_response: ProxyResponse,
    pub(crate) first_connection_guard: Option<ActiveConnectionGuard>,
    pub(crate) forwarder: RequestForwarder,
    pub(crate) method: Method,
    pub(crate) endpoint: String,
    pub(crate) base_body: Value,
    pub(crate) headers: HeaderMap,
    pub(crate) extensions: Extensions,
    pub(crate) providers: Vec<ProviderAttempt>,
    pub(crate) config: CodexContinueConfig,
}

struct FoldContinuationRequest {
    forwarder: RequestForwarder,
    method: Method,
    endpoint: String,
    base_body: Value,
    headers: HeaderMap,
    extensions: Extensions,
    providers: Vec<ProviderAttempt>,
    config: CodexContinueConfig,
}

pub(crate) fn build_folded_proxy_response(args: FoldedProxyResponseArgs) -> ProxyResponse {
    let status = args.first_response.status();
    let mut response_headers = args.first_response.headers().clone();
    response_headers.remove(CONTENT_LENGTH);
    response_headers.remove(CONTENT_ENCODING);
    response_headers.remove(TRANSFER_ENCODING);
    response_headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    let stream = fold_responses_stream(
        args.first_response,
        args.first_connection_guard,
        FoldContinuationRequest {
            forwarder: args.forwarder,
            method: args.method,
            endpoint: args.endpoint,
            base_body: args.base_body,
            headers: args.headers,
            extensions: args.extensions,
            providers: args.providers,
            config: args.config,
        },
    );
    ProxyResponse::streamed(status, response_headers, stream)
}

fn fold_responses_stream(
    first_response: ProxyResponse,
    first_connection_guard: Option<ActiveConnectionGuard>,
    continuation: FoldContinuationRequest,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut response = first_response;
        let mut connection_guard = first_connection_guard;
        let FoldContinuationRequest {
            forwarder,
            method,
            endpoint,
            base_body,
            headers,
            extensions,
            providers,
            config,
        } = continuation;
        let mut round_no = 0usize;
        let mut continuations = 0usize;
        let mut seq = 0u64;
        let mut downstream_output_index = 0usize;
        let mut base_response: Option<Value> = None;
        let mut final_output: Vec<Value> = Vec::new();
        let mut replay_tail: Vec<Value> = Vec::new();
        let mut rounds: Vec<Value> = Vec::new();
        let mut usage_acc = FoldedUsage::default();

        loop {
            round_no += 1;
            let mut parser = IncrementalSseParser::default();
            let stream = response.bytes_stream();
            tokio::pin!(stream);
            let _round_guard = connection_guard.take();
            // 本回合 上游 output_index → (下游 output_index, 是否 reasoning item)。
            // 所有事件实时直通下发（只重映射 index / 重编 sequence），不缓冲：
            // 缓冲会丢续写回合的非 reasoning 产出、并让下游在正文阶段零字节输出
            // 而触发静默期超时。
            let mut round_items: std::collections::HashMap<String, (usize, bool)> =
                std::collections::HashMap::new();
            // 本回合最近一次 output_item.added 的 (下游 index, 是否 reasoning)，
            // 作为缺失/不匹配 output_index 事件的兜底归属目标。
            let mut last_added_item: Option<(usize, bool)> = None;
            // 本回合已完成的 output items（按序，含非 reasoning），用于终止事件的
            // output 数组和续写回放（续写模型需要看到自己上一轮的全部产出）。
            let mut round_output: Vec<Value> = Vec::new();
            let mut round_has_reasoning_encrypted = false;
            let mut terminal: Option<Value> = None;
            let mut stream_error: Option<String> = None;

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        stream_error = Some(e.to_string());
                        break;
                    }
                };
                for frame in parser.push(&chunk) {
                    match frame {
                        SseFrame::Done => {}
                        SseFrame::Event(mut ev) => {
                            let t = event_type(&ev).to_string();
                            if t == "response.created" || t == "response.in_progress" {
                                if round_no == 1 {
                                    if t == "response.created" {
                                        base_response = ev.get("response").cloned();
                                    }
                                    set_sequence(&mut ev, &mut seq);
                                    yield Ok(sse_event(&ev));
                                }
                                continue;
                            }

                            if terminal_event(&ev) {
                                terminal = Some(ev);
                                continue;
                            }

                            if t == "response.output_item.added" {
                                let is_reasoning = output_item_type(&ev) == Some("reasoning");
                                // Key by the upstream output_index when present. When the upstream
                                // omits output_index on `added`, later events for the same item
                                // (delta/done) can still arrive carrying a numeric output_index, so
                                // a synthetic key would never match and the raw round-local index
                                // would leak downstream and collide. Track the most-recent added
                                // item as the fallback target for such orphaned events.
                                if let Some(oi) = output_index(&ev) {
                                    round_items.insert(oi.to_string(), (downstream_output_index, is_reasoning));
                                }
                                last_added_item = Some((downstream_output_index, is_reasoning));
                                set_output_index(&mut ev, downstream_output_index);
                                downstream_output_index += 1;
                                set_sequence(&mut ev, &mut seq);
                                yield Ok(sse_event(&ev));
                                continue;
                            }

                            // Resolve this event's downstream index: prefer an exact match on the
                            // upstream output_index, else fall back to the most-recently-added item
                            // (covers upstreams that drop output_index on `added` but include it
                            // later, and passthrough events that carry no index at all).
                            let resolved = ev
                                .get("output_index")
                                .and_then(|v| v.as_u64())
                                .and_then(|up_oi| round_items.get(&up_oi.to_string()).copied())
                                .or(last_added_item);
                            if let Some((ds_oi, is_reasoning)) = resolved {
                                if output_index(&ev).is_some() {
                                    set_output_index(&mut ev, ds_oi);
                                }
                                if t == "response.output_item.done" {
                                    if let Some(item) = ev.get("item").cloned() {
                                        if is_reasoning && has_encrypted_content(&item) {
                                            round_has_reasoning_encrypted = true;
                                        }
                                        round_output.push(item.clone());
                                        final_output.push(item);
                                    }
                                }
                            }
                            set_sequence(&mut ev, &mut seq);
                            yield Ok(sse_event(&ev));
                        }
                    }
                }
            }

            for frame in parser.finish() {
                match frame {
                    SseFrame::Done => {},
                    SseFrame::Event(ev) if terminal_event(&ev) => terminal = Some(ev),
                    SseFrame::Event(mut ev) => {
                        set_sequence(&mut ev, &mut seq);
                        yield Ok(sse_event(&ev));
                    }
                }
            }

            let usage = terminal.as_ref().and_then(usage_from_terminal);
            let rt = reasoning_tokens(usage);
            usage_acc.add_round_usage(usage);
            let has_encrypted = round_has_reasoning_encrypted;
            let truncated = is_truncation_pattern(rt, config.step);
            let can_continue = terminal.is_some()
                && truncated
                && has_encrypted
                && continuations < config.max_continuations;
            rounds.push(json!({
                "round": round_no,
                "reasoning_tokens": rt,
                "truncated": truncated,
                "has_encrypted_content": has_encrypted,
                "continued": can_continue,
            }));

            if let Some(error) = stream_error {
                log::warn!("[CodexContinue] round {round_no} upstream stream error: {error}");
                let public_usage = usage_acc.public_usage();
                let metadata_usage = MetadataUsage {
                    public_usage: &public_usage,
                    proxy_billed_usage: &usage_acc.proxy_billed_usage,
                    truncation_step: config.step,
                };
                let ev = synthetic_incomplete(
                    base_response.as_ref(),
                    &final_output,
                    &rounds,
                    "upstream_error",
                    metadata_usage,
                    round_no,
                    &mut seq,
                );
                yield Ok(sse_event(&ev));
                yield Ok(sse_done());
                break;
            }

            if can_continue {
                continuations += 1;
                // 回放本回合全部产出（reasoning + message/function_call 等），
                // 续写模型必须能看到自己上一轮的完整输出，否则会重复/丢内容。
                replay_tail.append(&mut round_output);
                replay_tail.push(commentary_marker(&config.marker));

                let next_body = build_continuation_payload(&base_body, &replay_tail);
                log::info!(
                    "[CodexContinue] round {round_no}: reasoning_tokens={:?}, continue {}/{}",
                    rt,
                    continuations,
                    config.max_continuations
                );

                match forwarder
                    .forward_with_retry(
                        &AppType::Codex,
                        method.clone(),
                        &endpoint,
                        next_body,
                        headers.clone(),
                        extensions.clone(),
                        providers.clone(),
                    )
                    .await
                {
                    Ok(mut result) => {
                        connection_guard = result.connection_guard.take();
                        response = result.response;
                        if !response.status().is_success() || !response.is_sse() {
                            let reason = if response.is_sse() { "upstream_status" } else { "upstream_not_sse" };
                            log::warn!(
                                "[CodexContinue] continuation round {} stopped: status={}, is_sse={}",
                                round_no + 1,
                                response.status().as_u16(),
                                response.is_sse()
                            );
                            let public_usage = usage_acc.public_usage();
                            let metadata_usage = MetadataUsage {
                                public_usage: &public_usage,
                                proxy_billed_usage: &usage_acc.proxy_billed_usage,
                                truncation_step: config.step,
                            };
                            let ev = synthetic_incomplete(
                                base_response.as_ref(),
                                &final_output,
                                &rounds,
                                reason,
                                metadata_usage,
                                round_no,
                                &mut seq,
                            );
                            yield Ok(sse_event(&ev));
                            yield Ok(sse_done());
                            break;
                        }
                        continue;
                    }
                    Err(err) => {
                        log::warn!(
                            "[CodexContinue] continuation round {} forward failed: {}",
                            round_no + 1,
                            err.error
                        );
                        let public_usage = usage_acc.public_usage();
                        let metadata_usage = MetadataUsage {
                            public_usage: &public_usage,
                            proxy_billed_usage: &usage_acc.proxy_billed_usage,
                            truncation_step: config.step,
                        };
                        let ev = synthetic_incomplete(
                            base_response.as_ref(),
                            &final_output,
                            &rounds,
                            "upstream_error",
                            metadata_usage,
                            round_no,
                            &mut seq,
                        );
                        yield Ok(sse_event(&ev));
                        yield Ok(sse_done());
                        break;
                    }
                }
            }

            let stopped_reason = if truncated && !has_encrypted {
                Some("no_encrypted_content")
            } else if truncated && continuations >= config.max_continuations {
                Some("max_continue")
            } else if terminal.is_none() {
                Some("upstream_eof")
            } else {
                None
            };

            if terminal.is_none() {
                let public_usage = usage_acc.public_usage();
                let metadata_usage = MetadataUsage {
                    public_usage: &public_usage,
                    proxy_billed_usage: &usage_acc.proxy_billed_usage,
                    truncation_step: config.step,
                };
                let ev = synthetic_incomplete(
                    base_response.as_ref(),
                    &final_output,
                    &rounds,
                    "upstream_eof",
                    metadata_usage,
                    round_no,
                    &mut seq,
                );
                yield Ok(sse_event(&ev));
                yield Ok(sse_done());
                break;
            }

            let public_usage = usage_acc.public_usage();
            let metadata_usage = MetadataUsage {
                public_usage: &public_usage,
                proxy_billed_usage: &usage_acc.proxy_billed_usage,
                truncation_step: config.step,
            };
            let ev = reconstruct_terminal(
                terminal,
                TerminalReconstruction {
                    base_response: base_response.as_ref(),
                    final_output: &final_output,
                    rounds: &rounds,
                    stopped_reason,
                    usage: metadata_usage,
                    proxy_rounds: round_no,
                },
                &mut seq,
            );
            yield Ok(sse_event(&ev));
            yield Ok(sse_done());
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_formula_matches_518n_minus_2() {
        assert!(is_truncation_pattern(Some(516), 518));
        assert!(is_truncation_pattern(Some(1034), 518));
        assert!(!is_truncation_pattern(Some(515), 518));
        assert!(!is_truncation_pattern(Some(517), 518));
        assert!(!is_truncation_pattern(None, 518));
    }

    #[test]
    fn sse_parser_handles_split_chunks_multi_data_and_done() {
        let mut parser = IncrementalSseParser::default();
        let part1 = b"event: response.output_text.delta\ndata: {\"type\":\"response.output";
        let part2 = b"_text.delta\",\ndata: \"delta\":\"hi\"}\n\ndata: [DONE]\n\n";

        assert!(parser.push(part1).is_empty());
        let frames = parser.push(part2);
        assert_eq!(frames.len(), 2);
        match &frames[0] {
            SseFrame::Event(v) => {
                assert_eq!(v["type"], "response.output_text.delta");
                assert_eq!(v["delta"], "hi");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        assert_eq!(frames[1], SseFrame::Done);
    }

    #[test]
    fn folded_public_usage_avoids_replayed_input_and_keeps_billed_metadata() {
        let round1 = json!({
            "input_tokens": 100,
            "output_tokens": 526,
            "total_tokens": 626,
            "input_tokens_details": { "cached_tokens": 20 },
            "output_tokens_details": { "reasoning_tokens": 516 },
        });
        let round2 = json!({
            "input_tokens": 180,
            "output_tokens": 525,
            "total_tokens": 705,
            "input_tokens_details": { "cached_tokens": 30 },
            "output_tokens_details": { "reasoning_tokens": 516 },
        });
        let round3 = json!({
            "input_tokens": 240,
            "output_tokens": 80,
            "total_tokens": 320,
            "input_tokens_details": { "cached_tokens": 40 },
            "output_tokens_details": { "reasoning_tokens": 20 },
        });

        let mut usage = FoldedUsage::default();
        usage.add_round_usage(Some(&round1));
        usage.add_round_usage(Some(&round2));
        usage.add_round_usage(Some(&round3));

        let public_usage = usage.public_usage();
        assert_eq!(public_usage["input_tokens"], json!(100));
        assert_eq!(
            public_usage["input_tokens_details"]["cached_tokens"],
            json!(20)
        );
        assert_eq!(
            public_usage["output_tokens_details"]["reasoning_tokens"],
            json!(1052)
        );
        assert_eq!(public_usage["output_tokens"], json!(1112));
        assert_eq!(public_usage["total_tokens"], json!(1212));

        assert_eq!(usage.proxy_billed_usage["input_tokens"], json!(520));
        assert_eq!(usage.proxy_billed_usage["output_tokens"], json!(1131));
        assert_eq!(usage.proxy_billed_usage["total_tokens"], json!(1651));
        assert_eq!(
            usage.proxy_billed_usage["input_tokens_details"]["cached_tokens"],
            json!(90)
        );
        assert_eq!(
            usage.proxy_billed_usage["output_tokens_details"]["reasoning_tokens"],
            json!(1052)
        );

        let response = metadata_with_continue(
            json!({"id": "resp_1", "metadata": {}}),
            &[],
            None,
            MetadataUsage {
                public_usage: &public_usage,
                proxy_billed_usage: &usage.proxy_billed_usage,
                truncation_step: 518,
            },
            3,
        );

        assert_eq!(response["usage"], Value::Object(public_usage));
        let continue_md = &response["metadata"]["ccswitch_codex_continue"];
        assert_eq!(
            continue_md["proxy_billed_usage"]["input_tokens"],
            json!(520)
        );
        assert_eq!(continue_md["provider_failover_allowed"], json!(true));
        assert_eq!(
            continue_md["continuation_via_forward_with_retry"],
            json!(true)
        );
        assert_eq!(continue_md["truncation_step"], json!(518));
    }

    #[test]
    fn request_gating_requires_stream_reasoning_object_and_store_false() {
        let cfg = CodexContinueConfig {
            enabled: true,
            max_continuations: 8,
            step: 518,
            marker: DEFAULT_MARKER.to_string(),
        };
        // 齐活：stream + reasoning 对象 + store:false
        assert!(should_enable_for_request(
            &json!({"stream": true, "reasoning": {"effort": "high"}, "store": false}),
            &cfg
        ));
        // 缺 reasoning 对象（非推理请求）不启用
        assert!(!should_enable_for_request(
            &json!({"stream": true, "store": false}),
            &cfg
        ));
        // store 非 false（省略即默认 true / 服务端会话态客户端）不启用
        assert!(!should_enable_for_request(
            &json!({"stream": true, "reasoning": {"effort": "high"}}),
            &cfg
        ));
        assert!(!should_enable_for_request(
            &json!({"stream": true, "reasoning": {"effort": "high"}, "store": true}),
            &cfg
        ));
        // 非流式不启用
        assert!(!should_enable_for_request(
            &json!({"stream": false, "reasoning": {"effort": "high"}, "store": false}),
            &cfg
        ));
        // reasoning: false（布尔）不是对象，不启用
        assert!(!should_enable_for_request(
            &json!({"stream": true, "reasoning": false, "store": false}),
            &cfg
        ));
    }

    #[test]
    fn float_usage_is_tolerated_and_malformed_usage_does_not_zero_out() {
        // 浮点序列化的 usage 仍能提取
        let float_usage = json!({
            "input_tokens": 100.0,
            "output_tokens": 526.0,
            "output_tokens_details": { "reasoning_tokens": 516.0 },
        });
        assert_eq!(reasoning_tokens(Some(&float_usage)), Some(516));
        let mut acc = FoldedUsage::default();
        acc.add_round_usage(Some(&float_usage));
        assert_eq!(acc.public_usage()["input_tokens"], json!(100));

        // 完全提取不到数字的畸形 usage 不置 saw_usage，
        // 不会用全 0 的 public_usage 覆盖上游真实 usage
        let malformed = json!({ "input_tokens": "not-a-number" });
        let mut acc = FoldedUsage::default();
        acc.add_round_usage(Some(&malformed));
        assert!(acc.public_usage().is_empty());
    }

    #[test]
    fn string_shorthand_input_is_wrapped_as_typed_message() {
        let base = json!({ "stream": true, "input": "solve this" });
        let payload = build_continuation_payload(&base, &[]);
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "solve this");
    }

    #[test]
    fn reconstruct_terminal_preserves_failed_error_object() {
        let terminal = json!({
            "type": "response.failed",
            "response": {
                "status": "failed",
                "error": { "code": "server_error", "message": "boom" },
            }
        });
        let public_usage = Map::new();
        let billed = Map::new();
        let mut seq = 0u64;
        let ev = reconstruct_terminal(
            Some(terminal),
            TerminalReconstruction {
                base_response: Some(&json!({"id": "resp_1"})),
                final_output: &[],
                rounds: &[],
                stopped_reason: None,
                usage: MetadataUsage {
                    public_usage: &public_usage,
                    proxy_billed_usage: &billed,
                    truncation_step: 518,
                },
                proxy_rounds: 1,
            },
            &mut seq,
        );
        assert_eq!(ev["type"], "response.failed");
        assert_eq!(ev["response"]["error"]["code"], "server_error");
        assert_eq!(ev["response"]["error"]["message"], "boom");
    }

    #[test]
    fn initial_payload_adds_encrypted_include_and_keeps_previous_response_id() {
        let base = json!({
            "model": "gpt-5",
            "stream": true,
            "previous_response_id": "resp_old",
            "input": [{"type": "message", "role": "user", "content": "hi"}],
        });

        let payload = prepare_initial_payload(&base);

        assert_eq!(payload["previous_response_id"], "resp_old");
        assert_eq!(payload["input"], base["input"]);
        let include = payload["include"].as_array().unwrap();
        assert!(include
            .iter()
            .any(|v| v.as_str() == Some(ENCRYPTED_INCLUDE)));
    }

    #[test]
    fn payload_builder_appends_replay_tail_and_preserves_encrypted_include() {
        let base = json!({
            "model": "gpt-5",
            "stream": false,
            "previous_response_id": "resp_old",
            "include": ["foo"],
            "input": [{"type": "message", "role": "user", "content": "hi"}],
        });
        let reasoning = json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "secret",
        });
        let marker = commentary_marker("continue");
        let payload = build_continuation_payload(&base, &[reasoning.clone(), marker.clone()]);

        assert_eq!(payload["stream"], true);
        assert!(payload.get("previous_response_id").is_none());
        assert_eq!(payload["input"].as_array().unwrap().len(), 3);
        assert_eq!(payload["input"][1], reasoning);
        assert_eq!(payload["input"][2], marker);
        let include = payload["include"].as_array().unwrap();
        assert!(include.iter().any(|v| v.as_str() == Some("foo")));
        assert!(include
            .iter()
            .any(|v| v.as_str() == Some(ENCRYPTED_INCLUDE)));
    }

    #[test]
    fn payload_builder_preserves_multi_round_replay_tail_order() {
        let base = json!({
            "stream": true,
            "input": [{"role": "user", "content": "start"}],
        });
        let r1 = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "a"});
        let m1 = commentary_marker("continue 1");
        let r2 = json!({"type": "reasoning", "id": "rs_2", "encrypted_content": "b"});
        let m2 = commentary_marker("continue 2");

        let payload =
            build_continuation_payload(&base, &[r1.clone(), m1.clone(), r2.clone(), m2.clone()]);
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 5);
        assert_eq!(input[1], r1);
        assert_eq!(input[2], m1);
        assert_eq!(input[3], r2);
        assert_eq!(input[4], m2);
    }
}
