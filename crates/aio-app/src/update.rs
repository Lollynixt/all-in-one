//! In-app updates: finds the newest all-in-one-prototype*.tar.gz in Downloads and runs the project's update.sh on it.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, PartialEq)]
pub struct Tarball {
    pub path: PathBuf,
    pub modified: SystemTime,
}

#[derive(Debug)]
pub enum UpdateEvent {
    Line(String),
    Finished(Result<(), String>),
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

pub fn downloads_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Downloads"))
}

/// Newest `all-in-one-prototype*.tar.gz` in `dir` (browsers add " (1)" to repeat downloads).
pub fn newest_tarball(dir: &Path) -> Option<Tarball> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with("all-in-one-prototype") && n.ends_with(".tar.gz")
        })
        .filter_map(|e| Some(Tarball { modified: e.metadata().ok()?.modified().ok()?, path: e.path() }))
        .max_by_key(|t| t.modified)
}

pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The project folder this binary was built from, when it still has update.sh.
pub fn project_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2)?.to_path_buf();
    root.join("update.sh").is_file().then_some(root)
}

pub fn exe_secs() -> u64 {
    std::env::current_exe().ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()).map_or(0, unix_secs)
}

/// An update is available when the tarball is newer than both this build and the last applied update.
pub fn is_available(tarball: &Tarball, exe_secs: u64, applied_secs: u64) -> bool {
    unix_secs(tarball.modified) > exe_secs.max(applied_secs) + 2
}

pub fn ago(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).unwrap_or(Duration::ZERO).as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

/// Runs `update.sh <tarball> --no-run`, streaming its output. Builds the release binary but does not launch it.
pub async fn run_update(root: PathBuf, tarball: PathBuf, tx: UnboundedSender<UpdateEvent>) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;
    let done = |r: Result<(), String>| {
        let _ = tx.send(UpdateEvent::Finished(r));
    };
    if !cfg!(unix) {
        return done(Err("In-app update works on macOS and Linux for now. On Windows, unpack the tarball over the project folder and run cargo build --release.".into()));
    }
    let mut path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(h) = home() {
        let mut p = std::ffi::OsString::from(h.join(".cargo/bin"));
        p.push(":");
        p.push(&path);
        path = p;
    }
    let mut child = match Command::new("bash")
        .arg(root.join("update.sh"))
        .arg(&tarball)
        .arg("--no-run")
        .current_dir(&root)
        .env("PATH", path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return done(Err(format!("could not start update.sh: {e}"))),
    };
    let mut readers = vec![];
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(UpdateEvent::Line(l));
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(UpdateEvent::Line(l));
            }
        }));
    }
    let status = child.wait().await;
    for r in readers {
        let _ = r.await;
    }
    match status {
        Ok(s) if s.success() => done(Ok(())),
        Ok(s) => done(Err(format!("update.sh exited with {s}. Your previous version is untouched or kept in all-in-one.prev."))),
        Err(e) => done(Err(e.to_string())),
    }
}

/// Starts a binary (the freshly installed or built version). The caller then closes the running app.
pub fn launch_exe(bin: &Path) -> Result<(), String> {
    if !bin.is_file() {
        return Err(format!("{} not found", bin.display()));
    }
    std::process::Command::new(bin).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Where `update.sh` leaves the release build.
pub fn built_binary(root: &Path) -> PathBuf {
    root.join("target/release").join(if cfg!(windows) { "all-in-one.exe" } else { "all-in-one" })
}

/// Opens a folder in the system file manager.
pub fn open_folder(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_matching_tarball() {
        let dir = std::env::temp_dir().join(format!("aio-upd-{}", unix_secs(SystemTime::now())));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(newest_tarball(&dir).is_none());
        std::fs::write(dir.join("other.tar.gz"), "x").unwrap();
        std::fs::write(dir.join("all-in-one-prototype.tar.gz"), "old").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(dir.join("all-in-one-prototype (1).tar.gz"), "new").unwrap();
        let t = newest_tarball(&dir).unwrap();
        assert!(t.path.ends_with("all-in-one-prototype (1).tar.gz"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn availability_rules() {
        let t = Tarball { path: "x".into(), modified: UNIX_EPOCH + Duration::from_secs(1000) };
        assert!(is_available(&t, 900, 0));
        assert!(!is_available(&t, 1000, 0)); // built after the download
        assert!(!is_available(&t, 0, 1000)); // already applied
        assert!(is_available(&t, 0, 0));
    }

    #[test]
    fn ago_text() {
        assert_eq!(ago(SystemTime::now()), "just now");
        assert_eq!(ago(SystemTime::now() - Duration::from_secs(300)), "5 min ago");
        assert_eq!(ago(SystemTime::now() - Duration::from_secs(7300)), "2 h ago");
    }
}
