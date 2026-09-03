//! All database access, through keep. The binary holds no local data: one
//! [`Client`] per database, mirroring the old one-file-per-app shape, with
//! schema migrations applied at startup through fleet-common's remote
//! migrate (WAL-equivalent consistency comes from keep's per-database lock;
//! the FK-off bracket and migration fingerprinting are enforced remotely).

use fleet_common::keep::{Client, Value};

use fleet_common::{Error, Result};

use super::model::{join_tags, split_tags, Recipe};

const RECIPE_COLS: &str = "id, title, description, ingredients, steps, tags, servings, \
                           prep_minutes, cook_minutes, source_url, notes, created_at, updated_at";

pub struct Store {
    client: Client,
}

/// Fields for a new recipe. `title`, `ingredients`, and `steps` are required
/// (the server validates non-emptiness); everything else is optional.
pub struct NewRecipe {
    pub title: String,
    pub description: Option<String>,
    pub ingredients: String,
    pub steps: String,
    pub tags: Vec<String>,
    pub servings: Option<i64>,
    pub prep_minutes: Option<i64>,
    pub cook_minutes: Option<i64>,
    pub source_url: Option<String>,
    pub notes: Option<String>,
}

/// Editable fields of a recipe (a full replace of the editable set, so
/// clearing an optional field is unambiguous).
pub struct UpdateRecipe {
    pub title: String,
    pub description: Option<String>,
    pub ingredients: String,
    pub steps: String,
    pub tags: Vec<String>,
    pub servings: Option<i64>,
    pub prep_minutes: Option<i64>,
    pub cook_minutes: Option<i64>,
    pub source_url: Option<String>,
    pub notes: Option<String>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Typed cell readers: a wrong-typed cell is data corruption, and corruption
/// must fail loudly rather than coerce.
fn integer(cells: &[Value], idx: usize) -> Result<i64> {
    match cells.get(idx) {
        Some(Value::Integer(v)) => Ok(*v),
        other => Err(Error::Internal(format!(
            "recipe column {idx} holds {other:?}, want integer"
        ))),
    }
}

fn text(cells: &[Value], idx: usize) -> Result<String> {
    match cells.get(idx) {
        Some(Value::Text(v)) => Ok(v.clone()),
        other => Err(Error::Internal(format!(
            "recipe column {idx} holds {other:?}, want text"
        ))),
    }
}

fn maybe_text(cells: &[Value], idx: usize) -> Result<Option<String>> {
    match cells.get(idx) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Text(v)) => Ok(Some(v.clone())),
        other => Err(Error::Internal(format!(
            "recipe column {idx} holds {other:?}, want text or null"
        ))),
    }
}

fn maybe_integer(cells: &[Value], idx: usize) -> Result<Option<i64>> {
    match cells.get(idx) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Integer(v)) => Ok(Some(*v)),
        other => Err(Error::Internal(format!(
            "recipe column {idx} holds {other:?}, want integer or null"
        ))),
    }
}

fn recipe_from_cells(cells: &[Value]) -> Result<Recipe> {
    Ok(Recipe {
        id: integer(cells, 0)?,
        title: text(cells, 1)?,
        description: maybe_text(cells, 2)?,
        ingredients: text(cells, 3)?,
        steps: text(cells, 4)?,
        tags: split_tags(&text(cells, 5)?),
        servings: maybe_integer(cells, 6)?,
        prep_minutes: maybe_integer(cells, 7)?,
        cook_minutes: maybe_integer(cells, 8)?,
        source_url: maybe_text(cells, 9)?,
        notes: maybe_text(cells, 10)?,
        created_at: text(cells, 11)?,
        updated_at: text(cells, 12)?,
    })
}

/// Ordered schema migrations. Append-only — never edit a past entry; add a new
/// file and a new line here. Applied through keep at startup, fingerprinted
/// exactly like the local runner did.
const MIGRATIONS: &[&str] = &[include_str!("../../migrations/001_init.sql")];

