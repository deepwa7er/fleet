//! Best-effort link metadata. Given a product URL, fetch the page and read its
//! Open Graph / Twitter / schema.org tags to pre-fill an item's title, store,
//! image, and price. Every field is optional and any failure degrades to an
//! empty result — the user can always type the fields by hand, so this never
//! blocks adding a link.
//!
//! Trust model: this is a single-user tool behind the tailnet (breakwater), and
//! the user pastes their own shopping links, so the fetch is deliberately simple.
//! It only follows `http`/`https`, caps time and body size, and refuses obvious
//! loopback/internal hosts — but it is NOT a hardened SSRF gateway (it does not
//! resolve DNS to vet the resolved IP). Do not expose it to untrusted callers.

use std::io::Read;
use std::time::Duration;

use scraper::{Html, Selector};
use serde::Serialize;

/// What we managed to extract. All optional; serialized to the add-item form.
#[derive(Debug, Default, Clone, Serialize)]
pub struct LinkMeta {
    pub title: Option<String>,
    pub store: Option<String>,
    pub image_url: Option<String>,
    pub price_cents: Option<i64>,
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(6);
/// Read at most this many bytes of HTML — the head (where meta tags live) is
/// early, so this is plenty and bounds memory against a hostile response.
const MAX_BYTES: usize = 512 * 1024;

/// Fetch and parse metadata for `url`. Blocking — call from `spawn_blocking`.
/// Returns an empty `LinkMeta` (never an error) if anything goes wrong, so a
/// flaky retailer never breaks the add flow.
pub fn fetch(url: &str) -> LinkMeta {
    match try_fetch(url) {
        Ok(meta) => meta,
        Err(reason) => {
            tracing::info!("link-meta fetch for {url} yielded nothing: {reason}");
            LinkMeta::default()
        }
    }
}

fn try_fetch(url: &str) -> Result<LinkMeta, String> {
    let host = vet_url(url)?;

    let resp = ureq::get(url)
        .timeout(FETCH_TIMEOUT)
        .set(
            "User-Agent",
            "Mozilla/5.0 (compatible; clothes-wardrobe/0.1; +tailnet)",
        )
        .set("Accept", "text/html,application/xhtml+xml")
        .call()
        .map_err(|e| e.to_string())?;

    let content_type = resp.content_type().to_string();
    if !content_type.contains("html") {
        return Err(format!("not HTML ({content_type})"));
    }

    let mut body = String::new();
    resp.into_reader()
        .take(MAX_BYTES as u64)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;

    Ok(extract(&body, &host))
}

/// Accept only `http`/`https` URLs with a real, non-loopback host. Returns the
/// host for use as the store fallback.
fn vet_url(url: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("only http/https URLs are supported")?;
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_port.split('@').next_back().unwrap_or(host_port);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        return Err("no host".into());
    }
    let lower = host.to_ascii_lowercase();
    let internal = lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "127.0.0.1"
        || lower == "::1"
        || lower.starts_with("10.")
        || lower.starts_with("192.168.")
        || lower.starts_with("169.254.")
        || (lower.starts_with("172.")
            && lower
                .split('.')
                .nth(1)
                .and_then(|o| o.parse::<u8>().ok())
                .is_some_and(|o| (16..=31).contains(&o)));
    if internal {
        return Err(format!("refusing internal host {host}"));
    }
    Ok(lower)
}

fn extract(html: &str, host: &str) -> LinkMeta {
    let doc = Html::parse_document(html);
    LinkMeta {
        title: first_meta(&doc, &["og:title", "twitter:title"])
            .or_else(|| doc_title(&doc))
            .map(|s| trim_to(&s, 200)),
        store: first_meta(&doc, &["og:site_name"])
            .map(|s| trim_to(&s, 80))
            .or_else(|| Some(store_from_host(host))),
        image_url: first_meta(&doc, &["og:image", "og:image:url", "twitter:image"]),
        price_cents: first_meta(
            &doc,
            &["product:price:amount", "og:price:amount", "twitter:data1"],
        )
        .or_else(|| itemprop_price(&doc))
        .and_then(|s| parse_price_cents(&s)),
    }
}

