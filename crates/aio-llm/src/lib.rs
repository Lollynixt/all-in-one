//! Provider implementations. Ollama (native API) and OpenAI-compatible servers
//! (llama.cpp server, LM Studio, vLLM, OpenAI).
pub mod cloud;
pub mod launcher;

use aio_core::{ChatEvent, ChatRequest, ModelInfo, Provider, ProviderError};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

/// Splits a byte stream into complete lines, keeping partial lines between chunks.
#[derive(Default)]
pub struct LineBuffer(Vec<u8>);

impl LineBuffer {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.0.extend_from_slice(chunk);
        let mut out = vec![];
        while let Some(i) = self.0.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.0.drain(..=i).collect();
            let s = String::from_utf8_lossy(&line).trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
        }
        out
    }
}

/// Parses one Ollama NDJSON chat line.
pub fn parse_ollama_line(line: &str) -> Result<Option<ChatEvent>, ProviderError> {
    let v: Value = serde_json::from_str(line).map_err(|e| ProviderError::Decode(e.to_string()))?;
    if let Some(err) = v.get("error").and_then(Value::as_str) {
        return Ok(Some(ChatEvent::Error(err.to_string())));
    }
    if v.get("done").and_then(Value::as_bool) == Some(true) {
        return Ok(Some(ChatEvent::Done));
    }
    Ok(v.pointer("/message/content").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| ChatEvent::Token(s.to_string())))
}

/// Parses one OpenAI-style SSE line (`data: {...}` or `data: [DONE]`).
pub fn parse_sse_line(line: &str) -> Result<Option<ChatEvent>, ProviderError> {
    let Some(data) = line.strip_prefix("data:") else { return Ok(None) };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(Some(ChatEvent::Done));
    }
    let v: Value = serde_json::from_str(data).map_err(|e| ProviderError::Decode(e.to_string()))?;
    Ok(v.pointer("/choices/0/delta/content").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| ChatEvent::Token(s.to_string())))
}

/// Progress of `ollama pull`.
#[derive(Debug, Clone, PartialEq)]
pub enum PullEvent {
    Status { text: String, completed: u64, total: u64 },
    Done,
    Error(String),
}

pub fn parse_pull_line(line: &str) -> Result<Option<PullEvent>, ProviderError> {
    let v: Value = serde_json::from_str(line).map_err(|e| ProviderError::Decode(e.to_string()))?;
    if let Some(err) = v.get("error").and_then(Value::as_str) {
        return Ok(Some(PullEvent::Error(err.to_string())));
    }
    let status = v.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "success" {
        return Ok(Some(PullEvent::Done));
    }
    if status.is_empty() {
        return Ok(None);
    }
    Ok(Some(PullEvent::Status {
        text: status.to_string(),
        completed: v.get("completed").and_then(Value::as_u64).unwrap_or(0),
        total: v.get("total").and_then(Value::as_u64).unwrap_or(0),
    }))
}

fn net(e: reqwest::Error) -> ProviderError {
    ProviderError::Unreachable(e.to_string())
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(ProviderError::Api(format!("{status}: {}", body.chars().take(300).collect::<String>())))
}

pub struct Ollama {
    base: String,
    client: reqwest::Client,
}

impl Ollama {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), client: reqwest::Client::new() }
    }
    pub fn local() -> Self {
        Self::new("http://127.0.0.1:11434")
    }
}

