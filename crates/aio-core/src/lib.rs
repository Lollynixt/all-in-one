//! UI-free core of All In One.
pub mod engine;
pub mod hardware;
pub mod nodes;
pub mod notes;
pub mod provider;
pub mod workflow;

pub use engine::{run_workflow, RunEvent, RunOptions};
pub use provider::{ChatEvent, ChatMessage, ChatRequest, ModelInfo, Provider, ProviderError, Role};
pub use workflow::*;
