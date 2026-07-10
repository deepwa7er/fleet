//! Which dealer sites to trawl. A baseline config covering the verified
//! Portland-area sources is compiled in; `--config` swaps in a user file of
//! the same shape.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The dealer-platform adapters trawler implements.
///
/// Sites behind Cloudflare-style bot walls (DealerInspire — BMW of Tigard,
/// Lithia — BMW of Salem) cannot be fetched without a real browser and are
/// deliberately not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Adapter {
    /// Craigslist area searches; `url` is a full search URL (the adapter
    /// appends price-range parameters for pagination).
    Craigslist,
    /// Dealer.com storefronts (e.g. bmwofsalem.com), rendered through the
    /// headless browser because their WAFs reject plain HTTP clients.
    DealerCom,
    /// DealerOn "Cosmos" storefronts (e.g. bmwofportland.com).
    DealerOn,
    /// Space Auto WordPress storefronts (e.g. freemanmotor.com).
    SpaceAuto,
}

/// One dealer website to trawl.
#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    /// Display name used in output, e.g. "BMW of Portland".
    pub name: String,
    pub adapter: Adapter,
    /// Site origin, e.g. "https://www.bmwofportland.com". No trailing slash.
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(rename = "source")]
    pub sources: Vec<Source>,
}

/// The verified Portland-area sources, shipped as the default config.
const BASELINE: &str = r#"
[[source]]
name = "BMW of Portland"
adapter = "dealeron"
url = "https://www.bmwofportland.com"

[[source]]
name = "Freeman Motor Company"
adapter = "spaceauto"
url = "https://freemanmotor.com"

[[source]]
name = "Royal Moore Toyota"
adapter = "dealeron"
url = "https://www.royalmooretoyota.com"

[[source]]
name = "Canby Ford"
adapter = "dealeron"
url = "https://www.canbyford.com"

[[source]]
name = "Newberg Ford"
adapter = "dealeron"
url = "https://www.newbergford.com"

[[source]]
name = "BMW of Salem"
adapter = "dealercom"
url = "https://www.bmwofsalem.com"

[[source]]
name = "Toyota of Portland"
adapter = "dealercom"
url = "https://www.toyotaofportland.com"

[[source]]
name = "Craigslist Portland (dealers)"
adapter = "craigslist"
url = "https://www.craigslist.org/search/area/portland?cat=cta&purveyor=dealer&query=bmw"

[[source]]
name = "Craigslist Salem (dealers)"
adapter = "craigslist"
url = "https://www.craigslist.org/search/area/salem?cat=cta&purveyor=dealer&query=bmw"
"#;

impl Config {
    pub fn baseline() -> Config {
        toml::from_str(BASELINE).expect("baseline config must parse")
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        if config.sources.is_empty() {
            bail!("config {} defines no [[source]] entries", path.display());
        }
        for source in &config.sources {
            if source.url.ends_with('/') {
                bail!(
                    "source \"{}\": url must not end with a slash (got {})",
                    source.name,
                    source.url
                );
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::{Adapter, Config};

    #[test]
    fn baseline_parses() {
        let config = Config::baseline();
        assert_eq!(config.sources.len(), 9);
        let count = |wanted: Adapter| {
            config
                .sources
                .iter()
                .filter(|s| s.adapter == wanted)
                .count()
        };
        assert_eq!(count(Adapter::DealerOn), 4);
        assert_eq!(count(Adapter::SpaceAuto), 1);
        assert_eq!(count(Adapter::Craigslist), 2);
        assert_eq!(count(Adapter::DealerCom), 2);
    }
}