impl Store {
    /// Connect to keep and apply any pending schema migrations.
    /// fleet-common owns the remote open/migrate invariants (the FK-off
    /// bracket during migration, migration fingerprinting).
    pub async fn open(client: Client) -> Result<Self> {
        fleet_common::store::open_migrated_remote(&client, MIGRATIONS).await?;
        Ok(Store { client })
    }

    /// The underlying client, for operations outside the store's shape —
    /// the one-time import is the only one.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Every recipe, ordered like a cookbook index (title, case-insensitive).
    /// Full rows — a personal recipe book stays small enough that a separate
    /// summary shape would be speculative.
    pub async fn recipes(&self) -> Result<Vec<Recipe>> {
        let outcome = self
            .client
            .query(
                &format!(
                    "SELECT {RECIPE_COLS} FROM recipes ORDER BY title COLLATE NOCASE ASC, id ASC"
                ),
                vec![],
            )
            .await?;
        outcome.rows.iter().map(|cells| recipe_from_cells(cells)).collect()
    }

    pub async fn recipe(&self, id: i64) -> Result<Recipe> {
        Self::load_recipe(&self.client, id).await
    }

    async fn load_recipe(client: &Client, id: i64) -> Result<Recipe> {
        let outcome = client
            .query(
                &format!("SELECT {RECIPE_COLS} FROM recipes WHERE id = ?1"),
                vec![Value::from(id)],
            )
            .await?;
        outcome
            .rows
            .first()
            .map(|cells| recipe_from_cells(cells))
            .transpose()?
            .ok_or(Error::NotFound(id))
    }

