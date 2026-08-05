//! The public HTTP surface.
//!
//! Everything here is a GET that reads the local snapshot. There is no write
//! path, no form, no API, and no code path from a request to Fizzy: a visitor
//! cannot make this service talk to the private side at all, let alone change
//! anything there. That is the whole design — the read-only guarantee is
//! structural, not a permission check that could be misconfigured.
//!
//! Bound to loopback; nginx on the VPS terminates TLS in front of it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use maud::Markup;

use crate::assets::Cache;
use crate::render::{self, Site};
use crate::store::Store;

pub struct App {
    pub store: Arc<Store>,
    pub cache: Arc<Cache>,
    pub site: Site,
}

/// The page's own security policy. There is no JavaScript on any page here, so
/// scripting is refused outright rather than allowlisted; images come only
/// from this origin (the asset cache), which is what makes the "nothing is
/// hot-linked" rule enforceable by the browser as well as by the ingest code.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src 'self'; \
     style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Assets are content-addressed, so a given URL's bytes can never change.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";

/// Pages are regenerated from a snapshot that refreshes on the sync interval;
/// a minute of shared caching absorbs a burst of traffic without making the
/// page meaningfully staler than it already is.
const PAGE_CACHE: &str = "public, max-age=60";

pub async fn run(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let router = Router::new()
        .route("/", get(root))
        .route("/b/{slug}", get(board))
        .route("/b/{slug}/c/{number}", get(card))
        .route("/a/{key}", get(asset))
        .route("/robots.txt", get(robots))
        .route("/healthz", get(fleet_common::http::healthz))
        .fallback(not_found)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

/// With one published board, `/` **is** that board: an index page listing a
/// single entry is a click that buys the reader nothing.
async fn root(State(app): State<Arc<App>>) -> Response {
    let state = match app.store.sync_state() {
        Ok(state) => state,
        Err(e) => return e.into_response(),
    };
    let boards = match app.store.boards() {
        Ok(boards) => boards,
        Err(e) => return e.into_response(),
    };
    match boards.as_slice() {
        [only] => page(render::board(&app.site, only, &state, true), StatusCode::OK),
        many => page(render::index(&app.site, many, &state), StatusCode::OK),
    }
}

async fn board(State(app): State<Arc<App>>, Path(slug): Path<String>) -> Response {
    let state = match app.store.sync_state() {
        Ok(state) => state,
        Err(e) => return e.into_response(),
    };
    match app.store.board(&slug) {
        Ok(Some(found)) => page(
            render::board(&app.site, &found, &state, false),
            StatusCode::OK,
        ),
        Ok(None) => page(render::not_found(&app.site, &state), StatusCode::NOT_FOUND),
        Err(e) => e.into_response(),
    }
}

async fn card(State(app): State<Arc<App>>, Path((slug, number)): Path<(String, i64)>) -> Response {
    let state = match app.store.sync_state() {
        Ok(state) => state,
        Err(e) => return e.into_response(),
    };
    let found = match app.store.board(&slug) {
        Ok(found) => found,
        Err(e) => return e.into_response(),
    };
    match found
        .as_ref()
        .and_then(|b| b.card(number).map(|(c, k)| (b, c, k)))
    {
        Some((board, card, kind)) => page(
            render::card(&app.site, board, card, kind, &state),
            StatusCode::OK,
        ),
        None => page(render::not_found(&app.site, &state), StatusCode::NOT_FOUND),
    }
}

async fn asset(State(app): State<Arc<App>>, Path(key): Path<String>) -> Response {
    match app.cache.read(&key) {
        Some((bytes, content_type)) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, ASSET_CACHE),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                // An image request should never be able to do anything but
                // decode to pixels. `sandbox` neuters an SVG or anything else
                // that turns out to be a document when opened directly.
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox",
                ),
            ],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// The mirror exists to be read, including by crawlers. The only thing worth
/// keeping out of an index is the asset cache, whose URLs are hashes.
async fn robots() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "User-agent: *\nDisallow: /a/\nAllow: /\n",
    )
        .into_response()
}

async fn not_found(State(app): State<Arc<App>>) -> Response {
    let state = app.store.sync_state().unwrap_or_default();
    page(render::not_found(&app.site, &state), StatusCode::NOT_FOUND)
}

fn page(markup: Markup, status: StatusCode) -> Response {
    let mut response = (status, markup.into_string()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if status == StatusCode::OK {
            PAGE_CACHE
        } else {
            "no-store"
        }),
    );
    response
}
