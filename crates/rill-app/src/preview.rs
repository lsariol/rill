//! Link previews: a page's title, description and picture, fetched when a link is hovered (never
//! when a message arrives, so only links you point at are visited).

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use futures::AsyncReadExt as _;
use gpui::http_client::{AsyncBody, HttpClient, Url};

/// How much of a page is read looking for its preview.
const MAX_BYTES: usize = 256 * 1024;
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);
/// Previews kept, newest last.
const KEEP: usize = 200;

/// What a page says about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Preview {
    pub site: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum State {
    Loading,
    Ready(Preview),
    /// Nothing to show beyond the address.
    Failed,
}

/// Previews by address.
#[derive(Default)]
pub(crate) struct Previews {
    by_url: HashMap<String, State>,
    order: VecDeque<String>,
}

impl Previews {
    pub fn get(&self, url: &str) -> Option<&State> {
        self.by_url.get(url)
    }

    pub fn set(&mut self, url: String, state: State) {
        if self.by_url.insert(url.clone(), state).is_none() {
            self.order.push_back(url);
            if self.order.len() > KEEP
                && let Some(old) = self.order.pop_front()
            {
                self.by_url.remove(&old);
            }
        }
    }
}

/// Whether a preview may be fetched: web addresses only, and nothing on this computer or the
/// local network.
pub(crate) fn may_fetch(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    match u.host() {
        Some(gpui::http_client::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            let local = d == "localhost"
                || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
                    .iter()
                    .any(|s| d.ends_with(s));
            !local && d.contains('.')
        }
        Some(gpui::http_client::Host::Ipv4(ip)) => public_v4(ip),
        Some(gpui::http_client::Host::Ipv6(ip)) => public_v6(ip),
        None => false,
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80)
}

/// Fetches a page's preview. The caller limits how long it may take (`TIMEOUT`).
pub(crate) async fn fetch(client: Arc<dyn HttpClient>, url: String) -> State {
    let mut response = match client.get(&url, AsyncBody::empty(), true).await {
        Ok(r) if r.status().is_success() => r,
        _ => return State::Failed,
    };
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if kind.starts_with("image/") {
        return State::Ready(Preview {
            image: Some(url),
            ..Preview::default()
        });
    }
    if !kind.contains("html") {
        return State::Failed;
    }
    let mut body = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    while body.len() < MAX_BYTES {
        let n = match response.body_mut().read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        body.extend_from_slice(&chunk[..n]);
        // The preview is in the head; stop once it's over.
        if find_ci(&String::from_utf8_lossy(&body), "</head").is_some() {
            break;
        }
    }
    match parse(&url, &String::from_utf8_lossy(&body)) {
        p if p == Preview::default() => State::Failed,
        p => State::Ready(p),
    }
}

/// Reads a page's Open Graph (or plain HTML) title, description, picture and site name.
pub(crate) fn parse(url: &str, html: &str) -> Preview {
    let head = match find_ci(html, "</head") {
        Some(end) => &html[..end],
        None => html,
    };
    let mut p = Preview::default();
    let mut fallback_description = None;
    for (key, content) in meta_tags(head) {
        let content = clean(&content);
        if content.is_empty() {
            continue;
        }
        match key.as_str() {
            "og:title" | "twitter:title" if p.title.is_none() => p.title = Some(content),
            "og:description" | "twitter:description" if p.description.is_none() => {
                p.description = Some(content)
            }
            "description" => fallback_description = Some(content),
            "og:image" | "og:image:url" | "og:image:secure_url" | "twitter:image"
                if p.image.is_none() =>
            {
                p.image = absolute(url, &content).filter(|u| may_fetch(u));
            }
            "og:site_name" if p.site.is_none() => p.site = Some(content),
            _ => {}
        }
    }
    if p.description.is_none() {
        p.description = fallback_description;
    }
    if p.title.is_none()
        && let Some(start) = find_ci(head, "<title")
        && let Some(open) = head[start..].find('>')
    {
        let rest = &head[start + open + 1..];
        let end = find_ci(rest, "</title").unwrap_or(rest.len());
        let title = clean(&rest[..end]);
        p.title = (!title.is_empty()).then_some(title);
    }
    p
}

/// `<meta>` tags' `property` (or `name`) and `content`, lowercased keys.
fn meta_tags(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = find_ci(rest, "<meta") {
        rest = &rest[at + 5..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        let key = attr(tag, "property").or_else(|| attr(tag, "name"));
        if let (Some(key), Some(content)) = (key, attr(tag, "content")) {
            out.push((key.to_ascii_lowercase(), content));
        }
        rest = &rest[end..];
    }
    out
}

/// An attribute's value in a tag's text, quoted or not.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value_at = tag.len() - after.len() + 1;
        let value = tag[value_at..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or("").to_string(),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_string(),
        });
    }
    None
}

/// Decodes the common HTML entities and squeezes whitespace.
fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(10)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn absolute(base: &str, href: &str) -> Option<String> {
    Url::parse(base)
        .ok()?
        .join(href)
        .ok()
        .map(|u| u.to_string())
}

/// Finds `needle` (ASCII) in `hay` ignoring case.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    (0..=h.len().checked_sub(n.len())?).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_open_graph_tags() {
        let html = r#"<html><head><title>Fallback</title>
            <meta property="og:title" content="A &amp; B" />
            <meta name="description" content="plain description">
            <meta property='og:image' content='/thumb.jpg'>
            <meta property="og:site_name" content="Example">
            </head><body><meta property="og:title" content="not this"></body></html>"#;
        let p = parse("https://example.com/watch?v=1", html);
        assert_eq!(p.title.as_deref(), Some("A & B"));
        assert_eq!(p.description.as_deref(), Some("plain description"));
        assert_eq!(p.image.as_deref(), Some("https://example.com/thumb.jpg"));
        assert_eq!(p.site.as_deref(), Some("Example"));
    }

    #[test]
    fn falls_back_to_the_title_tag() {
        let p = parse(
            "https://x.io",
            "<HEAD><TITLE>\n  Hello &#39;there&#x27; </TITLE></HEAD>",
        );
        assert_eq!(p.title.as_deref(), Some("Hello 'there'"));
    }

    #[test]
    fn never_fetches_this_computer_or_the_local_network() {
        assert!(may_fetch("https://www.youtube.com/watch?v=1"));
        assert!(may_fetch("http://93.184.216.34/"));
        for url in [
            "http://localhost:8080",
            "http://127.0.0.1/",
            "http://192.168.1.1/admin",
            "http://10.0.0.5",
            "http://169.254.169.254/latest",
            "http://[::1]/",
            "http://router.local/",
            "file:///c:/windows",
            "ftp://example.com",
            "http://intranet/",
        ] {
            assert!(!may_fetch(url), "{url}");
        }
    }

    #[test]
    fn keeps_the_newest_previews() {
        let mut p = Previews::default();
        for i in 0..KEEP + 1 {
            p.set(format!("u{i}"), State::Failed);
        }
        assert!(p.get("u0").is_none());
        assert!(p.get(&format!("u{KEEP}")).is_some());
    }
}
