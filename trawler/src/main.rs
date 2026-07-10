mod browser;
mod config;
mod fitment;
mod listing;
mod report;
mod sources;
mod vin;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::config::{Config, Source};
use crate::listing::{B58Car, RawListing};
use crate::sources::Net;

/// Some dealer platforms 403 unknown clients, so identify as a current
/// browser — the same one the endpoints were verified with.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Trawler — finds used BMWs with the B58 engine on Portland-area
/// dealership websites.
///
/// Scrapes each configured dealer site, decodes every VIN through the NHTSA
/// vPIC API to establish what each car actually is, filters to B58-engined
/// models, and writes a browsable HTML report.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Path to a TOML config listing [[source]] entries. Defaults to the
    /// built-in Portland-area sources.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Where to write the HTML report.
    #[arg(short, long, default_value = "b58-report.html")]
    out: PathBuf,

    /// Print the results as JSON on stdout instead of a table.
    #[arg(long)]
    json: bool,

    /// Include the X-series SUVs (X3–X7). Hidden by default.
    #[arg(long)]
    suvs: bool,

    /// Newest model year to show.
    #[arg(long, default_value_t = 2019)]
    max_year: u16,

    /// Path of the VIN-decode cache file.
    #[arg(long)]
    vin_cache: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = match &cli.config {
        Some(path) => Config::load(path)?,
        None => Config::baseline(),
    };
    let cache_path = match cli.vin_cache {
        Some(path) => path,
        None => vin::default_cache_path()?,
    };

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .context("building HTTP client")?;
    let net = Arc::new(Net::new(client));

    let raws = fetch_all_sources(&net, &config.sources).await?;
    let mut cars = classify_all(&net.http, &raws, &cache_path, cli.suvs, cli.max_year).await?;

    for car in &mut cars {
        if car.mileage.is_none()
            && let Some((source, raw)) = raws.get(&car.vin)
        {
            let mut raw = raw.clone();
            sources::backfill(&net, source, &mut raw).await;
            car.mileage = raw.mileage;
        }
    }

    // Cheapest first; unpriced cars sink to the bottom.
    cars.sort_by_key(|c| (c.price.is_none(), c.price, c.mileage));

    let generated_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    std::fs::write(&cli.out, report::render_html(&cars, &generated_at))
        .with_context(|| format!("writing {}", cli.out.display()))?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&cars)?);
    } else {
        report::print_table(&cars);
    }
    eprintln!("report written to {}", cli.out.display());
    Ok(())
}

/// Fetches every source concurrently. A failing source is reported and
/// skipped so one dealer outage doesn't sink the run, but if every source
/// fails there is nothing to report on. Listings are deduplicated by VIN,
/// preferring the entry that knows a price.
async fn fetch_all_sources(
    net: &Arc<Net>,
    sources: &[Source],
) -> Result<HashMap<String, (Source, RawListing)>> {
    let mut tasks = tokio::task::JoinSet::new();
    for source in sources.iter().cloned() {
        let net = Arc::clone(net);
        tasks.spawn(async move {
            let result = sources::fetch(&net, &source).await;
            (source, result)
        });
    }

    let mut raws: HashMap<String, (Source, RawListing)> = HashMap::new();
    let mut failures = 0;
    while let Some(joined) = tasks.join_next().await {
        let (source, result) = joined.context("source task panicked")?;
        match result {
            Ok(listings) => {
                eprintln!("{}: {} used BMWs", source.name, listings.len());
                for listing in listings {
                    match raws.entry(listing.vin.clone()) {
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert((source.clone(), listing));
                        }
                        std::collections::hash_map::Entry::Occupied(mut slot) => {
                            let (_, existing) = slot.get();
                            let better = match (existing.price, listing.price) {
                                (None, Some(_)) => true,
                                (Some(old), Some(new)) => new < old,
                                _ => false,
                            };
                            if better {
                                slot.insert((source.clone(), listing));
                            }
                        }
                    }
                }
            }
            Err(err) => {
                failures += 1;
                eprintln!("error: {}: {err:#}", source.name);
            }
        }
    }
    if failures == sources.len() {
        bail!("every source failed; nothing to report");
    }
    Ok(raws)
}

/// Decodes all VINs and keeps the cars the fitment table confirms as B58.
/// SUVs are kept only when `include_suvs` is set; model years newer than
/// `max_year` are dropped.
async fn classify_all(
    client: &reqwest::Client,
    raws: &HashMap<String, (Source, RawListing)>,
    cache_path: &std::path::Path,
    include_suvs: bool,
    max_year: u16,
) -> Result<Vec<B58Car>> {
    let vins: Vec<String> = raws.keys().cloned().collect();
    let decoded = vin::decode_all(client, &vins, cache_path).await?;

    let mut cars = Vec::new();
    for (vin, (_, raw)) in raws {
        let Some(d) = decoded.get(vin) else {
            eprintln!("warning: no VIN decode for {vin} ({}); skipping", raw.title);
            continue;
        };
        if !d.make.eq_ignore_ascii_case("bmw") {
            continue;
        }
        let Some(year) = d.year else {
            eprintln!("warning: no model year for {vin} ({}); skipping", raw.title);
            continue;
        };
        if year > max_year {
            continue;
        }
        let Some(fit) = fitment::classify(year, &d.model, &d.trim) else {
            continue;
        };
        if fit.suv && !include_suvs {
            continue;
        }
        if fitment::engine_contradicts(d.cylinders, d.displacement_l) {
            eprintln!(
                "warning: {vin} ({}) matched \"{}\" but decodes as {:?} cyl / {:?} L; skipping",
                raw.title, fit.designation, d.cylinders, d.displacement_l
            );
            continue;
        }
        cars.push(B58Car {
            vin: vin.clone(),
            year,
            model: d.model.clone(),
            trim: d.trim.clone(),
            fitment: fit.designation.to_string(),
            phev: fit.phev,
            price: raw.price,
            mileage: raw.mileage,
            url: raw.url.clone(),
            photo: raw.photo.clone(),
            dealer: raw.dealer.clone(),
            title: raw.title.clone(),
        });
    }
    Ok(cars)
}
