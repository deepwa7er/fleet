# trawler

Drags a net through Portland-area dealership websites and hauls in every
used BMW with the B58 engine. A local CLI, not a fleet service — there is no
`deploy.toml` on purpose.

```
cargo run -p trawler              # table on stdout + b58-report.html
cargo run -p trawler -- --json    # machine-readable instead of the table
cargo run -p trawler -- --suvs    # include the X-series (hidden by default)
cargo run -p trawler -- --max-year 2023   # default caps at model year 2019
cargo run -p trawler -- --config my-dealers.toml --out report.html
```

## How it decides what a B58 car is

Dealer listing titles are unreliable ("2023 BMW X5" may be a B58 xDrive40i
or an N63 M50i), so trawler never trusts them. Every VIN is decoded through
the free NHTSA vPIC batch API, and the decoded model year / model / trim is
matched against a US-market fitment table in `src/fitment.rs` (340i, 440i,
M240i/M340i/M440i, 540i, 740i/745e, 840i, X3 M40i/M50, X4 M40i, X5 40i and
the 45e/50e PHEVs, X6 40i, X7 40i, Z4 M40i, with year guards for names BMW
reused across engines). A decode that contradicts the B58's 3.0 L inline
six drops the car with a warning. Decodes are cached forever in
`~/.cache/trawler/vin-cache.json` — a VIN never changes.

## Sources

Configured in TOML; the compiled-in baseline covers the sites that were
verified scrapeable:

| source | adapter | how |
|---|---|---|
| BMW of Portland | `dealeron` | DealerOn "Cosmos" JSON API behind the used-search page |
| Royal Moore Toyota, Canby Ford, Newberg Ford | `dealeron` | same API; non-BMW dealers still take BMW trade-ins |
| Freeman Motor Company | `spaceauto` | Space Auto JSON inventory dump linked from the homepage |
| BMW of Salem, Toyota of Portland | `dealercom` | Dealer.com state blob in the rendered page, via the headless browser (their WAF rejects plain HTTP) |
| Craigslist Portland + Salem (dealers) | `craigslist` | server-rendered search cards; price-range bisection beats the ~340-result render cap; candidate detail pages fetched for VIN/odometer |

The `dealercom` adapter needs a Chromium-family browser (Brave, Chrome,
Chromium, or Edge; `TRAWLER_BROWSER` overrides detection) and keeps a
dedicated profile in `~/.cache/trawler/browser-profile` — the first-ever run
warms it, which can take a few minutes. It is a real browser doing a real
page load: no stealth patches or fingerprint spoofing. Sites that challenge
even that (DealerInspire stores like BMW of Tigard, cars.com, AutoTrader,
CarGurus) stay unsupported on purpose.

The Craigslist adapter covers every dealership that cross-posts there, which
is most of the metro's used lots. Its one recall limit: a post whose title
never names the trim ("2018 BMW 3 Series") is skipped rather than fetching
hundreds of detail pages per run, and a post without a VIN is skipped with a
warning because the engine cannot be verified.

To add a dealer, copy the shape of the baseline in `src/config.rs` into a
file and pass `--config`. Only sites on one of the implemented platforms
work; notably **BMW of Tigard** (DealerInspire) and **BMW of Salem**
(Lithia) sit behind TLS-fingerprinting bot walls that a plain HTTP client
cannot pass, and trawler does not try to evade them. The big aggregators
(cars.com, AutoTrader, CarGurus) block the same way, which is why per-dealer
adapters exist at all.

Be a polite guest: trawler paginates with a delay, sends a handful of
requests per run, and should stay that way.
