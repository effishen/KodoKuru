//! OpenAI-compatible client: streaming chat completions with tool calls,
//! health checks, and the audio/image endpoints used by capability engines.

pub mod parse;

use futures::StreamExt;
use kk_core::{EngineConfig, ToolCall, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub use parse::{extract_text_tool_calls, repair_json, Piece, ThinkSplitter};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireFunction {
    pub name: String,
    pub arguments: String,
}

/// A chat message in OpenAI wire format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(s: impl Into<String>) -> Self {
        Self { role: "system".into(), content: Some(s.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(s.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn assistant(content: impl Into<String>, calls: &[ToolCall]) -> Self {
        let content: String = content.into();
        Self {
            role: "assistant".into(),
            content: Some(content),
            tool_calls: if calls.is_empty() {
                None
            } else {
                Some(
                    calls
                        .iter()
                        .map(|c| WireToolCall {
                            id: c.id.clone(),
                            kind: "function".into(),
                            function: WireFunction { name: c.name.clone(), arguments: c.arguments.clone() },
                        })
                        .collect(),
                )
            },
            tool_call_id: None,
        }
    }
    pub fn tool(call_id: impl Into<String>, s: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: Some(s.into()), tool_calls: None, tool_call_id: Some(call_id.into()) }
    }
    /// Rough size for context budgeting.
    pub fn approx_chars(&self) -> usize {
        self.content.as_ref().map_or(0, |c| c.len())
            + self
                .tool_calls
                .as_ref()
                .map_or(0, |v| v.iter().map(|c| c.function.arguments.len() + c.function.name.len() + 16).sum())
            + 8
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<String>,
    /// Ask for a JSON object reply (used by control calls).
    pub json_mode: bool,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LlmError {
    /// Connection failed, timed out, stalled, or the server was busy/5xx.
    /// Retry later; the engine is offline or overloaded.
    Unreachable(String),
    /// The prompt does not fit the model's context window.
    ContextTooLong(String),
    /// The request was rejected (4xx); retrying unchanged will not help.
    Rejected(u16, String),
    Cancelled,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Unreachable(s) => write!(f, "engine unreachable: {s}"),
            LlmError::ContextTooLong(s) => write!(f, "context too long: {s}"),
            LlmError::Rejected(c, s) => write!(f, "request rejected ({c}): {s}"),
            LlmError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for LlmError {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// Max time without any bytes from a streaming response.
    pub idle_timeout: Duration,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

fn url(engine: &EngineConfig, path: &str) -> String {
    format!("{}/{}", engine.base_url.trim_end_matches('/'), path)
}

fn toml_to_json(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn classify_status(status: u16, body: String) -> LlmError {
    let lower = body.to_lowercase();
    if status == 429 || status >= 500 {
        return LlmError::Unreachable(format!("HTTP {status}: {}", truncate(&body, 300)));
    }
    if (status == 400 || status == 413)
        && (lower.contains("context") || lower.contains("too long") || lower.contains("maximum") || lower.contains("exceed"))
        && (lower.contains("token") || lower.contains("length") || lower.contains("context"))
    {
        return LlmError::ContextTooLong(truncate(&body, 300));
    }
    LlmError::Rejected(status, truncate(&body, 500))
}

fn classify_reqwest(e: reqwest::Error) -> LlmError {
    LlmError::Unreachable(format!("{e}"))
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

impl Client {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Client { http, idle_timeout: Duration::from_secs(300) }
    }

    fn auth(&self, rb: reqwest::RequestBuilder, key: Option<&str>) -> reqwest::RequestBuilder {
        match key {
            Some(k) if !k.is_empty() => rb.bearer_auth(k),
            _ => rb,
        }
    }

    /// `GET /models` — used for the online/offline indicator.
    pub async fn health(&self, engine: &EngineConfig, key: Option<&str>) -> Result<Vec<String>, LlmError> {
        let rb = self.http.get(url(engine, "models")).timeout(Duration::from_secs(8));
        let resp = self.auth(rb, key).send().await.map_err(classify_reqwest)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(classify_reqwest)?;
        if status >= 400 {
            return Err(classify_status(status, body));
        }
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        Ok(v["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    /// Streaming chat completion. `on_event` receives text and reasoning
    /// deltas as they arrive; the full response is returned at the end.
    pub async fn chat(
        &self,
        engine: &EngineConfig,
        key: Option<&str>,
        req: &ChatRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, LlmError> {
        let mut body = json!({
            "model": engine.model,
            "messages": req.messages,
            "stream": true,
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
                    .collect(),
            );
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if let Some(t) = req.temperature.or(engine.temperature) {
            body["temperature"] = json!(t);
        }
        if let Some(r) = req.reasoning_effort.as_ref().or(engine.reasoning_effort.as_ref()) {
            body["reasoning_effort"] = json!(r);
        }
        if req.json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        for (k, v) in &engine.extra {
            body[k] = toml_to_json(v);
        }

        let started = Instant::now();
        let rb = self.http.post(url(engine, "chat/completions")).json(&body);
        let send = self.auth(rb, key).send();
        let resp = tokio::select! {
            r = send => r.map_err(classify_reqwest)?,
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        };
        let status = resp.status().as_u16();
        if status >= 400 {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_status(status, text));
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("event-stream"));

        let mut out = ChatResponse::default();
        let mut splitter = ThinkSplitter::default();
        let mut reasoning = String::new();
        let mut calls: Vec<PartialCall> = Vec::new();

        let handle_text = |text: &str, out: &mut ChatResponse, reasoning: &mut String, splitter: &mut ThinkSplitter, on_event: &mut (dyn FnMut(StreamEvent) + Send)| {
            for p in splitter.push(text) {
                match p {
                    Piece::Text(t) => {
                        on_event(StreamEvent::Text(t.clone()));
                        out.content.push_str(&t);
                    }
                    Piece::Reasoning(r) => {
                        on_event(StreamEvent::Reasoning(r.clone()));
                        reasoning.push_str(&r);
                    }
                }
            }
        };

        if !is_sse {
            // Server ignored `stream: true`; parse a normal response.
            let text = resp.text().await.map_err(classify_reqwest)?;
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| LlmError::Rejected(status, format!("invalid JSON response: {e}")))?;
            let msg = &v["choices"][0]["message"];
            if let Some(r) = msg["reasoning_content"].as_str().or(msg["reasoning"].as_str()) {
                on_event(StreamEvent::Reasoning(r.to_string()));
                reasoning.push_str(r);
            }
            if let Some(c) = msg["content"].as_str() {
                handle_text(c, &mut out, &mut reasoning, &mut splitter, on_event);
            }
            if let Some(tc) = msg["tool_calls"].as_array() {
                for c in tc {
                    calls.push(PartialCall {
                        id: c["id"].as_str().unwrap_or_default().to_string(),
                        name: c["function"]["name"].as_str().unwrap_or_default().to_string(),
                        args: match &c["function"]["arguments"] {
                            Value::String(s) => s.clone(),
                            other if other.is_null() => String::new(),
                            other => other.to_string(),
                        },
                    });
                }
            }
            out.finish_reason = v["choices"][0]["finish_reason"].as_str().map(str::to_string);
            out.usage = parse_usage(&v["usage"]);
        } else {
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            'outer: loop {
                let next = tokio::select! {
                    n = tokio::time::timeout(self.idle_timeout, stream.next()) => n,
                    _ = cancel.cancelled() => return Err(LlmError::Cancelled),
                };
                let chunk = match next {
                    Err(_) => return Err(LlmError::Unreachable("stream stalled (no data)".into())),
                    Ok(None) => break,
                    Ok(Some(Err(e))) => return Err(LlmError::Unreachable(format!("stream interrupted: {e}"))),
                    Ok(Some(Ok(b))) => b,
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buf.find('\n') {
                    let line: String = buf[..pos].trim_end_matches('\r').to_string();
                    buf.drain(..=pos);
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data == "[DONE]" {
                        break 'outer;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                    if let Some(err) = v.get("error") {
                        return Err(LlmError::Rejected(status, err.to_string()));
                    }
                    if let Some(u) = parse_usage(&v["usage"]) {
                        out.usage = Some(u);
                    }
                    let choice = &v["choices"][0];
                    let delta = &choice["delta"];
                    if let Some(r) = delta["reasoning_content"].as_str().or(delta["reasoning"].as_str()) {
                        if !r.is_empty() {
                            on_event(StreamEvent::Reasoning(r.to_string()));
                            reasoning.push_str(r);
                        }
                    }
                    if let Some(c) = delta["content"].as_str() {
                        handle_text(c, &mut out, &mut reasoning, &mut splitter, on_event);
                    }
                    if let Some(tcs) = delta["tool_calls"].as_array() {
                        for tc in tcs {
                            let idx = tc["index"].as_u64().unwrap_or(calls.len().saturating_sub(1) as u64) as usize;
                            while calls.len() <= idx {
                                calls.push(PartialCall::default());
                            }
                            let pc = &mut calls[idx];
                            if let Some(id) = tc["id"].as_str() {
                                if !id.is_empty() {
                                    pc.id = id.to_string();
                                }
                            }
                            if let Some(n) = tc["function"]["name"].as_str() {
                                pc.name.push_str(n);
                            }
                            match &tc["function"]["arguments"] {
                                Value::String(a) => pc.args.push_str(a),
                                Value::Null => {}
                                other => pc.args.push_str(&other.to_string()),
                            }
                        }
                    }
                    if let Some(f) = choice["finish_reason"].as_str() {
                        out.finish_reason = Some(f.to_string());
                    }
                }
            }
        }
        for p in splitter.finish() {
            match p {
                Piece::Text(t) => {
                    on_event(StreamEvent::Text(t.clone()));
                    out.content.push_str(&t);
                }
                Piece::Reasoning(r) => reasoning.push_str(&r),
            }
        }
        out.tool_calls = calls
            .into_iter()
            .enumerate()
            .filter(|(_, c)| !c.name.is_empty())
            .map(|(i, c)| ToolCall {
                id: if c.id.is_empty() { format!("call_{i}_{}", started.elapsed().as_micros()) } else { c.id },
                name: c.name,
                arguments: c.args,
            })
            .collect();
        if out.tool_calls.is_empty() && out.content.contains("<tool_call>") {
            let (rest, calls) = extract_text_tool_calls(&out.content);
            out.content = rest;
            out.tool_calls = calls;
        }
        if !reasoning.trim().is_empty() {
            out.reasoning = Some(reasoning.trim().to_string());
        }
        let elapsed = started.elapsed().as_millis() as u64;
        match &mut out.usage {
            Some(u) => u.elapsed_ms = elapsed,
            None => out.usage = Some(Usage { elapsed_ms: elapsed, ..Default::default() }),
        }
        Ok(out)
    }

    /// `POST /audio/speech` — returns audio bytes.
    pub async fn tts(
        &self,
        engine: &EngineConfig,
        key: Option<&str>,
        text: &str,
        voice: &str,
        format: &str,
    ) -> Result<Vec<u8>, LlmError> {
        let mut body = json!({"model": engine.model, "input": text, "voice": voice, "response_format": format});
        for (k, v) in &engine.extra {
            body[k] = toml_to_json(v);
        }
        let rb = self.http.post(url(engine, "audio/speech")).json(&body).timeout(Duration::from_secs(600));
        let resp = self.auth(rb, key).send().await.map_err(classify_reqwest)?;
        let status = resp.status().as_u16();
        if status >= 400 {
            return Err(classify_status(status, resp.text().await.unwrap_or_default()));
        }
        Ok(resp.bytes().await.map_err(classify_reqwest)?.to_vec())
    }

    /// `POST /images/generations` with `b64_json` — returns image bytes.
    pub async fn image(&self, engine: &EngineConfig, key: Option<&str>, prompt: &str, size: &str) -> Result<Vec<u8>, LlmError> {
        use base64::Engine as _;
        let mut body = json!({"model": engine.model, "prompt": prompt, "size": size, "n": 1, "response_format": "b64_json"});
        for (k, v) in &engine.extra {
            body[k] = toml_to_json(v);
        }
        let rb = self.http.post(url(engine, "images/generations")).json(&body).timeout(Duration::from_secs(600));
        let resp = self.auth(rb, key).send().await.map_err(classify_reqwest)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(classify_reqwest)?;
        if status >= 400 {
            return Err(classify_status(status, text));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| LlmError::Rejected(status, e.to_string()))?;
        let b64 = v["data"][0]["b64_json"]
            .as_str()
            .ok_or_else(|| LlmError::Rejected(status, "response has no data[0].b64_json".into()))?;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| LlmError::Rejected(status, e.to_string()))
    }
}

fn parse_usage(v: &Value) -> Option<Usage> {
    let p = v.get("prompt_tokens")?.as_u64()? as u32;
    let c = v.get("completion_tokens").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
    Some(Usage { prompt_tokens: p, completion_tokens: c, elapsed_ms: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};

    fn engine(base: &str) -> EngineConfig {
        toml::from_str(&format!("id = \"t\"\nbase_url = \"{base}\"\nmodel = \"m\"")).unwrap()
    }

    async fn serve(body: &'static str, ctype: &'static str) -> String {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || async move { ([(axum::http::header::CONTENT_TYPE, ctype)], body) }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn streams_text_reasoning_and_tool_calls() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let base = serve(sse, "text/event-stream").await;
        let c = Client::new();
        let mut deltas = Vec::new();
        let r = c
            .chat(&engine(&base), None, &ChatRequest::default(), &mut |e| deltas.push(format!("{e:?}")), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.content, "Hi");
        assert_eq!(r.reasoning.as_deref(), Some("think"));
        assert_eq!(r.tool_calls, vec![ToolCall { id: "c1".into(), name: "read_file".into(), arguments: "{\"path\":\"a\"}".into() }]);
        assert_eq!(r.usage.unwrap().prompt_tokens, 5);
        assert_eq!(deltas.len(), 2);
    }

    #[tokio::test]
    async fn non_streaming_fallback() {
        let body = r#"{"choices":[{"message":{"content":"<think>hmm</think>Done."},"finish_reason":"stop"}]}"#;
        let base = serve(body, "application/json").await;
        let r = Client::new()
            .chat(&engine(&base), None, &ChatRequest::default(), &mut |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.content, "Done.");
        assert_eq!(r.reasoning.as_deref(), Some("hmm"));
    }

    #[tokio::test]
    async fn unreachable_is_classified() {
        let e = Client::new()
            .chat(&engine("http://127.0.0.1:9/v1"), None, &ChatRequest::default(), &mut |_| {}, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(e, LlmError::Unreachable(_)), "{e:?}");
    }

    #[test]
    fn status_classification() {
        assert!(matches!(classify_status(503, "busy".into()), LlmError::Unreachable(_)));
        assert!(matches!(
            classify_status(400, "This model's maximum context length is 8192 tokens".into()),
            LlmError::ContextTooLong(_)
        ));
        assert!(matches!(classify_status(401, "bad key".into()), LlmError::Rejected(401, _)));
    }
}
