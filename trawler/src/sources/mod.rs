//! Source adapters — one per dealer-website platform.
//!
//! Adapters are the only scraping-aware code: each knows how to pull the
//! used-BMW inventory out of one platform and normalize it into
//! [`RawListing`]s. Everything downstream (VIN decode, fitment, report) is
//! source-agnostic.

pub mod craigslist;
pub mod dealercom;
pub mod dealeron;
pub mod spaceauto;

use anyhow::Result;
use tokio::sync::OnceCell;

use crate::browser::Browser;
use crate::config::{Adapter, Source};
use crate::listing::RawListing;

/// Everything an adapter may fetch with: a plain HTTP client, plus a
/// lazily-detected headless browser for the platforms whose WAFs reject
/// plain clients. The browser is only looked for when a configured source
/// actually needs it.
pub struct Net {
    pub http: reqwest::Client,
    browser: OnceCell<Browser>,
}

impl Net {
    pub fn new(http: reqwest::Client) -> Net {
        Net {
            http,
            browser: OnceCell::new(),
        }
    }

    pub async fn browser(&self) -> Result<&Browser> {
        self.browser
            .get_or_try_init(|| async { Browser::detect() })
            .await
    }
}

/// Fetches the used-BMW inventory of one configured source.
pub async fn fetch(net: &Net, source: &Source) -> Result<Vec<RawListing>> {
    match source.adapter {
        Adapter::Craigslist => craigslist::fetch(&net.http, source).await,
        Adapter::DealerCom => dealercom::fetch(net, source).await,
        Adapter::DealerOn => dealeron::fetch(&net.http, source).await,
        Adapter::SpaceAuto => spaceauto::fetch(&net.http, source).await,
    }
}

/// Fills in fields the source's list feed lacks (currently: mileage on Space
/// Auto listings, which only the detail page carries). Called only for
/// confirmed B58 cars to keep the extra requests proportional to the catch.
pub async fn backfill(net: &Net, source: &Source, listing: &mut RawListing) {
    if source.adapter == Adapter::SpaceAuto {
        spaceauto::backfill_mileage(&net.http, listing).await;
    }
}
