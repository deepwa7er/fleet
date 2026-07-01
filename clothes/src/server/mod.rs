//! HTTP server: JSON API for the web view plus static serving of the built SPA.

mod meta;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::core::model::{Category, Item, Status};
use crate::core::{NewCategory, NewItem, Store, UpdateCategory, UpdateItem};
use crate::error::{Error, Result};

use self::meta::LinkMeta;

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
}

pub struct ServerConfig {
    pub addr: SocketAddr,
    pub web_dir: PathBuf,
}

pub async fn run(store: Arc<Store>, config: ServerConfig) -> std::io::Result<()> {
    let state = AppState { store };

    let index = config.web_dir.join("index.html");
    let static_files = ServeDir::new(&config.web_dir).fallback(ServeFile::new(index));

    let app = Router::new()
        .route("/api/categories", get(list_categories).post(create_category))
        .route(
            "/api/categories/{id}",
            axum::routing::put(update_category).delete(delete_category),
        )
        .route("/api/items", get(list_items).post(create_item))
        .route(
            "/api/items/{id}",
            axum::routing::put(update_item).delete(delete_item),
        )
        .route("/api/items/{id}/status", post(set_item_status))
        .route("/api/fetch-meta", post(fetch_meta))
        // Unknown /api paths must 404 as JSON, not fall through to the SPA shell.
        .route("/api/{*rest}", any(api_not_found))
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!("clothes listening on http://{}", config.addr);
    axum::serve(listener, app).await
}

// ---- request bodies -------------------------------------------------------

#[derive(Deserialize)]
struct CategoryReq {
    name: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    target_count: Option<i64>,
}

#[derive(Deserialize)]
struct ItemReq {
    category_id: i64,
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    store: Option<String>,
    #[serde(default)]
    price_cents: Option<i64>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    status: Option<Status>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct StatusReq {
    status: Status,
}

#[derive(Deserialize)]
struct FetchMetaReq {
    url: String,
}

// ---- helpers --------------------------------------------------------------

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "unknown endpoint" })),
    )
}

fn require(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(Error::BadRequest(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

/// Trim a string, mapping empty/whitespace to `None` so blank form fields clear
/// an optional column rather than storing "".
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn non_negative(field: &str, value: Option<i64>) -> Result<Option<i64>> {
    match value {
        Some(n) if n < 0 => Err(Error::BadRequest(format!("{field} must not be negative"))),
        other => Ok(other),
    }
}

// ---- category handlers ----------------------------------------------------

async fn list_categories(State(st): State<AppState>) -> Result<Json<Vec<Category>>> {
    Ok(Json(st.store.categories()?))
}

async fn create_category(
    State(st): State<AppState>,
    Json(req): Json<CategoryReq>,
) -> Result<(StatusCode, Json<Category>)> {
    require("name", &req.name)?;
    let category = st.store.create_category(NewCategory {
        name: req.name.trim().to_string(),
        note: clean(req.note),
        target_count: non_negative("target_count", req.target_count)?,
    })?;
    Ok((StatusCode::CREATED, Json(category)))
}

async fn update_category(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<CategoryReq>,
) -> Result<Json<Category>> {
    require("name", &req.name)?;
    let category = st.store.update_category(
        id,
        UpdateCategory {
            name: req.name.trim().to_string(),
            note: clean(req.note),
            target_count: non_negative("target_count", req.target_count)?,
        },
    )?;
    Ok(Json(category))
}

async fn delete_category(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    st.store.delete_category(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- item handlers --------------------------------------------------------

async fn list_items(State(st): State<AppState>) -> Result<Json<Vec<Item>>> {
    Ok(Json(st.store.items()?))
}

async fn create_item(
    State(st): State<AppState>,
    Json(req): Json<ItemReq>,
) -> Result<(StatusCode, Json<Item>)> {
    require("url", &req.url)?;
    let item = st.store.create_item(NewItem {
        category_id: req.category_id,
        url: req.url.trim().to_string(),
        title: clean(req.title),
        store: clean(req.store),
        price_cents: non_negative("price_cents", req.price_cents)?,
        image_url: clean(req.image_url),
        status: req.status.unwrap_or_default(),
        size: clean(req.size),
        notes: clean(req.notes),
    })?;
    Ok((StatusCode::CREATED, Json(item)))
}

async fn update_item(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ItemReq>,
) -> Result<Json<Item>> {
    require("url", &req.url)?;
    let item = st.store.update_item(
        id,
        UpdateItem {
            category_id: req.category_id,
            url: req.url.trim().to_string(),
            title: clean(req.title),
            store: clean(req.store),
            price_cents: non_negative("price_cents", req.price_cents)?,
            image_url: clean(req.image_url),
            status: req.status.unwrap_or_default(),
            size: clean(req.size),
            notes: clean(req.notes),
        },
    )?;
    Ok(Json(item))
}

async fn set_item_status(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<StatusReq>,
) -> Result<Json<Item>> {
    Ok(Json(st.store.set_item_status(id, req.status)?))
}

async fn delete_item(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    st.store.delete_item(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- link metadata --------------------------------------------------------

async fn fetch_meta(Json(req): Json<FetchMetaReq>) -> Result<Json<LinkMeta>> {
    require("url", &req.url)?;
    let url = req.url.trim().to_string();
    // The fetch is blocking (HTTP + HTML parse); keep it off the async runtime.
    let meta = tokio::task::spawn_blocking(move || meta::fetch(&url))
        .await
        .map_err(|e| Error::BadRequest(format!("metadata fetch task failed: {e}")))?;
    Ok(Json(meta))
}
