//! Adapter for Space Auto WordPress storefronts (e.g. freemanmotor.com).
//!
//! The homepage embeds the URL of a date-stamped JSON inventory dump on
//! `inventory.apollo.space.auto`; the dump carries VIN, title, price, and
//! detail URL, but no mileage — that only appears on the detail page, so
//! [`backfill_mileage`] scrapes it there for the few cars that survive the
//! B58 filter. Verified against the live site on 2026-07-03.

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::config::Source;
use crate::listing::{RawListing, parse_quantity};

#[derive(Debug, Deserialize)]
struct Dump {
    #[serde(default)]
    vehicles: Vec<Vehicle>,
}

#[derive(Debug, Deserialize)]
struct Vehicle {
    #[serde(default)]
    condition: String,
    /// "2021 BMW M340i" — year, make, model as the dealer titles it.
    #[serde(default)]
    name: String,
    /// Absent or 0 means "call for price".
    #[serde(default)]
    price: Option<u32>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    vin: String,
}

pub async fn fetch(client: &reqwest::Client, source: &Source) -> Result<Vec<RawListing>> {
    let home_url = format!("{}/", source.url);
    let page = client
        .get(&home_url)
        .send()
        .await
        .with_context(|| format!("fetching {home_url}"))?
        .error_for_status()
        .with_context(|| format!("{home_url} refused the request"))?
        .text()
        .await?;

    let dump_re =
        Regex::new(r#"https://inventory\.apollo\.space\.auto/[^"']*?vehicle-data-[^"']*?\.json"#)
            .expect("dump pattern must compile");
    let Some(dump_url) = dump_re.find(&page) else {
        bail!(
            "{}: no Space Auto inventory dump URL on {home_url} — \
             the site may have moved off Space Auto",
            source.name
        );
    };

    let dump: Dump = client
        .get(dump_url.as_str())
        .send()
        .await
        .with_context(|| format!("fetching {}", dump_url.as_str()))?
        .error_for_status()
        .with_context(|| format!("{} refused the request", dump_url.as_str()))?
        .json()
        .await
        .with_context(|| format!("parsing inventory JSON from {}", dump_url.as_str()))?;

    Ok(dump
        .vehicles
        .into_iter()
        .filter(|v| {
            !v.vin.is_empty()
                && v.condition.eq_ignore_ascii_case("used")
                && v.name.to_lowercase().contains("bmw")
        })
        .map(|v| RawListing {
            vin: v.vin,
            title: v.name.trim().to_string(),
            price: v.price.filter(|&p| p > 0),
            mileage: None,
            url: v.url,
            photo: None,
            dealer: source.name.clone(),
        })
        .collect())
}

/// Scrapes the detail page for the odometer reading ("85,413 miles"). Leaves
/// mileage as `None` when the page or pattern is unavailable — the report
/// renders that honestly as unknown.
pub async fn backfill_mileage(client: &reqwest::Client, listing: &mut RawListing) {
    let page = match client.get(&listing.url).send().await {
        Ok(response) => match response.error_for_status() {
            Ok(response) => response.text().await.unwrap_or_default(),
            Err(err) => {
                eprintln!("warning: mileage lookup failed for {}: {err}", listing.url);
                return;
            }
        },
        Err(err) => {
            eprintln!("warning: mileage lookup failed for {}: {err}", listing.url);
            return;
        }
    };
    let miles_re = Regex::new(r"([0-9][0-9,]*)\s*miles").expect("mileage pattern must compile");
    listing.mileage = miles_re
        .captures(&page)
        .and_then(|c| c.get(1))
        .and_then(|m| parse_quantity(m.as_str()));
}
