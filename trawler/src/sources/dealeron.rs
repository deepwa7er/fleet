//! Adapter for DealerOn "Cosmos" dealer sites (e.g. bmwofportland.com).
//!
//! The used-vehicle search page embeds a `dealerId` and `pageId`; those key
//! a JSON API that serves the same result cards the page renders:
//!
//!   GET {base}/api/vhcliaa/vehicle-pages/cosmos/srp/vehicles/{dealerId}/{pageId}
//!       ?pt={page}&pn=96&Make=BMW
//!
//! The response is paginated (`Paging.PaginationDataModel`), 96 cards per
//! page. Card fields verified against the live site on 2026-07-03.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::config::Source;
use crate::listing::{RawListing, parse_quantity};

/// Pause between page requests — this is someone's storefront, not an API
/// we have an SLA with.
const PAGE_DELAY: Duration = Duration::from_millis(300);

#[derive(Debug, Deserialize)]
struct SrpResponse {
    #[serde(rename = "Paging")]
    paging: Paging,
    #[serde(rename = "DisplayCards", default)]
    display_cards: Vec<DisplayCard>,
}

#[derive(Debug, Deserialize)]
struct Paging {
    #[serde(rename = "PaginationDataModel")]
    data: Pagination,
}

#[derive(Debug, Deserialize)]
struct Pagination {
    #[serde(rename = "PageNumber")]
    page_number: u32,
    #[serde(rename = "TotalPages")]
    total_pages: u32,
    #[serde(rename = "TotalCount")]
    total_count: u32,
}

#[derive(Debug, Deserialize)]
struct DisplayCard {
    #[serde(rename = "IsAdCard", default)]
    is_ad_card: bool,
    #[serde(rename = "VehicleCard")]
    vehicle_card: Option<VehicleCard>,
}

#[derive(Debug, Deserialize)]
struct VehicleCard {
    #[serde(rename = "VehicleName", default)]
    vehicle_name: String,
    #[serde(rename = "TaggingPrice", default)]
    tagging_price: String,
    #[serde(rename = "Mileage", default)]
    mileage: String,
    #[serde(rename = "VehicleDetailUrl", default)]
    detail_url: String,
    #[serde(rename = "VehicleCompareModel")]
    compare: Option<CompareModel>,
    #[serde(rename = "VehicleImageModel")]
    image: Option<ImageModel>,
}

#[derive(Debug, Deserialize)]
struct CompareModel {
    #[serde(rename = "Vin", default)]
    vin: String,
}

#[derive(Debug, Deserialize)]
struct ImageModel {
    #[serde(rename = "VehicleImageCarouselModel")]
    carousel: Option<CarouselModel>,
}

#[derive(Debug, Deserialize)]
struct CarouselModel {
    #[serde(rename = "PhotoList", default)]
    photo_list: Vec<String>,
}

/// Extracts the first `"key":<digits>` (optionally quoted) match.
fn extract_id(page: &str, key: &str) -> Option<u64> {
    let re = Regex::new(&format!(r#""{key}"\s*:\s*"?(\d+)"#)).expect("id pattern must compile");
    re.captures(page)?.get(1)?.as_str().parse().ok()
}

/// One retry after a short pause smooths over transient network flakes
/// (observed: sporadic connect timeouts on otherwise-healthy dealer sites).
async fn get_with_retry(client: &reqwest::Client, url: &str) -> reqwest::Result<reqwest::Response> {
    if let Ok(response) = client.get(url).send().await {
        return Ok(response);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    client.get(url).send().await
}

pub async fn fetch(client: &reqwest::Client, source: &Source) -> Result<Vec<RawListing>> {
    let srp_url = format!("{}/searchused.aspx?Make=BMW", source.url);
    let page = get_with_retry(client, &srp_url)
        .await
        .with_context(|| format!("fetching {srp_url}"))?
        .error_for_status()
        .with_context(|| format!("{srp_url} refused the request"))?
        .text()
        .await?;

    let dealer_id = extract_id(&page, "dealerId");
    let page_id = extract_id(&page, "pageId");
    let (Some(dealer_id), Some(page_id)) = (dealer_id, page_id) else {
        bail!(
            "{}: could not find dealerId/pageId on {srp_url} — \
             the site may have moved off DealerOn",
            source.name
        );
    };

    let mut listings = Vec::new();
    let mut page_number = 1;
    loop {
        let api_url = format!(
            "{}/api/vhcliaa/vehicle-pages/cosmos/srp/vehicles/{dealer_id}/{page_id}?pt={page_number}&pn=96&Make=BMW",
            source.url
        );
        let response: SrpResponse = get_with_retry(client, &api_url)
            .await
            .with_context(|| format!("fetching {api_url}"))?
            .error_for_status()
            .with_context(|| format!("{api_url} refused the request"))?
            .json()
            .await
            .with_context(|| format!("parsing inventory JSON from {api_url}"))?;

        for card in response.display_cards {
            if card.is_ad_card {
                continue;
            }
            let Some(vehicle) = card.vehicle_card else {
                continue;
            };
            let Some(vin) = vehicle.compare.map(|c| c.vin).filter(|v| !v.is_empty()) else {
                eprintln!(
                    "warning: {}: skipping card without VIN ({})",
                    source.name, vehicle.vehicle_name
                );
                continue;
            };
            let photo = vehicle
                .image
                .and_then(|i| i.carousel)
                .and_then(|c| c.photo_list.into_iter().next())
                .map(|path| {
                    if path.starts_with("http") {
                        path
                    } else {
                        format!("{}{path}", source.url)
                    }
                });
            listings.push(RawListing {
                vin,
                title: vehicle.vehicle_name.trim().to_string(),
                price: parse_quantity(&vehicle.tagging_price),
                mileage: parse_quantity(&vehicle.mileage),
                url: vehicle.detail_url,
                photo,
                dealer: source.name.clone(),
            });
        }

        let pagination = response.paging.data;
        if pagination.page_number >= pagination.total_pages {
            if listings.len() as u32 != pagination.total_count {
                eprintln!(
                    "warning: {}: site reported {} vehicles, collected {}",
                    source.name,
                    pagination.total_count,
                    listings.len()
                );
            }
            break;
        }
        page_number += 1;
        tokio::time::sleep(PAGE_DELAY).await;
    }

    Ok(listings)
}
