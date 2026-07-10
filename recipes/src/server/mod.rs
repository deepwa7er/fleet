//! HTTP server: JSON API for the web view plus static serving of the built SPA.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{any, get};
use axum::{Json, Router};
use fleet_common::http::{api_not_found, healthz, spa};
use serde::Deserialize;
use tower_http::trace::TraceLayer;

use crate::core::model::{normalize_tags, Recipe};
use crate::core::{NewRecipe, Store, UpdateRecipe};
use fleet_common::{Error, Result};

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

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/recipes", get(list_recipes).post(create_recipe))
        .route(
            "/api/recipes/{id}",
            get(get_recipe).put(update_recipe).delete(delete_recipe),
        )
        // Unknown /api paths must 404 as JSON, not fall through to the SPA shell.
        .route("/api/{*rest}", any(api_not_found))
        .fallback_service(spa(&config.web_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!("recipes listening on http://{}", config.addr);
    axum::serve(listener, app).await
}

// ---- request bodies ---------------------------------------------------------

/// Create and update share one body: the server does a full replace of the
/// editable set, so a blank optional field clears the column.
#[derive(Deserialize)]
struct RecipeReq {
    title: String,
    #[serde(default)]
    description: Option<String>,
    ingredients: String,
    steps: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    servings: Option<i64>,
    #[serde(default)]
    prep_minutes: Option<i64>,
    #[serde(default)]
    cook_minutes: Option<i64>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

// ---- helpers ----------------------------------------------------------------

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

/// Trim a multi-line field and drop blank lines, so stored ingredients/steps
/// split cleanly back into list entries.
fn clean_lines(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn non_negative(field: &str, value: Option<i64>) -> Result<Option<i64>> {
    match value {
        Some(n) if n < 0 => Err(Error::BadRequest(format!("{field} must not be negative"))),
        other => Ok(other),
    }
}

impl RecipeReq {
    /// Validate and normalize into the store's shape. `NewRecipe` and
    /// `UpdateRecipe` carry identical fields, so this builds the former and
    /// update converts.
    fn into_new(self) -> Result<NewRecipe> {
        require("title", &self.title)?;
        require("ingredients", &self.ingredients)?;
        require("steps", &self.steps)?;
        Ok(NewRecipe {
            title: self.title.trim().to_string(),
            description: clean(self.description),
            ingredients: clean_lines(&self.ingredients),
            steps: clean_lines(&self.steps),
            tags: normalize_tags(self.tags),
            servings: non_negative("servings", self.servings)?,
            prep_minutes: non_negative("prep_minutes", self.prep_minutes)?,
            cook_minutes: non_negative("cook_minutes", self.cook_minutes)?,
            source_url: clean(self.source_url),
            notes: clean(self.notes),
        })
    }

    fn into_update(self) -> Result<UpdateRecipe> {
        let n = self.into_new()?;
        Ok(UpdateRecipe {
            title: n.title,
            description: n.description,
            ingredients: n.ingredients,
            steps: n.steps,
            tags: n.tags,
            servings: n.servings,
            prep_minutes: n.prep_minutes,
            cook_minutes: n.cook_minutes,
            source_url: n.source_url,
            notes: n.notes,
        })
    }
}

// ---- handlers -----------------------------------------------------------------

async fn list_recipes(State(st): State<AppState>) -> Result<Json<Vec<Recipe>>> {
    Ok(Json(st.store.recipes()?))
}

async fn get_recipe(State(st): State<AppState>, Path(id): Path<i64>) -> Result<Json<Recipe>> {
    Ok(Json(st.store.recipe(id)?))
}

async fn create_recipe(
    State(st): State<AppState>,
    Json(req): Json<RecipeReq>,
) -> Result<(StatusCode, Json<Recipe>)> {
    let recipe = st.store.create_recipe(req.into_new()?)?;
    Ok((StatusCode::CREATED, Json(recipe)))
}

async fn update_recipe(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<RecipeReq>,
) -> Result<Json<Recipe>> {
    Ok(Json(st.store.update_recipe(id, req.into_update()?)?))
}

async fn delete_recipe(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    st.store.delete_recipe(id)?;
    Ok(StatusCode::NO_CONTENT)
}
