//! Self-update from GitHub Releases: find the newest release, download the asset for this platform,
//! verify its SHA-256, and swap the running binary. Works with public repos and, with a token, private ones.
use crate::update::UpdateEvent;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    /// API URL of the asset (works for private repos with a token).
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

// ------------------------------------------------------------------ versions

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pre {
    Num(u64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    core: (u64, u64, u64),
    pre: Vec<Pre>,
}

impl Version {
    /// Parses `v1.2.3`, `1.2.3`, `1.2.3-beta.1` (build metadata after `+` is ignored).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().map_or(Some(0), |v| v.parse().ok())?;
        let patch = it.next().map_or(Some(0), |v| v.parse().ok())?;
        if it.next().is_some() {
            return None;
        }
        let pre = pre.map(|p| p.split('.').map(|id| id.parse().map(Pre::Num).unwrap_or_else(|_| Pre::Text(id.to_string()))).collect()).unwrap_or_default();
        Some(Version { core: (major, minor, patch), pre })
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.core.cmp(&o.core).then_with(|| match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater, // 1.0.0 is newer than 1.0.0-beta
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&o.pre) {
                    let c = match (a, b) {
                        (Pre::Num(x), Pre::Num(y)) => x.cmp(y),
                        (Pre::Num(_), Pre::Text(_)) => Ordering::Less,
                        (Pre::Text(_), Pre::Num(_)) => Ordering::Greater,
                        (Pre::Text(x), Pre::Text(y)) => x.cmp(y),
                    };
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                self.pre.len().cmp(&o.pre.len())
            }
        })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

pub fn is_newer(release_tag: &str, current: &str) -> bool {
    matches!((Version::parse(release_tag), Version::parse(current)), (Some(r), Some(c)) if r > c)
}

// ------------------------------------------------------------------ platform and assets

/// Rust target triple of this build, for choosing the right release asset.
pub fn platform_target() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

pub fn binary_name() -> &'static str {
    if cfg!(windows) { "all-in-one.exe" } else { "all-in-one" }
}

/// The archive for `target` and its checksum file.
pub fn assets_for<'a>(r: &'a Release, target: &str) -> Option<(&'a Asset, Option<&'a Asset>)> {
    let name = format!("all-in-one-{target}.tar.gz");
    let archive = r.assets.iter().find(|a| a.name == name)?;
    let sum = r.assets.iter().find(|a| a.name == format!("{name}.sha256"));
    Some((archive, sum))
}

// ------------------------------------------------------------------ GitHub API

pub fn api_base() -> String {
    std::env::var("AIO_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into())
}

pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(timeout).user_agent("all-in-one-updater").build().map_err(|e| e.to_string())
}

fn authed(rb: reqwest::RequestBuilder, token: Option<&str>) -> reqwest::RequestBuilder {
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => rb.bearer_auth(t),
        None => rb,
    }
}

/// Newest eligible release by version (drafts never; pre-releases only when `beta`).
pub fn pick_latest(releases: Vec<Release>, beta: bool) -> Option<Release> {
    releases.into_iter().filter(|r| !r.draft && (beta || !r.prerelease)).filter_map(|r| Some((Version::parse(&r.tag_name)?, r))).max_by(|a, b| a.0.cmp(&b.0)).map(|(_, r)| r)
}

pub async fn fetch_latest(repo: &str, token: Option<&str>, beta: bool) -> Result<Option<Release>, String> {
    if !valid_repo(repo) {
        return Err("Repository should look like owner/name".into());
    }
    let url = format!("{}/repos/{repo}/releases?per_page=20", api_base());
    let resp = authed(client(Duration::from_secs(20))?.get(url).header("Accept", "application/vnd.github+json"), token).send().await.map_err(|e| format!("could not reach GitHub: {e}"))?;
    let status = resp.status();
    match status.as_u16() {
        200 => {}
        404 => return Err("Repository not found. If it is private, add a GitHub token with read access to its contents.".into()),
        401 => return Err("GitHub rejected the token. Check it and try again.".into()),
        403 | 429 => return Err("GitHub refused the request (rate limit or missing permission). Adding a token helps.".into()),
        c => return Err(format!("GitHub returned HTTP {c}")),
    }
    let releases: Vec<Release> = resp.json().await.map_err(|e| format!("unexpected response from GitHub: {e}"))?;
    Ok(pick_latest(releases, beta))
}

