//! Renders JavaScript-heavy pages by shelling out to a Chromium-family
//! browser in headless mode (`--headless=new --dump-dom`).
//!
//! Some dealer platforms sit behind TLS-fingerprinting WAFs that reject any
//! plain HTTP client but pass a real browser (e.g. Lithia's Dealer.com
//! storefronts). A real browser engine is the honest way through: no
//! stealth patches, no fingerprint spoofing — sites that challenge even a
//! real headless browser (Cloudflare-managed DealerInspire stores, the big
//! aggregators) stay unsupported.
//!
//! Implementation is a subprocess per page load rather than a CDP client
//! crate: `--dump-dom` is enough for pages that render inventory on load,
//! and it keeps the dependency tree flat.
//!
//! ## Two observed Brave quirks this module works around
//!
//! **The process lingers after dumping.** Brave 149 with `--dump-dom` often
//! writes the complete DOM and then never exits. So stdout goes to a file
//! that is polled for the closing `</html>`; once the dump is complete (or
//! the process exits on its own) the browser is killed and the file read.
//! Waiting for exit alone would misclassify every successful lingering dump
//! as a timeout.
//!
//! **Fresh profiles need a long first run.** Runs use a dedicated
//! persistent profile so they can never touch (or deadlock against) the
//! user's real browser profile. The FIRST browser process ever started on a
//! profile directory spends several minutes on one-time initialization
//! (component installs, first-run services) and dumps nothing until that
//! settles. So the first use warms the profile with a generous window and
//! records success in a marker file; later runs skip straight to fast
//! loads.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// Wall-clock cap per page load on a warmed profile. Dealer pages drag in
/// enough third-party scripts that two minutes is not paranoia.
const LOAD_TIMEOUT: Duration = Duration::from_secs(150);
/// Wall-clock cap for the one-time profile warmup.
const WARMUP_TIMEOUT: Duration = Duration::from_secs(300);
/// Browser-side navigation timeout (ms), a backstop under the wall clocks.
const NAVIGATION_TIMEOUT_MS: u32 = 120_000;
/// Virtual time granted to page JavaScript before the DOM is dumped.
/// Without this the dump fires at the load event, before the XHRs that
/// populate inventory widgets have landed.
const VIRTUAL_TIME_BUDGET_MS: u32 = 20_000;
/// How often the dump file is checked for completion.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// A small, stable page whose successful render proves the profile works.
const WARMUP_URL: &str = "https://example.com/";
/// Marker file recording that this profile has rendered a page successfully.
const WARMED_MARKER: &str = ".trawler-warmed";

/// Chromium-family binaries to try, most-preferred first. Absolute paths
/// are macOS app bundles; bare names are resolved through `PATH` (Linux).
const CANDIDATES: &[&str] = &[
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "google-chrome",
    "brave-browser",
    "chromium",
    "chromium-browser",
];

pub struct Browser {
    binary: PathBuf,
    profile_dir: PathBuf,
    /// Serializes page loads: concurrent launches would race on the profile
    /// directory's singleton lock, and one page at a time is politer anyway.
    lock: tokio::sync::Mutex<()>,
}

/// True when the dump file ends with a closing `</html>` tag — `--dump-dom`
/// writes the serialized document in one final burst, so a present
/// terminator means the dump is done even if the process lingers.
fn dump_is_complete(path: &std::path::Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return false;
    };
    if len < 256 {
        return false;
    }
    let tail_len = len.min(64);
    if file.seek(SeekFrom::End(-(tail_len as i64))).is_err() {
        return false;
    }
    let mut tail = Vec::with_capacity(tail_len as usize);
    if file.read_to_end(&mut tail).is_err() {
        return false;
    }
    String::from_utf8_lossy(&tail)
        .trim_end()
        .ends_with("</html>")
}

/// Resolves a bare command name through `PATH`.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

