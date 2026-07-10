//! VIN decoding through the NHTSA vPIC batch API, with a local cache.
//!
//! The decode is the single source of truth for what a listing actually is —
//! dealer titles routinely omit the trim that separates a B58 X5 xDrive40i
//! from an N63 X5 M50i. Decodes are immutable facts about a VIN, so the
//! cache never expires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const VPIC_BATCH_URL: &str = "https://vpic.nhtsa.dot.gov/api/vehicles/DecodeVINValuesBatch/";
/// vPIC accepts at most 50 VINs per batch request.
const BATCH_SIZE: usize = 50;

/// The vPIC fields the fitment table needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decoded {
    pub year: Option<u16>,
    pub make: String,
    pub model: String,
    /// vPIC `Series` and `Trim` joined with a space; either may be empty.
    pub trim: String,
    pub cylinders: Option<u8>,
    pub displacement_l: Option<f32>,
}

/// One entry of the vPIC batch response, as returned by the API.
#[derive(Debug, Deserialize)]
struct VpicRecord {
    #[serde(rename = "VIN", default)]
    vin: String,
    #[serde(rename = "ModelYear", default)]
    model_year: String,
    #[serde(rename = "Make", default)]
    make: String,
    #[serde(rename = "Model", default)]
    model: String,
    #[serde(rename = "Series", default)]
    series: String,
    #[serde(rename = "Trim", default)]
    trim: String,
    #[serde(rename = "EngineCylinders", default)]
    engine_cylinders: String,
    #[serde(rename = "DisplacementL", default)]
    displacement_l: String,
}

#[derive(Debug, Deserialize)]
struct VpicResponse {
    #[serde(rename = "Results")]
    results: Vec<VpicRecord>,
}

impl From<VpicRecord> for Decoded {
    fn from(r: VpicRecord) -> Self {
        Decoded {
            year: r.model_year.parse().ok(),
            make: r.make,
            model: r.model,
            trim: [r.series, r.trim].join(" ").trim().to_string(),
            cylinders: r.engine_cylinders.parse().ok(),
            displacement_l: r.displacement_l.parse().ok(),
        }
    }
}

/// Default cache path: `~/.cache/trawler/vin-cache.json`.
pub fn default_cache_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set; pass --vin-cache")?;
    Ok(PathBuf::from(home).join(".cache/trawler/vin-cache.json"))
}

fn load_cache(path: &Path) -> HashMap<String, Decoded> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    match serde_json::from_slice(&bytes) {
        Ok(cache) => cache,
        Err(err) => {
            eprintln!(
                "warning: ignoring unreadable VIN cache {}: {err}",
                path.display()
            );
            HashMap::new()
        }
    }
}

fn store_cache(path: &Path, cache: &HashMap<String, Decoded>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating cache directory {}", dir.display()))?;
    }
    let json = serde_json::to_vec_pretty(cache)?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

/// Decodes every VIN, reading and updating the cache file at `cache_path`.
/// VINs vPIC cannot make sense of (no make/model) are omitted from the result.
pub async fn decode_all(
    client: &reqwest::Client,
    vins: &[String],
    cache_path: &Path,
) -> Result<HashMap<String, Decoded>> {
    let mut cache = load_cache(cache_path);

    let missing: Vec<&String> = vins.iter().filter(|v| !cache.contains_key(*v)).collect();
    if !missing.is_empty() {
        eprintln!(
            "decoding {} VINs via NHTSA vPIC ({} already cached)",
            missing.len(),
            vins.len() - missing.len()
        );
        for batch in missing.chunks(BATCH_SIZE) {
            let data: Vec<&str> = batch.iter().map(|v| v.as_str()).collect();
            let response: VpicResponse = client
                .post(VPIC_BATCH_URL)
                .form(&[("format", "json"), ("data", &data.join(";"))])
                .send()
                .await
                .context("requesting NHTSA vPIC batch decode")?
                .error_for_status()
                .context("NHTSA vPIC returned an error status")?
                .json()
                .await
                .context("parsing NHTSA vPIC response")?;
            for record in response.results {
                if record.vin.is_empty() {
                    continue;
                }
                let vin = record.vin.clone();
                cache.insert(vin, Decoded::from(record));
            }
        }
        store_cache(cache_path, &cache)?;
    }

    Ok(vins
        .iter()
        .filter_map(|v| cache.get(v).map(|d| (v.clone(), d.clone())))
        .filter(|(_, d)| !d.make.is_empty() && !d.model.is_empty())
        .collect())
}