// ------------------------------------------------------------------ verify, extract, install

pub fn verify_sha256(bytes: &[u8], checksum_file: &str) -> Result<(), String> {
    let expected = checksum_file.split_whitespace().next().unwrap_or("").to_lowercase();
    let actual = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
    if expected.len() == 64 && expected == actual {
        Ok(())
    } else {
        Err("Checksum does not match. The download is corrupt or has been tampered with, so it was not installed.".into())
    }
}

pub fn extract_binary(tar_gz: &[u8], bin_name: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let is_it = entry.path().map_err(|e| e.to_string())?.file_name().is_some_and(|n| n == bin_name);
        if is_it {
            let mut out = Vec::new();
            entry.by_ref().take(MAX_DOWNLOAD).read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok(out);
        }
    }
    Err(format!("{bin_name} not found in the downloaded archive"))
}

/// Replaces `exe` with `new_bytes`, keeping the old one as `<exe>.prev`. Safe while `exe` is running
/// (the old process keeps its open file on Unix; Windows allows renaming a running executable).
pub fn install_binary(new_bytes: &[u8], exe: &Path) -> Result<PathBuf, String> {
    let dir = exe.parent().ok_or("cannot find the app folder")?;
    let name = exe.file_name().ok_or("bad executable path")?.to_string_lossy().into_owned();
    let tmp = dir.join(format!("{name}.new"));
    let prev = dir.join(format!("{name}.prev"));
    std::fs::write(&tmp, new_bytes).map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&prev);
    let had_old = exe.exists();
    if had_old {
        std::fs::rename(exe, &prev).map_err(|e| format!("cannot move the current version aside: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&tmp, exe) {
        if had_old {
            let _ = std::fs::rename(&prev, exe); // put it back
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot install the new version: {e}"));
    }
    Ok(prev)
}

async fn download(asset: &Asset, token: Option<&str>, tx: &UnboundedSender<UpdateEvent>) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    if asset.size > MAX_DOWNLOAD {
        return Err("asset is unexpectedly large".into());
    }
    let resp = authed(client(Duration::from_secs(900))?.get(&asset.url).header("Accept", "application/octet-stream"), token).send().await.map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status().as_u16()));
    }
    let total = resp.content_length().unwrap_or(asset.size);
    let mut got: u64 = 0;
    let mut next_report = 25u64;
    let mut buf = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download failed: {e}"))?;
        got += chunk.len() as u64;
        if got > MAX_DOWNLOAD {
            return Err("download is larger than expected".into());
        }
        buf.extend_from_slice(&chunk);
        if total > 0 {
            let pct = got * 100 / total;
            if pct >= next_report {
                let _ = tx.send(UpdateEvent::Line(format!("  {pct}% ({:.1} MB)", got as f64 / 1e6)));
                next_report += 25;
            }
        }
    }
    Ok(buf)
}

