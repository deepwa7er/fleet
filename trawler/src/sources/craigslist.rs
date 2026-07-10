//! Adapter for Craigslist vehicle searches.
//!
//! The search page is server-rendered: each result is a
//! `<li class="cl-static-search-result" title="…">` carrying the full title,
//! detail URL, price, and the poster's name (usually the dealership). The
//! render is truncated at roughly 340 results with no working offset
//! parameter, so when a query comes back at the truncation threshold the
//! fetch recursively bisects the price range until every slice is complete.
//!
//! Titles alone can't be trusted to identify an engine, but fetching ~400
//! detail pages per run to decode every VIN would be obnoxious. Compromise:
//! only listings whose title mentions a B58-plausible designation ("40i",
//! "45e", "50e" — every B58 badge contains one of these) get their detail
//! page fetched for the VIN and odometer; the VIN decode then has the final
//! word as usual. A dealer post titled without the designation ("2018 BMW
//! 3 Series") is missed — that recall limit is inherent and documented.
//! Listings without a VIN on the detail page are skipped with a warning
//! rather than reported unverified. Verified against the live site on
//! 2026-07-03.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use regex::Regex;

use crate::config::Source;
use crate::listing::{RawListing, parse_quantity};

/// Pause between requests. Craigslist rate-limits aggressively; stay modest.
const REQUEST_DELAY: Duration = Duration::from_millis(500);
/// Result counts at or above this are treated as a truncated render.
const TRUNCATION_THRESHOLD: usize = 300;
/// Bisection depth cap: 8 halvings of a $0–$200k range is sub-$1000 slices,
/// far past what any real inventory needs.
const MAX_DEPTH: u32 = 8;

/// One parsed search-result card.
#[derive(Debug, Clone)]
struct Card {
    title: String,
    url: String,
    price: Option<u32>,
    /// The poster's display name — the dealership, for purveyor=dealer.
    poster: Option<String>,
}