#[async_trait]
impl Provider for Ollama {
    fn id(&self) -> &str {
        "ollama"
    }
    fn is_cloud(&self) -> bool {
        // Ollama on loopback is local. A remote base URL is treated as off-machine.
        !(self.base.contains("127.0.0.1") || self.base.contains("localhost") || self.base.contains("[::1]"))
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let resp = check(self.client.get(format!("{}/api/tags", self.base)).send().await.map_err(net)?).await?;
        let v: Value = resp.json().await.map_err(|e| ProviderError::Decode(e.to_string()))?;
        Ok(v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| Some(ModelInfo { name: m["name"].as_str()?.to_string(), size_bytes: m["size"].as_u64().unwrap_or(0) }))
                    .collect()
            })
            .unwrap_or_default())
    }
    async fn chat(&self, req: ChatRequest, tx: UnboundedSender<ChatEvent>) -> Result<(), ProviderError> {
        let body = json!({ "model": req.model, "messages": req.messages, "stream": true });
        let resp = match self.client.post(format!("{}/api/chat", self.base)).json(&body).send().await {
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
                if let Some(ev) = parse_ollama_line(&line)? {
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

impl Ollama {
    /// Downloads a model. Interrupting (dropping the future) is safe: Ollama resumes partial downloads.
    pub async fn pull(&self, model: &str, tx: UnboundedSender<PullEvent>) -> Result<(), ProviderError> {
        let body = json!({ "model": model, "stream": true });
        let resp = check(self.client.post(format!("{}/api/pull", self.base)).json(&body).send().await.map_err(net)?).await?;
        let mut stream = resp.bytes_stream();
        let mut buf = LineBuffer::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(net)?;
            for line in buf.push(&chunk) {
                if let Some(ev) = parse_pull_line(&line)? {
                    let end = matches!(ev, PullEvent::Done | PullEvent::Error(_));
                    let _ = tx.send(ev);
                    if end {
                        return Ok(());
                    }
                }
            }
        }
        Err(ProviderError::Api("download ended before Ollama reported success".into()))
    }
}

pub struct OpenAiCompat {
    base: String,
    api_key: Option<String>,
    cloud: bool,
    client: reqwest::Client,
}

impl OpenAiCompat {
    /// `base` includes the version prefix, e.g. `http://127.0.0.1:8080/v1`.
    pub fn new(base: impl Into<String>, api_key: Option<String>, cloud: bool) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), api_key, cloud, client: reqwest::Client::new() }
    }
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        }
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> &str {
        "openai-compatible"
    }
    fn is_cloud(&self) -> bool {
        self.cloud
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let resp = check(self.auth(self.client.get(format!("{}/models", self.base))).send().await.map_err(net)?).await?;
        let v: Value = resp.json().await.map_err(|e| ProviderError::Decode(e.to_string()))?;
        Ok(v["data"].as_array().map(|a| a.iter().filter_map(|m| Some(ModelInfo { name: m["id"].as_str()?.to_string(), size_bytes: 0 })).collect()).unwrap_or_default())
    }
    async fn chat(&self, req: ChatRequest, tx: UnboundedSender<ChatEvent>) -> Result<(), ProviderError> {
        let body = json!({ "model": req.model, "messages": req.messages, "stream": true });
        let resp = match self.auth(self.client.post(format!("{}/chat/completions", self.base))).json(&body).send().await {
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
                if let Some(ev) = parse_sse_line(&line)? {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_buffer_handles_split_chunks() {
        let mut b = LineBuffer::default();
        assert!(b.push(b"{\"a\":1}\n{\"b\"").len() == 1);
        let rest = b.push(b":2}\n");
        assert_eq!(rest, vec!["{\"b\":2}".to_string()]);
    }

    #[test]
    fn ollama_lines() {
        assert!(matches!(parse_ollama_line(r#"{"message":{"content":"Hi"},"done":false}"#).unwrap(), Some(ChatEvent::Token(t)) if t == "Hi"));
        assert!(matches!(parse_ollama_line(r#"{"done":true}"#).unwrap(), Some(ChatEvent::Done)));
        assert!(matches!(parse_ollama_line(r#"{"error":"model not found"}"#).unwrap(), Some(ChatEvent::Error(_))));
        assert!(parse_ollama_line("not json").is_err());
    }

    #[test]
    fn sse_lines() {
        assert!(matches!(parse_sse_line(r#"data: {"choices":[{"delta":{"content":"Yo"}}]}"#).unwrap(), Some(ChatEvent::Token(t)) if t == "Yo"));
        assert!(matches!(parse_sse_line("data: [DONE]").unwrap(), Some(ChatEvent::Done)));
        assert!(parse_sse_line(": keep-alive").unwrap().is_none());
    }

    #[test]
    fn pull_lines() {
        assert_eq!(parse_pull_line(r#"{"status":"pulling manifest"}"#).unwrap(), Some(PullEvent::Status { text: "pulling manifest".into(), completed: 0, total: 0 }));
        assert_eq!(
            parse_pull_line(r#"{"status":"pulling abc","digest":"abc","total":1000,"completed":250}"#).unwrap(),
            Some(PullEvent::Status { text: "pulling abc".into(), completed: 250, total: 1000 })
        );
        assert_eq!(parse_pull_line(r#"{"status":"success"}"#).unwrap(), Some(PullEvent::Done));
        assert!(matches!(parse_pull_line(r#"{"error":"pull model manifest: file does not exist"}"#).unwrap(), Some(PullEvent::Error(_))));
        assert!(parse_pull_line("garbage").is_err());
    }

    #[test]
    fn loopback_is_local() {
        assert!(!Ollama::local().is_cloud());
        assert!(Ollama::new("http://192.168.1.5:11434").is_cloud());
    }
}