/// Downloads, verifies and installs the release for this platform, reporting progress through `tx`.
pub async fn install_release(release: Release, token: Option<String>, exe: PathBuf, tx: UnboundedSender<UpdateEvent>) {
    let say = |s: String| {
        let _ = tx.send(UpdateEvent::Line(s));
    };
    let result: Result<(), String> = async {
        let target = platform_target().ok_or("There is no prebuilt version for this platform yet.")?;
        let (archive, sum) = assets_for(&release, target).ok_or_else(|| format!("Release {} has no build for {target}. It may still be building; try again in a few minutes.", release.tag_name))?;
        let sum = sum.ok_or("The release has no checksum file, so it was not installed.")?;
        say(format!("Downloading {} {}", release.tag_name, archive.name));
        let bytes = download(archive, token.as_deref(), &tx).await?;
        say("Verifying checksum".into());
        let sum_bytes = download(sum, token.as_deref(), &tx).await?;
        verify_sha256(&bytes, &String::from_utf8_lossy(&sum_bytes))?;
        say("Installing".into());
        let name = binary_name();
        let prev = tokio::task::spawn_blocking(move || {
            let bin = extract_binary(&bytes, name)?;
            install_binary(&bin, &exe)
        })
        .await
        .map_err(|e| e.to_string())??;
        say(format!("Installed. The previous version is kept at {}", prev.display()));
        Ok(())
    }
    .await;
    let _ = tx.send(UpdateEvent::Finished(result));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool) -> Release {
        Release { tag_name: tag.into(), name: None, body: None, draft: false, prerelease: pre, assets: vec![] }
    }

    #[test]
    fn version_ordering() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(v("v0.2.0") > v("0.1.9"));
        assert!(v("1.0.0") > v("1.0.0-beta.3"));
        assert!(v("1.0.0-beta.10") > v("1.0.0-beta.2"));
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.9"));
        assert!(Version::parse("garbage").is_none());
        assert!(is_newer("v0.1.1", "0.1.0") && !is_newer("v0.1.0", "0.1.0") && !is_newer("v0.0.9", "0.1.0"));
    }

    #[test]
    fn latest_respects_channel_and_drafts() {
        let mut draft = rel("v9.0.0", false);
        draft.draft = true;
        let list = vec![rel("v0.2.0", false), rel("v0.3.0-beta.1", true), draft, rel("v0.1.0", false)];
        assert_eq!(pick_latest(list.clone(), false).unwrap().tag_name, "v0.2.0");
        assert_eq!(pick_latest(list, true).unwrap().tag_name, "v0.3.0-beta.1");
        assert!(pick_latest(vec![], false).is_none());
    }

    #[test]
    fn asset_selection_and_repo_names() {
        let mut r = rel("v1.0.0", false);
        r.assets = vec![
            Asset { name: "all-in-one-aarch64-apple-darwin.tar.gz".into(), url: "u1".into(), size: 1 },
            Asset { name: "all-in-one-aarch64-apple-darwin.tar.gz.sha256".into(), url: "u2".into(), size: 1 },
            Asset { name: "all-in-one-x86_64-unknown-linux-gnu.tar.gz".into(), url: "u3".into(), size: 1 },
        ];
        let (a, s) = assets_for(&r, "aarch64-apple-darwin").unwrap();
        assert_eq!((a.url.as_str(), s.map(|s| s.url.as_str())), ("u1", Some("u2")));
        assert!(assets_for(&r, "x86_64-unknown-linux-gnu").unwrap().1.is_none());
        assert!(assets_for(&r, "x86_64-pc-windows-msvc").is_none());
        assert!(valid_repo("octocat/all-in-one") && !valid_repo("nope") && !valid_repo("a/b/c") && !valid_repo("a/ b"));
    }

    fn make_targz(name: &str, content: &[u8]) -> Vec<u8> {
        let mut tar_bytes = vec![];
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            let mut h = tar::Header::new_gnu();
            h.set_size(content.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, name, content).unwrap();
            b.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn checksum_extract_and_swap() {
        let archive = make_targz("all-in-one", b"NEW BINARY");
        let sum = Sha256::digest(&archive).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert!(verify_sha256(&archive, &format!("{sum}  all-in-one-x.tar.gz\n")).is_ok());
        assert!(verify_sha256(&archive, &"0".repeat(64)).is_err());
        assert!(verify_sha256(&archive, "").is_err());

        let bin = extract_binary(&archive, "all-in-one").unwrap();
        assert_eq!(bin, b"NEW BINARY");
        assert!(extract_binary(&archive, "other").is_err());

        let dir = std::env::temp_dir().join(format!("aio-rel-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("all-in-one");
        std::fs::write(&exe, b"OLD").unwrap();
        let prev = install_binary(&bin, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW BINARY");
        assert_eq!(std::fs::read(prev).unwrap(), b"OLD");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111, 0o111);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
