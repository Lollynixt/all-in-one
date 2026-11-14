//! Cloud providers (Anthropic Claude, OpenAI) and the router that lets each AI node pick its own model.
//!
//! Model references: a bare name such as `llama3.2:3b` is the local provider. Cloud models carry a prefix:
//! `anthropic/claude-sonnet-5-5`, `openai/gpt-4o`.
use super::{check, net, LineBuffer, OpenAiCompat};
use aio_core::{ChatEvent, ChatRequest, ModelInfo, Provider, ProviderError, Role};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

pub const ANTHROPIC_PREFIX: &str = "anthropic/";
pub const OPENAI_PREFIX: &str = "openai/";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const OPENAI_BASE: &str = "https://api.openai.com/v1";

pub fn is_cloud_ref(model: &str) -> bool {
    model.starts_with(ANTHROPIC_PREFIX) || model.starts_with(OPENAI_PREFIX)
}

/// Human-friendly name for a model reference, e.g. `Claude · claude-sonnet-5-5`.
pub fn display_ref(model: &str) -> String {
    if let Some(m) = model.strip_prefix(ANTHROPIC_PREFIX) {
        format!("Claude · {m}")
    } else if let Some(m) = model.strip_prefix(OPENAI_PREFIX) {
        format!("OpenAI · {m}")
    } else {
        model.to_string()
    }
}

/// One Anthropic SSE `data:` line. Text deltas become tokens, `message_stop` ends the stream.
pub fn parse_anthropic_line(line: &str) -> Result<Option<ChatEvent>, ProviderError> {
    let Some(data) = line.strip_prefix("data:") else { return Ok(None) };
    let v: Value = serde_json::from_str(data.trim()).map_err(|e| ProviderError::Decode(e.to_string()))?;
    Ok(match v["type"].as_str() {
        Some("content_block_delta") => v.pointer("/delta/text").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| ChatEvent::Token(s.to_string())),
        Some("message_stop") => Some(ChatEvent::Done),
        Some("error") => Some(ChatEvent::Error(v.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error").to_string())),
        _ => None,
    })
}

pub struct Anthropic {
    base: String,
    key: String,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(key: impl Into<String>) -> Self {
        Self::with_base("https://api.anthropic.com", key)
    }
    pub fn with_base(base: impl Into<String>, key: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), key: key.into(), client: reqwest::Client::new() }
    }
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("x-api-key", &self.key).header("anthropic-version", ANTHROPIC_VERSION)
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn id(&self) -> &str {
        "anthropic"
    }
    fn is_cloud(&self) -> bool {
        true
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let resp = check(self.auth(self.client.get(format!("{}/v1/models?limit=100", self.base))).send().await.map_err(net)?).await?;
        let v: Value = resp.json().await.map_err(|e| ProviderError::Decode(e.to_string()))?;
        Ok(v["data"].as_array().map(|a| a.iter().filter_map(|m| Some(ModelInfo { name: m["id"].as_str()?.to_string(), size_bytes: 0 })).collect()).unwrap_or_default())
    }
    async fn chat(&self, req: ChatRequest, tx: UnboundedSender<ChatEvent>) -> Result<(), ProviderError> {
        let system: Vec<&str> = req.messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| json!({ "role": if m.role == Role::Assistant { "assistant" } else { "user" }, "content": m.content }))
            .collect();
        let mut body = json!({ "model": req.model, "max_tokens": 8192, "stream": true, "messages": messages });
        if !system.is_empty() {
            body["system"] = json!(system.join("\n\n"));
        }
        let resp = match self.auth(self.client.post(format!("{}/v1/messages", self.base))).json(&body).send().await {
            Ok(r) => check(r).await,
            Err(e) => Err(net(e)),
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(ChatEvent::Error(e.to_string()));
                return Err(e);
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf = LineBuffer::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(net)?;
            for line in buf.push(&chunk) {
                if let Some(ev) = parse_anthropic_line(&line)? {
                    let end = matches!(ev, ChatEvent::Done | ChatEvent::Error(_));
                    let _ = tx.send(ev);
                    if end {
                        return Ok(());
                    }
                }
            }
        }
        let _ = tx.send(ChatEvent::Done);
        Ok(())
    }
}

