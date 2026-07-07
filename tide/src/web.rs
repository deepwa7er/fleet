//! tide's HTTP surface, fronted by breakwater at https://tide.intern.deepwa7er.net.
//!
//! - `GET /theme`            → `{"theme":"dark"}`. CORS-open for GET so every
//!   fleet web UI (each on its own subdomain) can read it cross-origin.
//! - `GET /set?theme=dark`   → persist the theme, set a cookie shared across
//!   `*.intern.deepwa7er.net` (first-paint cache for the UIs), and show a
//!   confirmation page. This is what ferry's `b dark` / `b light` redirect to.
//! - `GET /healthz`          → liveness.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, Method, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};

use crate::store::{Store, Theme};

pub fn router(store: Arc<Store>) -> Router {
    // The theme is non-sensitive and read cross-origin by every fleet UI; allow
    // GET from any origin (the control surface here is GET-only anyway).
    let cors = CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]);
    Router::new()
        .route("/theme", get(get_theme))
        .route("/set", get(set_theme))
        .route("/healthz", get(|| async { "ok" }))
        .layer(cors)
        .with_state(store)
}

#[derive(Serialize)]
struct ThemeResponse {
    theme: Theme,
}

/// `GET /theme` — the current fleet theme, for the UIs to fetch and poll.
async fn get_theme(State(store): State<Arc<Store>>) -> Json<ThemeResponse> {
    Json(ThemeResponse { theme: store.get() })
}

#[derive(Deserialize)]
struct SetQuery {
    theme: Option<String>,
}

/// `GET /set?theme=dark|light` — persist the theme, set the shared cookie, and
/// confirm. The browser navigates here (via ferry), so the cookie is set on a
/// real top-level response and the page is a human-facing confirmation.
async fn set_theme(State(store): State<Arc<Store>>, Query(q): Query<SetQuery>) -> Response {
    let Some(theme) = q.theme.as_deref().and_then(|t| t.parse::<Theme>().ok()) else {
        return (StatusCode::BAD_REQUEST, Html(error_page(q.theme.as_deref()))).into_response();
    };

    if let Err(err) = store.set(theme) {
        // The in-memory value still updated, but a failed persist would be lost on
        // restart — surface it rather than claim success.
        eprintln!("tide: persisting theme failed: {err}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Html(persist_failed_page(theme))).into_response();
    }

    let cookie = format!(
        "fleet_theme={theme}; Domain=.intern.deepwa7er.net; Path=/; Max-Age=31536000; SameSite=Lax; Secure"
    );
    ([(header::SET_COOKIE, cookie)], Html(confirm_page(theme))).into_response()
}

/// Minimal confirmation page, itself rendered in the chosen theme.
fn confirm_page(theme: Theme) -> String {
    let (bg, surface, ink, muted, accent, rule) = palette(theme);
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>tide — fleet theme</title>
<style>
  html,body{{height:100%;margin:0}}
  body{{background:{bg};color:{ink};font-family:"Berkeley Mono","JetBrains Mono",ui-monospace,monospace;
    display:flex;align-items:center;justify-content:center}}
  .card{{background:{surface};border:1px solid {rule};padding:1.5rem 2rem;max-width:32rem}}
  h1{{font-size:1.1rem;margin:0 0 .5rem;font-weight:400}}
  .t{{color:{accent}}}
  p{{color:{muted};font-size:.85rem;line-height:1.5;margin:.5rem 0 0}}
  a{{color:{accent};text-decoration:none;border-bottom:1px solid {rule}}}
  nav{{margin-top:1rem;display:flex;flex-wrap:wrap;gap:.75rem;font-size:.85rem}}
</style></head>
<body><div class="card">
  <h1>Fleet theme → <span class="t">{theme}</span> ✓</h1>
  <p>Every fleet service will use <span class="t">{theme}</span> mode. Open tabs flip within a few seconds; new ones are already set.</p>
  <nav>
    <a href="https://lighthouse.intern.deepwa7er.net">lighthouse</a>
    <a href="https://docs.intern.deepwa7er.net">pilot</a>
    <a href="https://lagoon.intern.deepwa7er.net">lagoon</a>
    <a href="https://drydock.intern.deepwa7er.net">drydock</a>
  </nav>
</div></body></html>"#
    )
}

fn error_page(got: Option<&str>) -> String {
    let got = got.unwrap_or("(none)");
    let (bg, surface, ink, muted, accent, rule) = palette(Theme::Dark);
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>tide</title>
<style>html,body{{height:100%;margin:0}}body{{background:{bg};color:{ink};
font-family:ui-monospace,monospace;display:flex;align-items:center;justify-content:center}}
.card{{background:{surface};border:1px solid {rule};padding:1.5rem 2rem;max-width:32rem}}
h1{{font-size:1.1rem;margin:0 0 .5rem;font-weight:400;color:{accent}}}p{{color:{muted};font-size:.85rem;margin:.5rem 0 0}}
code{{color:{ink}}}</style></head>
<body><div class="card"><h1>Unknown theme</h1>
<p>Got <code>{got}</code>. Use <code>?theme=dark</code> or <code>?theme=light</code>.</p></div></body></html>"#
    )
}

fn persist_failed_page(theme: Theme) -> String {
    let (bg, surface, ink, muted, accent, rule) = palette(theme);
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>tide</title>
<style>html,body{{height:100%;margin:0}}body{{background:{bg};color:{ink};
font-family:ui-monospace,monospace;display:flex;align-items:center;justify-content:center}}
.card{{background:{surface};border:1px solid {rule};padding:1.5rem 2rem;max-width:32rem}}
h1{{font-size:1.1rem;margin:0 0 .5rem;font-weight:400;color:{accent}}}p{{color:{muted};font-size:.85rem;margin:.5rem 0 0}}</style></head>
<body><div class="card"><h1>Theme set to {theme}, but not saved</h1>
<p>The fleet is on {theme} for now, but tide couldn't write the settings file, so it may revert on restart. Check the service logs.</p></div></body></html>"#
    )
}

/// (bg, surface, ink, muted, accent, rule) for the confirmation pages — mirrors
/// the fleet's DG-002 TRITIUM tokens so the page matches the theme it just set.
fn palette(theme: Theme) -> (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str) {
    match theme {
        Theme::Dark => ("#000000", "#0a0a0a", "#00a645", "#00753d", "#4a90d4", "#242424"),
        Theme::Light => ("#f4f3ee", "#fafaf8", "#00702f", "#4d7a5e", "#2a6bb0", "#d8d6cd"),
    }
}
