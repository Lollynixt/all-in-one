//! Persistent settings: cloud API keys, privacy switches, default model.
//!
//! Stored as JSON in the app folder with owner-only permissions on Unix. Keys can also come from the
//! ANTHROPIC_API_KEY / OPENAI_API_KEY environment variables, which are never written to disk.
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where in-app updates come from unless the user points elsewhere.
pub const DEFAULT_REPO: &str = "Lollynixt/all-in-one";

pub const DEFAULT_CLAUDE_MODELS: &[&str] = &["claude-fable-5-2", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub anthropic_key: String,
    pub openai_key: String,
    pub anthropic_models: Vec<String>,
    pub openai_models: Vec<String>,
    pub cloud_allowed: bool,
    pub allow_network: bool,
    pub chosen_model: Option<String>,
    /// Modified time (unix seconds) of the last tarball applied by the in-app updater.
    pub update_applied: u64,
    /// GitHub `owner/name` that publishes releases. Empty means the default repository.
    pub update_repo: String,
    /// Read-only token for private repositories.
    pub github_token: String,
    pub include_prereleases: bool,
    pub disable_auto_update_check: bool,
}

/// Debug output hides the API keys so they cannot end up in logs.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hide = |k: &str| if k.is_empty() { "unset" } else { "<hidden>" };
        f.debug_struct("Settings")
            .field("anthropic_key", &hide(&self.anthropic_key))
            .field("openai_key", &hide(&self.openai_key))
            .field("anthropic_models", &self.anthropic_models)
            .field("openai_models", &self.openai_models)
            .field("cloud_allowed", &self.cloud_allowed)
            .field("allow_network", &self.allow_network)
            .field("chosen_model", &self.chosen_model)
            .field("update_applied", &self.update_applied)
            .field("update_repo", &self.update_repo)
            .field("github_token", &hide(&self.github_token))
            .field("include_prereleases", &self.include_prereleases)
            .field("disable_auto_update_check", &self.disable_auto_update_check)
            .finish()
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("settings.json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(self).map_err(std::io::Error::other)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(tmp, dir.join("settings.json"))
    }

    /// Key from settings, falling back to the environment.
    pub fn anthropic(&self) -> Option<String> {
        non_empty(&self.anthropic_key).or_else(|| std::env::var("ANTHROPIC_API_KEY").ok().and_then(|k| non_empty(&k)))
    }
    pub fn openai(&self) -> Option<String> {
        non_empty(&self.openai_key).or_else(|| std::env::var("OPENAI_API_KEY").ok().and_then(|k| non_empty(&k)))
    }
    pub fn repo(&self) -> String {
        non_empty(&self.update_repo).unwrap_or_else(|| DEFAULT_REPO.to_string())
    }
    pub fn github(&self) -> Option<String> {
        non_empty(&self.github_token).or_else(|| std::env::var("GITHUB_TOKEN").ok().and_then(|k| non_empty(&k)))
    }
    pub fn claude_models(&self) -> Vec<String> {
        if self.anthropic_models.is_empty() { DEFAULT_CLAUDE_MODELS.iter().map(|s| s.to_string()).collect() } else { self.anthropic_models.clone() }
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("aio-settings-{}", uuid_like()));
        assert_eq!(Settings::load(&dir), Settings::default());
        let s = Settings { anthropic_key: " sk-ant-x ".into(), cloud_allowed: true, chosen_model: Some("openai/gpt-4o".into()), ..Default::default() };
        s.save(&dir).unwrap();
        let back = Settings::load(&dir);
        assert_eq!(back, s);
        assert_eq!(back.anthropic().as_deref(), Some("sk-ant-x"));
        assert_eq!(back.claude_models().len(), DEFAULT_CLAUDE_MODELS.len());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("settings.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    fn uuid_like() -> String {
        format!("{:?}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
    }
}
