//! Adapter for Dealer.com ("DDC") storefronts, fetched through the headless
//! browser — the notable local one, BMW of Salem (Lithia), fronts the site
//! with a WAF that rejects plain HTTP clients but passes a real browser.
//!
//! The rendered search-results page embeds the full inventory state as
//! JavaScript:
//!
//!   DDC.WS.state['ws-inv-data'][…] = { "WIS": { "pageInfo": {…},
//!       "inventory": [ { vin, make, model, trim, year, link, pricing,
//!                        highlightedAttributes, images, … } ] } }
//!
//! Pagination is `?start=N` in steps of `pageInfo.pageSize`. Verified
//! against the live site on 2026-07-03.

use anyhow::{Result, bail};
use regex::Regex;
use serde::{Deserialize, Deserializer};

use crate::config::Source;
use crate::listing::{RawListing, parse_quantity};
use crate::sources::Net;

/// Like `#[serde(default)]`, but also maps an explicit JSON `null` to the
/// default — DDC emits nulls freely (e.g. `"isFinalPrice": null`).
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// DDC emits `"year"` as a bare number; accept a string too and normalize.
fn year_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        match Option::<serde_json::Value>::deserialize(deserializer)? {
            Some(serde_json::Value::Number(n)) => n.to_string(),
            Some(serde_json::Value::String(s)) => s,
            _ => String::new(),
        },
    )
}

/// Hard cap on pages per source; a used-BMW search needing more than this
/// means the filter is broken, not the inventory huge.
const PAGE_CAP: usize = 20;

#[derive(Debug, Deserialize)]
struct State {
    #[serde(rename = "WIS")]
    wis: Wis,
}

#[derive(Debug, Deserialize)]
struct Wis {
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
    #[serde(default)]
    inventory: Vec<Vehicle>,
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(rename = "totalCount")]
    total_count: usize,
    #[serde(rename = "pageSize")]
    page_size: usize,
    #[serde(rename = "pageStart")]
    page_start: usize,
}

#[derive(Debug, Deserialize)]
struct Vehicle {
    #[serde(rename = "isPlaceholder", default, deserialize_with = "null_default")]
    is_placeholder: bool,
    #[serde(default, deserialize_with = "null_default")]
    vin: String,
    #[serde(default, deserialize_with = "null_default")]
    make: String,
    #[serde(default, deserialize_with = "null_default")]
    model: String,
    #[serde(default, deserialize_with = "null_default")]
    trim: String,
    #[serde(default, deserialize_with = "year_string")]
    year: String,
    #[serde(default, deserialize_with = "null_default")]
    condition: String,
    #[serde(default, deserialize_with = "null_default")]
    link: String,
    #[serde(default)]
    pricing: Option<Pricing>,
    #[serde(
        rename = "highlightedAttributes",
        default,
        deserialize_with = "null_default"
    )]
    highlighted_attributes: Vec<Attribute>,
    #[serde(default, deserialize_with = "null_default")]
    images: Vec<Image>,
}

#[derive(Debug, Deserialize)]
struct Pricing {
    #[serde(default, deserialize_with = "null_default")]
    dprice: Vec<DPrice>,
    #[serde(rename = "retailPrice", default, deserialize_with = "null_default")]
    retail_price: String,
}

#[derive(Debug, Deserialize)]
struct DPrice {
    #[serde(rename = "typeClass", default, deserialize_with = "null_default")]
    type_class: String,
    #[serde(default, deserialize_with = "null_default")]
    value: String,
    #[serde(rename = "isFinalPrice", default, deserialize_with = "null_default")]
    is_final_price: bool,
}

#[derive(Debug, Deserialize)]
struct Attribute {
    #[serde(default, deserialize_with = "null_default")]
    name: String,
    #[serde(default, deserialize_with = "null_default")]
    value: String,
}

#[derive(Debug, Deserialize)]
struct Image {
    #[serde(default, deserialize_with = "null_default")]
    uri: String,
}

/// Returns the JSON object literal starting at `open` (which must index a
/// `{`), honoring strings and escapes.
fn json_object_at(text: &str, open: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let (mut depth, mut in_string, mut escaped) = (0u32, false, false);
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[open..=i]);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Finds and parses the `ws-inv-data` state blob in a rendered SRP.
fn extract_state(dom: &str) -> Option<State> {
    let marker = Regex::new(r"DDC\.WS\.state\['ws-inv-data'\]\[[^\]]+\]\s*=\s*\{")
        .expect("marker pattern must compile");
    marker.find_iter(dom).find_map(|m| {
        let blob = json_object_at(dom, m.end() - 1)?;
        serde_json::from_str(blob).ok()
    })
}

fn price_of(pricing: Option<&Pricing>) -> Option<u32> {
    let pricing = pricing?;
    pricing
        .dprice
        .iter()
        .find(|d| d.is_final_price)
        .or_else(|| {
            pricing
                .dprice
                .iter()
                .find(|d| d.type_class == "internetPrice")
        })
        .map(|d| d.value.as_str())
        .or(Some(pricing.retail_price.as_str()))
        .and_then(parse_quantity)
}