/// The first present, non-empty `<meta property|name=...>` content among `keys`.
fn first_meta(doc: &Html, keys: &[&str]) -> Option<String> {
    for key in keys {
        for attr in ["property", "name"] {
            let sel = Selector::parse(&format!(r#"meta[{attr}="{key}"]"#)).ok()?;
            if let Some(el) = doc.select(&sel).next() {
                if let Some(content) = el.value().attr("content") {
                    let content = content.trim();
                    if !content.is_empty() {
                        return Some(content.to_string());
                    }
                }
            }
        }
    }
    None
}

fn itemprop_price(doc: &Html) -> Option<String> {
    let sel = Selector::parse(r#"[itemprop="price"]"#).ok()?;
    let el = doc.select(&sel).next()?;
    el.value()
        .attr("content")
        .map(str::to_string)
        .or_else(|| Some(el.text().collect::<String>()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn doc_title(doc: &Html) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    let text = doc.select(&sel).next()?.text().collect::<String>();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Turn "www.uniqlo.com" into "Uniqlo": drop a leading www, take the second-level
/// label, and title-case it.
fn store_from_host(host: &str) -> String {
    let bare = host.strip_prefix("www.").unwrap_or(host);
    let label = bare.split('.').next().unwrap_or(bare);
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => bare.to_string(),
    }
}

/// Parse a price string like "$129.00", "129", or "1,299.99" into integer cents.
/// Returns None if no digits are present.
fn parse_price_cents(s: &str) -> Option<i64> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() {
        return None;
    }
    let value: f64 = cleaned.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some((value * 100.0).round() as i64)
}

fn trim_to(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>().trim_end().to_string() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_open_graph_tags() {
        let html = r#"<html><head>
            <meta property="og:title" content="Slim Fit Oxford Shirt">
            <meta property="og:site_name" content="J.Crew">
            <meta property="og:image" content="https://img.example/shirt.jpg">
            <meta property="product:price:amount" content="89.50">
        </head><body></body></html>"#;
        let m = extract(html, "jcrew.com");
        assert_eq!(m.title.as_deref(), Some("Slim Fit Oxford Shirt"));
        assert_eq!(m.store.as_deref(), Some("J.Crew"));
        assert_eq!(m.image_url.as_deref(), Some("https://img.example/shirt.jpg"));
        assert_eq!(m.price_cents, Some(8950));
    }

    #[test]
    fn falls_back_to_title_tag_and_host_store() {
        let html = "<html><head><title>  Wool Trousers  </title></head></html>";
        let m = extract(html, "www.uniqlo.com");
        assert_eq!(m.title.as_deref(), Some("Wool Trousers"));
        assert_eq!(m.store.as_deref(), Some("Uniqlo"), "derived from host");
        assert_eq!(m.image_url, None);
        assert_eq!(m.price_cents, None);
    }

    #[test]
    fn price_parsing_handles_symbols_and_separators() {
        assert_eq!(parse_price_cents("$129.00"), Some(12900));
        assert_eq!(parse_price_cents("1,299.99"), Some(129999));
        assert_eq!(parse_price_cents("129"), Some(12900));
        assert_eq!(parse_price_cents("Free"), None);
    }

    #[test]
    fn vet_url_rejects_non_http_and_internal_hosts() {
        assert!(vet_url("ftp://example.com").is_err());
        assert!(vet_url("http://localhost:8080/x").is_err());
        assert!(vet_url("http://127.0.0.1/x").is_err());
        assert!(vet_url("http://192.168.1.5/x").is_err());
        assert!(vet_url("http://172.16.0.9/x").is_err());
        assert_eq!(vet_url("https://www.example.com/p/1").unwrap(), "www.example.com");
        // 172.32 is outside the private 172.16-31 block, so it's allowed.
        assert!(vet_url("http://172.32.0.1/x").is_ok());
    }
}