/// Undoes the HTML escaping Craigslist applies to card titles and names.
fn unescape(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

/// Parses every `cl-static-search-result` card out of a search page.
fn parse_cards(html: &str) -> Vec<Card> {
    let title_re = Regex::new(r#"^[^>]*title="([^"]*)""#).expect("title pattern must compile");
    let href_re = Regex::new(r#"<a href="([^"]+)""#).expect("href pattern must compile");
    let price_re =
        Regex::new(r#"class="price">([^<]*)</div>"#).expect("price pattern must compile");
    let poster_re = Regex::new(r#"(?s)class="location">\s*\+?\s*([^<]*?)\s*</div>"#)
        .expect("poster pattern must compile");

    html.split(r#"<li class="cl-static-search-result""#)
        .skip(1)
        .filter_map(|chunk| {
            let title = unescape(title_re.captures(chunk)?.get(1)?.as_str());
            let url = href_re.captures(chunk)?.get(1)?.as_str().to_string();
            let price = price_re
                .captures(chunk)
                .and_then(|c| parse_quantity(c.get(1).map_or("", |m| m.as_str())));
            let poster = poster_re
                .captures(chunk)
                .map(|c| unescape(c.get(1).map_or("", |m| m.as_str())))
                .filter(|p| !p.is_empty());
            Some(Card {
                title,
                url,
                price,
                poster,
            })
        })
        .collect()
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    Ok(client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("{url} refused the request"))?
        .text()
        .await?)
}

/// Collects cards for the price range `[lo, hi]` (`hi = None` is unbounded),
/// bisecting whenever the render is truncated.
async fn collect_slice(
    client: &reqwest::Client,
    source: &Source,
    lo: u32,
    hi: Option<u32>,
    depth: u32,
    out: &mut HashMap<String, Card>,
) -> Result<()> {
    let mut url = format!("{}&min_price={lo}", source.url);
    if let Some(hi) = hi {
        url.push_str(&format!("&max_price={hi}"));
    }
    let cards = parse_cards(&get_text(client, &url).await?);
    tokio::time::sleep(REQUEST_DELAY).await;

    if cards.len() >= TRUNCATION_THRESHOLD && depth < MAX_DEPTH {
        // Truncated: split the range and retry. An unbounded top half splits
        // at double the floor (or $20k when starting from zero).
        let mid = match hi {
            Some(h) if h > lo + 1 => lo + (h - lo) / 2,
            Some(h) => {
                eprintln!(
                    "warning: {}: ${lo}–${h} still renders truncated; \
                     some listings in that range may be missing",
                    source.name
                );
                out.extend(cards.into_iter().map(|c| (c.url.clone(), c)));
                return Ok(());
            }
            None => (lo * 2).max(20_000),
        };
        Box::pin(collect_slice(client, source, lo, Some(mid), depth + 1, out)).await?;
        Box::pin(collect_slice(client, source, mid + 1, hi, depth + 1, out)).await?;
    } else {
        if cards.len() >= TRUNCATION_THRESHOLD {
            eprintln!(
                "warning: {}: price slice ${lo}+ hit the bisection depth limit; \
                 some listings may be missing",
                source.name
            );
        }
        out.extend(cards.into_iter().map(|c| (c.url.clone(), c)));
    }
    Ok(())
}

/// True when the title could plausibly be a B58 car. Every B58 badge
/// contains "40i" (340i…840i, M40i, xDrive40i), "45e", or "50e".
fn plausible_b58(title: &str) -> bool {
    let t = title.to_lowercase();
    ["40i", "45e", "50e"].iter().any(|d| t.contains(d))
}

/// Extracts the VIN from a detail page: the `auto_vin` attribute row first,
/// falling back to the embedded JSON `"vin"` key. Either way it must look
/// like a VIN (17 chars, no I/O/Q).
fn extract_vin(page: &str) -> Option<String> {
    let attr_re =
        Regex::new(r#"VIN:</span>\s*<span class="valu">\s*([A-HJ-NPR-Za-hj-npr-z0-9]{17})\s*<"#)
            .expect("vin attr pattern must compile");
    let json_re =
        Regex::new(r#""vin":"([A-HJ-NPR-Z0-9]{17})""#).expect("vin json pattern must compile");
    attr_re
        .captures(page)
        .or_else(|| json_re.captures(page))
        .map(|c| c[1].to_uppercase())
}

fn extract_odometer(page: &str) -> Option<u32> {
    let re = Regex::new(r#"odometer:</span>\s*<span class="valu">\s*([0-9][0-9,]*)"#)
        .expect("odometer pattern must compile");
    re.captures(page).and_then(|c| parse_quantity(&c[1]))
}

fn extract_photo(page: &str) -> Option<String> {
    let re = Regex::new(r#"<meta property="og:image" content="([^"]+)""#)
        .expect("photo pattern must compile");
    re.captures(page).map(|c| c[1].to_string())
}

pub async fn fetch(client: &reqwest::Client, source: &Source) -> Result<Vec<RawListing>> {
    let mut cards = HashMap::new();
    collect_slice(client, source, 0, None, 0, &mut cards).await?;

    let candidates: Vec<Card> = cards
        .into_values()
        .filter(|c| plausible_b58(&c.title))
        .collect();
    eprintln!(
        "{}: {} candidate listing(s), fetching detail pages for VINs",
        source.name,
        candidates.len()
    );

    let mut listings = Vec::new();
    for card in candidates {
        let page = match get_text(client, &card.url).await {
            Ok(page) => page,
            Err(err) => {
                eprintln!("warning: {}: {err:#}", source.name);
                continue;
            }
        };
        tokio::time::sleep(REQUEST_DELAY).await;
        let Some(vin) = extract_vin(&page) else {
            eprintln!(
                "warning: {}: no VIN on \"{}\" ({}); skipping — cannot verify engine",
                source.name, card.title, card.url
            );
            continue;
        };
        listings.push(RawListing {
            vin,
            title: card.title,
            price: card.price,
            mileage: extract_odometer(&page),
            url: card.url,
            photo: extract_photo(&page),
            dealer: card.poster.unwrap_or_else(|| source.name.clone()),
        });
    }
    Ok(listings)
}

#[cfg(test)]
mod tests {
    use super::{extract_odometer, extract_vin, parse_cards, plausible_b58};

    const CARD_HTML: &str = r#"
        <li class="cl-static-search-result" title="2018 BMW 3 Series 340i xDrive &amp; more">
            <a href="https://www.craigslist.org/view/d/beaverton-2018-bmw/abc123">
                <div class="title">2018 BMW 3 Series 340i xDrive &amp; more</div>
                <div class="details">
                    <div class="price">$32,888</div>
                    <div class="location">
                        + Damerow Ford
                    </div>
                </div>
            </a>
        </li>
        <li class="cl-static-search-result" title="2011 BMW 328i no price">
            <a href="https://www.craigslist.org/view/d/portland-2011-bmw/def456">
                <div class="title">2011 BMW 328i no price</div>
            </a>
        </li>"#;

    #[test]
    fn parses_search_cards() {
        let cards = parse_cards(CARD_HTML);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].title, "2018 BMW 3 Series 340i xDrive & more");
        assert_eq!(
            cards[0].url,
            "https://www.craigslist.org/view/d/beaverton-2018-bmw/abc123"
        );
        assert_eq!(cards[0].price, Some(32_888));
        assert_eq!(cards[0].poster.as_deref(), Some("Damerow Ford"));
        assert_eq!(cards[1].price, None);
    }

    #[test]
    fn plausibility_prefilter() {
        for yes in [
            "2018 BMW 340i",
            "BMW M340I XDRIVE",
            "X5 xDrive40i",
            "745e sedan",
            "X5 50e",
        ] {
            assert!(plausible_b58(yes), "{yes} should be a candidate");
        }
        for no in ["2018 BMW 330i", "BMW M550i", "2011 328i xDrive", "X5 M50i"] {
            assert!(!plausible_b58(no), "{no} must not be a candidate");
        }
    }

    #[test]
    fn extracts_detail_fields() {
        let page = r#"
            <div class="attr auto_vin">
                <span class="labl">VIN:</span>
                <span class="valu">WBA8B7G56JNU94941</span>
            </div>
            <div class="attr auto_miles">
                <span class="labl">odometer:</span>
                <span class="valu">63,524</span>
            </div>"#;
        assert_eq!(extract_vin(page).as_deref(), Some("WBA8B7G56JNU94941"));
        assert_eq!(extract_odometer(page), Some(63_524));
        assert_eq!(
            extract_vin(r#"junk "vin":"WBA8B7G56JNU94941" json"#).as_deref(),
            Some("WBA8B7G56JNU94941")
        );
        assert_eq!(extract_vin("no vin here"), None);
    }
}