fn mileage_of(vehicle: &Vehicle) -> Option<u32> {
    vehicle
        .highlighted_attributes
        .iter()
        .find(|a| a.name == "odometer")
        .and_then(|a| parse_quantity(&a.value))
}

pub async fn fetch(net: &Net, source: &Source) -> Result<Vec<RawListing>> {
    let browser = net.browser().await?;
    let mut listings = Vec::new();
    let mut start = 0;
    for page in 0..PAGE_CAP {
        let url = format!(
            "{}/used-inventory/index.htm?make=BMW&start={start}",
            source.url
        );
        // A dump that fires before the inventory request settles leaves
        // placeholder entries in the state; treat that as a failed render
        // and try the page once more before giving up.
        let mut state = None;
        for _attempt in 0..2 {
            let dom = browser.dump_dom(&url).await?;
            match extract_state(&dom) {
                Some(s) if !s.wis.inventory.iter().any(|v| v.is_placeholder) => {
                    state = Some(s);
                    break;
                }
                _ => {}
            }
        }
        let Some(state) = state else {
            bail!(
                "{}: no complete Dealer.com inventory state on {url} — slow \
                 render, bot challenge, or the site moved off Dealer.com",
                source.name
            );
        };

        for vehicle in &state.wis.inventory {
            if vehicle.is_placeholder
                || vehicle.vin.is_empty()
                || !vehicle.make.eq_ignore_ascii_case("bmw")
                || vehicle.condition.eq_ignore_ascii_case("new")
            {
                continue;
            }
            let title = format!(
                "{} {} {} {}",
                vehicle.year, vehicle.make, vehicle.model, vehicle.trim
            )
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
            listings.push(RawListing {
                vin: vehicle.vin.to_uppercase(),
                title,
                price: price_of(vehicle.pricing.as_ref()),
                mileage: mileage_of(vehicle),
                url: if vehicle.link.starts_with("http") {
                    vehicle.link.clone()
                } else {
                    format!("{}{}", source.url, vehicle.link)
                },
                photo: vehicle
                    .images
                    .first()
                    .map(|i| i.uri.clone())
                    .filter(|u| !u.is_empty()),
                dealer: source.name.clone(),
            });
        }

        let info = &state.wis.page_info;
        start = info.page_start + info.page_size;
        if start >= info.total_count {
            return Ok(listings);
        }
        if page + 1 == PAGE_CAP {
            eprintln!(
                "warning: {}: stopping at {PAGE_CAP} pages with {} of {} vehicles collected",
                source.name,
                listings.len(),
                info.total_count
            );
        }
    }
    Ok(listings)
}

#[cfg(test)]
mod tests {
    use super::{extract_state, json_object_at, mileage_of, price_of};

    const DOM: &str = r#"<script>DDC.WS.state['ws-inv-data']['inventory-data-bus1'] =
        {"WIS":{"pageInfo":{"totalCount":45,"pageSize":23,"pageStart":0},
        "inventory":[
          {"uuid":"0","isPlaceholder":true},
          {"vin":"WBA2J1C00L7F61401","make":"BMW","model":"230i","trim":"230i",
           "year":2020,"condition":"Used","link":"/used/BMW/2020-BMW-230i.htm",
           "pricing":{"retailPrice":"$17,740","dprice":[
             {"label":"Was","typeClass":"retailValue","value":"$17,740","isFinalPrice":null},
             {"label":"Price","typeClass":"internetPrice","value":"$15,750","isFinalPrice":true}]},
           "highlightedAttributes":[
             {"name":"type","value":"Used"},
             {"name":"odometer","value":"99,247 miles"}],
           "images":[{"uri":"https://pictures.dealer.com/x.jpg"}]}
        ]}}; </script>"#;

    #[test]
    fn extracts_inventory_state() {
        let state = extract_state(DOM).expect("state must parse");
        assert_eq!(state.wis.page_info.total_count, 45);
        let v = &state.wis.inventory[1];
        assert_eq!(v.vin, "WBA2J1C00L7F61401");
        assert_eq!(v.year, "2020", "numeric year must normalize to a string");
        assert_eq!(price_of(v.pricing.as_ref()), Some(15_750));
        assert_eq!(mileage_of(v), Some(99_247));
        assert!(state.wis.inventory[0].is_placeholder);
    }

    #[test]
    fn brace_matching_respects_strings() {
        let s = r#"{"a":"quoted } brace","b":{"c":1}} trailing"#;
        assert_eq!(
            json_object_at(s, 0),
            Some(r#"{"a":"quoted } brace","b":{"c":1}}"#)
        );
        assert_eq!(json_object_at(r#"{"unterminated": true"#, 0), None);
    }
}