    pub async fn create_recipe(&self, input: NewRecipe) -> Result<Recipe> {
        let now = now();
        let outcome = self
            .client
            .query(
                "INSERT INTO recipes
                   (title, description, ingredients, steps, tags, servings,
                    prep_minutes, cook_minutes, source_url, notes, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                vec![
                    input.title.into(),
                    input.description.into(),
                    input.ingredients.into(),
                    input.steps.into(),
                    join_tags(&input.tags).into(),
                    input.servings.into(),
                    input.prep_minutes.into(),
                    input.cook_minutes.into(),
                    input.source_url.into(),
                    input.notes.into(),
                    now.into(),
                ],
            )
            .await?;
        Self::load_recipe(&self.client, outcome.rowid).await
    }

    pub async fn update_recipe(&self, id: i64, input: UpdateRecipe) -> Result<Recipe> {
        let changed = self
            .client
            .query(
                "UPDATE recipes SET
                   title = ?1, description = ?2, ingredients = ?3, steps = ?4, tags = ?5,
                   servings = ?6, prep_minutes = ?7, cook_minutes = ?8, source_url = ?9,
                   notes = ?10, updated_at = ?11
                 WHERE id = ?12",
                vec![
                    input.title.into(),
                    input.description.into(),
                    input.ingredients.into(),
                    input.steps.into(),
                    join_tags(&input.tags).into(),
                    input.servings.into(),
                    input.prep_minutes.into(),
                    input.cook_minutes.into(),
                    input.source_url.into(),
                    input.notes.into(),
                    now().into(),
                    id.into(),
                ],
            )
            .await?
            .changes;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Self::load_recipe(&self.client, id).await
    }

    pub async fn delete_recipe(&self, id: i64) -> Result<()> {
        let changed = self
            .client
            .query("DELETE FROM recipes WHERE id = ?1", vec![Value::from(id)])
            .await?
            .changes;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;

    /// A real embedded keep. The address is shared so a test can open any
    /// number of clients (and stores) against the same database — which is
    /// how the reopen path gets exercised.
    struct Backend {
        addr: SocketAddr,
    }

    async fn start(name: &str) -> Backend {
        let dir = std::env::temp_dir().join(format!("recipes-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let registry = keep::store::Registry::open(&dir, vec![("testdb".into(), "secret".into())])
            .await
            .unwrap();
        let app = keep::server::router(std::sync::Arc::new(keep::server::AppState {
            registry: std::sync::Arc::new(registry),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Backend { addr }
    }

    fn client(backend: &Backend) -> Client {
        Client::new(&format!("http://{}", backend.addr), "testdb", "secret")
    }

    /// A migrated store plus its backend. Asserts the migration pass ran
    /// server-side (`user_version == 1`), so a silently skipped migration
    /// fails here rather than in a later test's confusing aftermath.
    async fn store(name: &str) -> (Backend, Store) {
        let backend = start(name).await;
        let store = Store::open(client(&backend)).await.expect("migrated open");
        let version = client(&backend)
            .query("PRAGMA user_version", vec![])
            .await
            .unwrap();
        assert_eq!(version.rows, vec![vec![Value::Integer(1)]]);
        (backend, store)
    }

    fn new_recipe(title: &str) -> NewRecipe {
        NewRecipe {
            title: title.into(),
            description: None,
            ingredients: "2 eggs\n100g flour".into(),
            steps: "Whisk the eggs.\nFold in the flour.".into(),
            tags: vec![],
            servings: None,
            prep_minutes: None,
            cook_minutes: None,
            source_url: None,
            notes: None,
        }
    }

    #[tokio::test]
    async fn reopen_verifies_and_sees_existing_data() {
        let (backend, first) = store("reopen").await;
        let r = first.create_recipe(new_recipe("Reopen Me")).await.unwrap();
        // A second open against the same database verifies hashes and
        // applies nothing — then sees the first store's rows.
        let second = Store::open(client(&backend)).await.expect("reopen");
        assert_eq!(second.recipe(r.id).await.unwrap().title, "Reopen Me");
    }

    #[tokio::test]
    async fn recipe_lifecycle() {
        let (_backend, s) = store("lifecycle").await;
        let r = s
            .create_recipe(NewRecipe {
                tags: vec!["breakfast".into()],
                servings: Some(2),
                prep_minutes: Some(5),
                cook_minutes: Some(10),
                ..new_recipe("Pancakes")
            })
            .await
            .unwrap();
        assert_eq!(r.title, "Pancakes");
        assert_eq!(r.tags, vec!["breakfast"]);
        assert_eq!(r.created_at, r.updated_at);

        let edited = s
            .update_recipe(
                r.id,
                UpdateRecipe {
                    title: "Buttermilk Pancakes".into(),
                    description: Some("Fluffy weekend stack.".into()),
                    ingredients: r.ingredients.clone(),
                    steps: r.steps.clone(),
                    tags: vec!["breakfast".into(), "weekend".into()],
                    servings: Some(4),
                    prep_minutes: None,
                    cook_minutes: Some(15),
                    source_url: None,
                    notes: Some("double the batch".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(edited.title, "Buttermilk Pancakes");
        assert_eq!(edited.tags, vec!["breakfast", "weekend"]);
        assert_eq!(edited.prep_minutes, None, "cleared");
        assert_eq!(edited.servings, Some(4));

        assert_eq!(s.recipe(r.id).await.unwrap().title, "Buttermilk Pancakes");
        s.delete_recipe(r.id).await.unwrap();
        assert!(matches!(s.recipe(r.id).await, Err(Error::NotFound(_))));
        assert!(matches!(s.delete_recipe(r.id).await, Err(Error::NotFound(_))));
    }

    #[tokio::test]
    async fn list_orders_like_a_cookbook_index() {
        let (_backend, s) = store("index").await;
        s.create_recipe(new_recipe("shakshuka")).await.unwrap();
        s.create_recipe(new_recipe("Aglio e Olio")).await.unwrap();
        s.create_recipe(new_recipe("Borscht")).await.unwrap();
        let titles: Vec<_> = s.recipes().await.unwrap().into_iter().map(|r| r.title).collect();
        assert_eq!(titles, vec!["Aglio e Olio", "Borscht", "shakshuka"]);
    }

    #[tokio::test]
    async fn tags_survive_the_column_round_trip() {
        let (_backend, s) = store("tags").await;
        let r = s
            .create_recipe(NewRecipe {
                tags: vec!["dinner".into(), "pasta".into()],
                ..new_recipe("Cacio e Pepe")
            })
            .await
            .unwrap();
        assert_eq!(s.recipe(r.id).await.unwrap().tags, vec!["dinner", "pasta"]);
    }
}
