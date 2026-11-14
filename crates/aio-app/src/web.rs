//! Text-mode page fetch used by the Browser tab until the embedded Servo view lands (Phase 0 spike).
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
}

pub fn normalise_url(input: &str) -> Result<String, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("enter an address".into());
    }
    if !t.contains("://") {
        // Reject non-web schemes such as `javascript:` or `mailto:`. `host:8080` is a port, not a scheme.
        if let Some((scheme, rest)) = t.split_once(':') {
            let is_scheme = !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphabetic());
            if is_scheme && !rest.starts_with(|c: char| c.is_ascii_digit()) {
                return Err("only http and https addresses are supported".into());
            }
        }
    }
    let url = if t.contains("://") { t.to_string() } else { format!("https://{t}") };
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url)
    } else {
        Err("only http and https addresses are supported".into())
    }
}

pub async fn fetch(url: &str) -> Result<Page, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("AllInOne/0.0.1 (text view)")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("server returned {}", resp.status()));
    }
    let final_url = resp.url().to_string();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let capped = &bytes[..bytes.len().min(400_000)];
    let html = String::from_utf8_lossy(capped).into_owned();
    let title = extract_title(&html).unwrap_or_else(|| final_url.clone());
    Ok(Page { url: final_url, title, text: html_to_text(&html) })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')? + start + 1;
    let end = lower[open_end..].find("</title>")? + open_end;
    let t = decode_entities(html.get(open_end..end)?.trim());
    (!t.is_empty()).then_some(t)
}

fn remove_blocks(src: &str, tag: &str) -> String {
    let lower = src.to_lowercase();
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = String::with_capacity(src.len());
    let mut pos = 0;
    while let Some(i) = lower[pos..].find(&open) {
        let start = pos + i;
        out.push_str(&src[pos..start]);
        match lower[start..].find(&close) {
            Some(j) => pos = start + j + close.len(),
            None => {
                pos = src.len();
                break;
            }
        }
    }
    out.push_str(&src[pos..]);
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

pub fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "head"] {
        s = remove_blocks(&s, tag);
    }
    // Turn block boundaries into newlines before stripping tags.
    let mut out = String::with_capacity(s.len() / 2);
    let mut in_tag = false;
    let mut tag_buf = String::new();
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag_buf.trim_start_matches('/').split(|ch: char| ch.is_whitespace() || ch == '/').next().unwrap_or("").to_lowercase();
                if matches!(name.as_str(), "p" | "br" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "section" | "article" | "ul" | "ol" | "table") {
                    out.push('\n');
                }
            }
            _ if in_tag => tag_buf.push(c),
            _ => out.push(c),
        }
    }
    let decoded = decode_entities(&out);
    let mut lines: Vec<String> = vec![];
    for l in decoded.lines() {
        let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() && lines.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        lines.push(l);
    }
    lines.join("\n").trim().to_string()
}

/// Opens an http(s) URL in the system browser. Anything else is refused.
pub fn open_external(url: &str) -> Result<(), String> {
    let url = normalise_url(url)?;
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&url).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_and_scripts() {
        let html = "<html><head><title>Hi &amp; bye</title><style>p{}</style></head><body><script>var x=1;</script><h1>Head</h1><p>One  <b>two</b></p><p>three</p></body></html>";
        assert_eq!(extract_title(html).as_deref(), Some("Hi & bye"));
        assert_eq!(html_to_text(html), "Head\n\nOne two\n\nthree");
    }

    #[test]
    fn url_rules() {
        assert_eq!(normalise_url("example.com").unwrap(), "https://example.com");
        assert!(normalise_url("file:///etc/passwd").is_err());
        assert!(normalise_url("javascript:alert(1)").is_err());
        assert!(normalise_url("  ").is_err());
        assert!(normalise_url("mailto:a@b.c").is_err());
        assert_eq!(normalise_url("localhost:8080/x").unwrap(), "https://localhost:8080/x");
    }
}
