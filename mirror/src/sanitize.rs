//! Turning Fizzy's rich text into HTML that is safe to put on a public page.
//!
//! `card.description_html` is `ActionText::RichText#to_s` — the raw stored
//! body, not the sanitized render path Rails uses in a view. Whatever is in
//! there arrives here verbatim, so this module is the only thing standing
//! between a card's contents and a public page. It runs at **ingest**, not at
//! render: what the database holds is already safe, so no rendering path can
//! reintroduce the problem by forgetting a call.
//!
//! Two passes, because rewriting an image URL requires first knowing which
//! images to download:
//!
//! 1. [`image_urls`] parses the fragment and reports every `<img src>`.
//! 2. The caller downloads the ones it is willing to serve ([`crate::assets`]).
//! 3. [`clean`] parses it again, applying the allowlist and swapping each
//!    `src` for the local asset path.
//!
//! Both passes go through ammonia (html5ever) rather than pattern-matching the
//! markup: an HTML parser is the only thing that agrees with a browser about
//! what an attribute is.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ammonia::Builder;

/// Elements a card body may keep. Everything else is unwrapped (its text
/// survives, the tag does not) — including ActionText's own
/// `<action-text-attachment>` wrappers, whose `<figure>` contents come
/// through.
const TAGS: &[&str] = &[
    "p",
    "br",
    "hr",
    "strong",
    "b",
    "em",
    "i",
    "u",
    "s",
    "del",
    "ins",
    "code",
    "pre",
    "blockquote",
    "ul",
    "ol",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "a",
    "img",
    "figure",
    "figcaption",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
];

/// Build the shared allowlist. `class`, `style`, `id` and every `data-*` are
/// absent on purpose: the mirror's stylesheet owns presentation, and letting
/// card text carry ids would let it collide with the page's own anchors.
fn builder<'a>() -> Builder<'a> {
    let mut b = Builder::default();
    b.tags(HashSet::from_iter(TAGS.iter().copied()))
        .generic_attributes(HashSet::new())
        .tag_attributes(HashMap::from([
            ("a", HashSet::from_iter(["href", "title"])),
            ("img", HashSet::from_iter(["src", "alt", "width", "height"])),
        ]))
        .url_schemes(HashSet::from_iter(["http", "https", "mailto"]))
        // Outbound links are third-party by definition; nofollow keeps the
        // mirror from lending them ranking, noopener/noreferrer keeps them
        // from learning where the click came from.
        .link_rel(Some("nofollow noopener noreferrer"));
    b
}

/// Every `<img src>` in `html`, in document order, deduplicated.
pub fn image_urls(html: &str) -> Vec<String> {
    let found: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = found.clone();
    // ammonia has no read-only visitor, so the collector is an attribute
    // filter whose output is thrown away. Cheap, and it means both passes see
    // the document exactly the same way.
    let mut b = builder();
    b.attribute_filter(move |element, attribute, value| {
        if element == "img" && attribute == "src" {
            if let Ok(mut sink) = sink.lock() {
                sink.push(value.to_string());
            }
        }
        Some(Cow::Borrowed(value))
    });
    let _ = b.clean(html).to_string();

    let mut seen = HashSet::new();
    let urls = found.lock().map(|f| f.clone()).unwrap_or_default();
    urls.into_iter()
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

/// Sanitize `html`, replacing each `<img src>` with the local path `assets`
/// gives for it.
///
/// An image with no entry in `assets` — one the fetcher refused (a foreign
/// origin) or could not retrieve — loses its `src` entirely. The alternative,
/// leaving the original URL in place, would either hot-link a stranger's
/// server from every visitor's browser or point at an internal hostname that
/// resolves to a tailnet address nobody outside can reach.
///
/// Links back into Fizzy are unwrapped for the same reason: `internal_origin`
/// names the host whose URLs must never appear on the public page.
pub fn clean(html: &str, assets: &HashMap<String, String>, internal_origin: &str) -> String {
    let assets = assets.clone();
    let internal_origin = internal_origin.to_string();
    let mut b = builder();
    b.attribute_filter(
        move |element, attribute, value| match (element, attribute) {
            ("img", "src") => assets.get(value).map(|local| Cow::Owned(local.clone())),
            ("a", "href") if value.starts_with(&internal_origin) => None,
            _ => Some(Cow::Borrowed(value)),
        },
    );
    b.clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERNAL: &str = "https://fizzy.internal";

    fn no_assets() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn strips_scripts_and_event_handlers() {
        let dirty = r#"<p onclick="steal()">hi<script>alert(1)</script></p>"#;
        let clean = clean(dirty, &no_assets(), INTERNAL);
        assert!(!clean.contains("script"), "{clean}");
        assert!(!clean.contains("onclick"), "{clean}");
        assert!(clean.contains("hi"), "{clean}");
    }

    #[test]
    fn keeps_ordinary_formatting() {
        let clean = clean(
            "<div class=\"action-text-content\"><p>a <strong>b</strong></p><ul><li>c</li></ul></div>",
            &no_assets(),
            INTERNAL,
        );
        assert!(clean.contains("<strong>b</strong>"), "{clean}");
        assert!(clean.contains("<li>c</li>"), "{clean}");
        assert!(!clean.contains("class="), "{clean}");
    }

    #[test]
    fn finds_image_urls_once_each() {
        let html = r#"<p><img src="https://a/1.png"><img src="https://a/1.png"><img src="https://b/2.png"></p>"#;
        assert_eq!(
            image_urls(html),
            vec!["https://a/1.png".to_string(), "https://b/2.png".to_string()]
        );
    }

    #[test]
    fn rewrites_cached_images_and_drops_the_rest() {
        let assets = HashMap::from([("https://a/1.png".to_string(), "/a/abc.png".to_string())]);
        let clean = clean(
            r#"<p><img src="https://a/1.png" alt="kept"><img src="https://evil/2.png" alt="dropped"></p>"#,
            &assets,
            INTERNAL,
        );
        assert!(clean.contains(r#"src="/a/abc.png""#), "{clean}");
        assert!(!clean.contains("evil"), "{clean}");
        // The element survives without a source; its alt text still reads.
        assert!(clean.contains("dropped"), "{clean}");
    }

    #[test]
    fn unwraps_links_back_into_fizzy() {
        let clean = clean(
            r#"<p><a href="https://fizzy.internal/1/cards/3">card 3</a> and <a href="https://example.com/x">out</a></p>"#,
            &no_assets(),
            INTERNAL,
        );
        assert!(!clean.contains("fizzy.internal"), "{clean}");
        assert!(clean.contains("card 3"), "{clean}");
        assert!(clean.contains(r#"href="https://example.com/x""#), "{clean}");
        assert!(clean.contains("nofollow"), "{clean}");
    }

    #[test]
    fn refuses_javascript_urls() {
        let clean = clean(
            r#"<p><a href="javascript:alert(1)">x</a></p>"#,
            &no_assets(),
            INTERNAL,
        );
        assert!(!clean.contains("javascript"), "{clean}");
    }
}