impl Browser {
    /// Finds a usable browser: `$TRAWLER_BROWSER` if set, then the known
    /// install locations.
    pub fn detect() -> Result<Browser> {
        let binary = if let Some(overridden) = std::env::var_os("TRAWLER_BROWSER") {
            let path = PathBuf::from(&overridden);
            if !path.is_file() {
                bail!("TRAWLER_BROWSER={} is not a file", path.display());
            }
            path
        } else {
            CANDIDATES
                .iter()
                .find_map(|c| {
                    let path = PathBuf::from(c);
                    if path.is_absolute() {
                        path.is_file().then_some(path)
                    } else {
                        find_in_path(c)
                    }
                })
                .context(
                    "no Chromium-family browser found (Brave/Chrome/Chromium/Edge); \
                     install one or set TRAWLER_BROWSER to a binary path",
                )?
        };
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let profile_dir = PathBuf::from(home).join(".cache/trawler/browser-profile");
        std::fs::create_dir_all(&profile_dir)
            .with_context(|| format!("creating {}", profile_dir.display()))?;
        Ok(Browser {
            binary,
            profile_dir,
            lock: tokio::sync::Mutex::new(()),
        })
    }

    /// One raw page load; the caller holds the lock and picks the budget.
    /// Succeeds as soon as the dump file holds a complete document, whether
    /// or not the browser process has deigned to exit.
    async fn raw_dump(&self, url: &str, budget: Duration) -> Result<String> {
        static DUMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let dump_path = std::env::temp_dir().join(format!(
            "trawler-dump-{}-{}.html",
            std::process::id(),
            DUMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let dump_file = std::fs::File::create(&dump_path)
            .with_context(|| format!("creating {}", dump_path.display()))?;

        let mut child = Command::new(&self.binary)
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg(format!("--user-data-dir={}", self.profile_dir.display()))
            .arg(format!("--virtual-time-budget={VIRTUAL_TIME_BUDGET_MS}"))
            .arg(format!("--timeout={NAVIGATION_TIMEOUT_MS}"))
            .arg("--dump-dom")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(dump_file)
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("launching {} for {url}", self.binary.display()))?;

        let deadline = tokio::time::Instant::now() + budget;
        let timed_out = loop {
            if child
                .try_wait()
                .context("polling browser process")?
                .is_some()
            {
                break false;
            }
            if dump_is_complete(&dump_path) {
                let _ = child.kill().await;
                break false;
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                break true;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        };

        let dom = std::fs::read_to_string(&dump_path).unwrap_or_default();
        let _ = std::fs::remove_file(&dump_path);
        if dom.len() >= 256 && dom.trim_end().ends_with("</html>") {
            Ok(dom)
        } else if timed_out {
            bail!("browser produced no complete DOM for {url} within {budget:?}");
        } else {
            bail!(
                "browser exited without a complete DOM for {url} ({} bytes)",
                dom.len()
            );
        }
    }

    /// First-use warmup (see module docs). The first attempt gets the long
    /// window; if it never settles it is killed, and a second attempt on the
    /// now-initialized profile must succeed quickly.
    async fn ensure_warm(&self) -> Result<()> {
        let marker = self.profile_dir.join(WARMED_MARKER);
        if marker.is_file() {
            return Ok(());
        }
        eprintln!(
            "warming up the headless browser profile (one-time, up to {} minutes)",
            (WARMUP_TIMEOUT + LOAD_TIMEOUT).as_secs() / 60
        );
        if self.raw_dump(WARMUP_URL, WARMUP_TIMEOUT).await.is_err() {
            self.raw_dump(WARMUP_URL, LOAD_TIMEOUT).await.context(
                "the browser profile never became usable; delete \
                 ~/.cache/trawler/browser-profile and retry",
            )?;
        }
        std::fs::write(&marker, b"").with_context(|| format!("writing {}", marker.display()))?;
        Ok(())
    }

    /// Loads `url`, lets its JavaScript run, and returns the rendered DOM.
    pub async fn dump_dom(&self, url: &str) -> Result<String> {
        let _serialized = self.lock.lock().await;
        self.ensure_warm().await?;
        self.raw_dump(url, LOAD_TIMEOUT).await
    }
}
