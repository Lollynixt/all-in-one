//! Finds and starts a local Ollama server so users never have to open a terminal.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub enum StartOutcome {
    AlreadyRunning,
    /// We started it. `Some(child)` if we own the process (and should stop it on exit).
    Started(Option<Child>),
    NotInstalled,
    Failed(String),
}

pub fn is_loopback(base: &str) -> bool {
    let host = host_port(base).unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host.as_str(), |(h, _)| h).trim_matches(|c| c == '[' || c == ']').to_string();
    matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
}

/// `http://127.0.0.1:11434/x` becomes `127.0.0.1:11434`.
pub fn host_port(base: &str) -> Option<String> {
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let hp = rest.split('/').next()?.trim();
    (!hp.is_empty()).then(|| hp.to_string())
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "ollama.exe" } else { "ollama" }
}

pub fn find_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|p| p.join(exe_name())));
    }
    // Apps launched from Finder or the Start menu get a short PATH, so list the usual install spots too.
    for p in ["/opt/homebrew/bin/ollama", "/usr/local/bin/ollama", "/usr/bin/ollama", "/Applications/Ollama.app/Contents/Resources/ollama"] {
        candidates.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Ollama.app/Contents/Resources/ollama"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("Programs").join("Ollama").join("ollama.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub async fn is_up(base: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_millis(1500)).build() else { return false };
    client.get(format!("{}/api/version", base.trim_end_matches('/'))).send().await.is_ok_and(|r| r.status().is_success())
}

pub async fn ensure_running(base: &str) -> StartOutcome {
    if is_up(base).await {
        return StartOutcome::AlreadyRunning;
    }
    if !is_loopback(base) {
        return StartOutcome::Failed(format!("Ollama at {base} is not reachable. Remote servers are not started automatically."));
    }
    let mut child = None;
    if let Some(bin) = find_binary() {
        let mut cmd = Command::new(bin);
        cmd.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(hp) = host_port(base) {
            cmd.env("OLLAMA_HOST", hp);
        }
        match cmd.spawn() {
            Ok(c) => child = Some(c),
            Err(e) => return StartOutcome::Failed(format!("Could not start Ollama: {e}")),
        }
    } else if cfg!(target_os = "macos") && Path::new("/Applications/Ollama.app").exists() {
        let _ = Command::new("open").args(["-a", "Ollama"]).spawn();
    } else {
        return StartOutcome::NotInstalled;
    }
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if is_up(base).await {
            return StartOutcome::Started(child);
        }
    }
    if let Some(mut c) = child {
        let _ = c.kill();
    }
    StartOutcome::Failed("Ollama did not start within 20 seconds".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("http://127.0.0.1:11434"));
        assert!(is_loopback("http://localhost:11434/"));
        assert!(is_loopback("http://[::1]:11434"));
        assert!(!is_loopback("http://192.168.1.5:11434"));
        assert!(!is_loopback("https://ollama.example.com"));
    }

    #[test]
    fn host_port_extraction() {
        assert_eq!(host_port("http://127.0.0.1:11434/api").as_deref(), Some("127.0.0.1:11434"));
        assert_eq!(host_port("localhost:9").as_deref(), Some("localhost:9"));
        assert_eq!(host_port("http://"), None);
    }
}
