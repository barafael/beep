//! Link previews for messages we send, when turned on: the linked page's title, description
//! and picture, as its `og:` meta tags (or its `<title>`) give them. The page is fetched
//! directly, so the site learns the user's address; that is why it is off by default.

use std::time::Duration;

/// What a page says about itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Page {
    pub title: String,
    pub description: String,
    /// The picture's address, made absolute.
    pub image: Option<String>,
}

/// Pages are read only this far; the head comes first.
const MAX_PAGE: usize = 512 * 1024;
/// Larger pictures are left out.
pub(crate) const MAX_IMAGE: usize = 2 * 1024 * 1024;

fn client() -> Option<reqwest::Client> {
    // As libsignal-service does, unless something installed one already.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (compatible; langis link preview)")
        .build()
        .ok()
}

/// Fetches what `url` (https only) says about itself; `None` if it can't be had or says
/// nothing.
pub(crate) async fn fetch_page(url: &str) -> Option<Page> {
    if !url.starts_with("https://") {
        return None;
    }
    let response = client()?
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let html_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|t| t.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"));
    if !html_type {
        return None;
    }
    let base = response.url().clone();
    let html = read_limited(response, MAX_PAGE).await?;
    let mut page = parse(&String::from_utf8_lossy(&html));
    page.image = page
        .image
        .and_then(|image| base.join(&image).ok())
        .filter(|image| image.scheme() == "https")
        .map(String::from);
    (!page.title.is_empty()).then_some(page)
}

/// Fetches a page's picture: its bytes and type, if it is a picture and not too large.
pub(crate) async fn fetch_image(url: &str) -> Option<(Vec<u8>, String)> {
    let response = client()?
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|t| t.to_str().ok())
        .filter(|t| t.starts_with("image/"))?
        .split(';')
        .next()?
        .trim()
        .to_owned();
    let bytes = read_limited(response, MAX_IMAGE + 1).await?;
    (bytes.len() <= MAX_IMAGE).then_some((bytes, content_type))
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() >= limit {
            body.truncate(limit);
            break;
        }
    }
    Some(body)
}

/// The title, description and picture a page's head gives.
pub(crate) fn parse(html: &str) -> Page {
    let head = html
        .to_ascii_lowercase()
        .find("</head")
        .map_or(html, |end| &html[..end]);
    let mut meta = std::collections::HashMap::new();
    let mut rest = head;
    while let Some(start) = find_ignoring_case(rest, "<meta") {
        let tag = &rest[start..];
        let end = tag.find('>').unwrap_or(tag.len());
        let attributes = attributes(&tag[5..end]);
        let key = attributes
            .iter()
            .find(|(name, _)| name == "property" || name == "name")
            .map(|(_, value)| value.to_ascii_lowercase());
        let content = attributes
            .iter()
            .find(|(name, _)| name == "content")
            .map(|(_, value)| value.clone());
        if let (Some(key), Some(content)) = (key, content) {
            meta.entry(key).or_insert(content);
        }
        rest = &tag[end..];
    }
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| meta.get(*key))
            .map(|value| decode_entities(value.trim()))
            .filter(|value| !value.is_empty())
    };
    let title = first(&["og:title", "twitter:title"]).or_else(|| {
        let start = find_ignoring_case(head, "<title")?;
        let after = &head[start..];
        let open = after.find('>')? + 1;
        let close = find_ignoring_case(&after[open..], "</title")?;
        Some(decode_entities(after[open..open + close].trim()))
    });
    Page {
        title: title.unwrap_or_default(),
        description: first(&["og:description", "twitter:description", "description"])
            .unwrap_or_default(),
        image: first(&["og:image", "og:image:url", "twitter:image"]),
    }
}

fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `name="value"` pairs of a tag (names lowercased); quotes may be single, double or none.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = tag.trim_start();
    while !rest.is_empty() {
        let name_end = rest
            .find(|c: char| c == '=' || c.is_whitespace() || c == '/')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (value, next) = match after.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let inner = &after[1..];
                    let end = inner.find(quote).unwrap_or(inner.len());
                    (&inner[..end], inner.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            rest = next.trim_start();
            value.to_owned()
        } else {
            // A lone `/` or a name without value.
            rest = rest.get(1..).unwrap_or("").trim_start();
            String::new()
        };
        if !name.is_empty() {
            found.push((name, value));
        }
    }
    found
}

/// The few entities titles use.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';').filter(|&end| end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_open_graph_tags_and_falls_back_to_the_title() {
        let html = r#"<html><head>
            <title>Fallback</title>
            <META property="og:title" content="Rust &amp; friends">
            <meta name=description content='Fast &#39;n&#x27; safe'>
            <meta property="og:image" content="/logo.png" />
            </head><body><meta property="og:title" content="not this"></body></html>"#;
        assert_eq!(
            parse(html),
            Page {
                title: "Rust & friends".into(),
                description: "Fast 'n' safe".into(),
                image: Some("/logo.png".into()),
            }
        );
        let plain = "<head><title> Just a title </title></head>";
        assert_eq!(parse(plain).title, "Just a title");
        assert_eq!(parse("<p>nothing</p>"), Page::default());
    }

    /// Goes online; run with `cargo test -- --ignored` to check against a real page.
    #[test]
    #[ignore]
    fn fetches_a_real_page() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let page = runtime
            .block_on(fetch_page("https://www.rust-lang.org/"))
            .expect("a preview");
        assert!(page.title.contains("Rust"), "{page:?}");
        let image = page.image.expect("a picture");
        let (bytes, content_type) = runtime.block_on(fetch_image(&image)).expect("downloads");
        assert!(content_type.starts_with("image/") && !bytes.is_empty());
    }

    #[test]
    fn odd_entities_stay_as_they_are() {
        assert_eq!(decode_entities("a & b &bogus; &#65;"), "a & b &bogus; A");
    }
}
