use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("cannot reach provider: {0}")]
    Unreachable(String),
    #[error("provider returned an error: {0}")]
    Api(String),
    #[error("bad response: {0}")]
    Decode(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self { role, content: content.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone)]
pub enum ChatEvent {
    Token(String),
    Done,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub name: String,
    pub size_bytes: u64,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    /// True when prompts leave this machine.
    fn is_cloud(&self) -> bool;
    /// True when prompts for this particular model leave the machine. Routers override this.
    fn is_cloud_model(&self, _model: &str) -> bool {
        self.is_cloud()
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    /// Streams events into `tx`; always ends with `Done` or `Error`.
    async fn chat(&self, req: ChatRequest, tx: UnboundedSender<ChatEvent>) -> Result<(), ProviderError>;
}

/// Deterministic provider for tests and offline demos.
pub struct EchoProvider;

#[async_trait]
impl Provider for EchoProvider {
    fn id(&self) -> &str {
        "echo"
    }
    fn is_cloud(&self) -> bool {
        false
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![ModelInfo { name: "echo".into(), size_bytes: 0 }])
    }
    async fn chat(&self, req: ChatRequest, tx: UnboundedSender<ChatEvent>) -> Result<(), ProviderError> {
        let last = req.messages.last().map(|m| m.content.clone()).unwrap_or_default();
        for word in format!("[echo] {last}").split_inclusive(' ') {
            let _ = tx.send(ChatEvent::Token(word.to_string()));
        }
        let _ = tx.send(ChatEvent::Done);
        Ok(())
    }
}