/// OpenAI's hosted API, using the OpenAI-compatible client with a bearer key.
pub fn openai(key: impl Into<String>) -> OpenAiCompat {
    OpenAiCompat::new(OPENAI_BASE, Some(key.into()), true)
}

/// Sends each request to the right backend based on the model reference. Listing models returns the local ones.
pub struct Router {
    local: Arc<dyn Provider>,
    anthropic: Option<Arc<dyn Provider>>,
    openai: Option<Arc<dyn Provider>>,
}

impl Router {
    pub fn new(local: Arc<dyn Provider>, anthropic: Option<Arc<dyn Provider>>, openai: Option<Arc<dyn Provider>>) -> Self {
        Self { local, anthropic, openai }
    }

    fn pick<'a>(&'a self, model: &'a str) -> Result<(&'a dyn Provider, &'a str), ProviderError> {
        if let Some(m) = model.strip_prefix(ANTHROPIC_PREFIX) {
            return self.anthropic.as_deref().map(|p| (p, m)).ok_or_else(|| ProviderError::Api("Claude is not set up. Add an API key in Models, Cloud providers.".into()));
        }
        if let Some(m) = model.strip_prefix(OPENAI_PREFIX) {
            return self.openai.as_deref().map(|p| (p, m)).ok_or_else(|| ProviderError::Api("OpenAI is not set up. Add an API key in Models, Cloud providers.".into()));
        }
        Ok((self.local.as_ref(), model))
    }
}

#[async_trait]
impl Provider for Router {
    fn id(&self) -> &str {
        "router"
    }
    fn is_cloud(&self) -> bool {
        self.local.is_cloud()
    }
    fn is_cloud_model(&self, model: &str) -> bool {
        is_cloud_ref(model) || self.local.is_cloud_model(model)
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.local.list_models().await
    }
    async fn chat(&self, mut req: ChatRequest, tx: UnboundedSender<ChatEvent>) -> Result<(), ProviderError> {
        let name = req.model.clone();
        let (provider, model) = match self.pick(&name) {
            Ok((p, m)) => (p, m.to_string()),
            Err(e) => {
                let _ = tx.send(ChatEvent::Error(e.to_string()));
                return Err(e);
            }
        };
        req.model = model;
        provider.chat(req, tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aio_core::provider::EchoProvider;
    use aio_core::ChatMessage;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn anthropic_stream_lines() {
        assert!(matches!(parse_anthropic_line(r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#).unwrap(), Some(ChatEvent::Token(t)) if t == "Hi"));
        assert!(matches!(parse_anthropic_line(r#"data: {"type":"message_stop"}"#).unwrap(), Some(ChatEvent::Done)));
        assert!(matches!(parse_anthropic_line(r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#).unwrap(), Some(ChatEvent::Error(m)) if m == "Overloaded"));
        assert!(parse_anthropic_line("event: ping").unwrap().is_none());
        assert!(parse_anthropic_line(r#"data: {"type":"ping"}"#).unwrap().is_none());
    }

    #[test]
    fn refs_and_names() {
        assert!(is_cloud_ref("anthropic/claude-sonnet-5-5") && is_cloud_ref("openai/gpt-4o"));
        assert!(!is_cloud_ref("llama3.2:3b") && !is_cloud_ref("hf.co/user/model"));
        assert_eq!(display_ref("anthropic/claude-haiku-5-5"), "Claude · claude-haiku-5-5");
    }

    #[tokio::test]
    async fn router_dispatches_and_reports_missing_keys() {
        let r = Router::new(Arc::new(EchoProvider), None, None);
        assert!(!r.is_cloud_model("echo"));
        assert!(r.is_cloud_model("openai/gpt-4o"));
        let (tx, mut rx) = unbounded_channel();
        r.chat(ChatRequest { model: "echo".into(), messages: vec![ChatMessage::new(Role::User, "hi")] }, tx).await.unwrap();
        assert!(matches!(rx.recv().await, Some(ChatEvent::Token(_))));
        let (tx, mut rx) = unbounded_channel();
        let err = r.chat(ChatRequest { model: "anthropic/x".into(), messages: vec![] }, tx).await.unwrap_err();
        assert!(err.to_string().contains("not set up"));
        assert!(matches!(rx.recv().await, Some(ChatEvent::Error(_))));
    }
}
