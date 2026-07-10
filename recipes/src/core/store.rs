//! All SQLite access. The binary is the single writer, so one `Connection`
//! behind a `Mutex` is sufficient and trivially correct.

use std::path::Path;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};

use fleet_common::{Error, Result};

use super::model::{join_tags, split_tags, Recipe};

const RECIPE_COLS: &str = "id, title, description, ingredients, steps, tags, servings, \
                           prep_minutes, cook_minutes, source_url, notes, created_at, updated_at";

pub struct Store {
    conn: Mutex<Connection>,
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
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn recipe_from_row(row: &Row) -> rusqlite::Result<Recipe> {
    Ok(Recipe {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        ingredients: row.get(3)?,
        steps: row.get(4)?,
        tags: split_tags(&row.get::<_, String>(5)?),
        servings: row.get(6)?,
        prep_minutes: row.get(7)?,
        cook_minutes: row.get(8)?,
        source_url: row.get(9)?,
        notes: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// Ordered schema migrations. Append-only — never edit a past entry; add a new
/// file and a new line here. The DB's `user_version` records how many have run.
const MIGRATIONS: &[&str] = &[include_str!("../../migrations/001_init.sql")];

impl Store {
    /// Open (creating if needed) the database at `path` and apply any pending
    /// schema migrations. fleet-common owns the open/migrate invariants (WAL,
    /// the FK-off bracket during migration, migration fingerprinting).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = fleet_common::store::open_migrated(path, MIGRATIONS)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    /// Every recipe, ordered like a cookbook index (title, case-insensitive).
    /// Full rows — a personal recipe book stays small enough that a separate
    /// summary shape would be speculative.
    pub fn recipes(&self) -> Result<Vec<Recipe>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RECIPE_COLS} FROM recipes ORDER BY title COLLATE NOCASE ASC, id ASC"
        ))?;
        let rows = stmt
            .query_map([], recipe_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn recipe(&self, id: i64) -> Result<Recipe> {
        Self::load_recipe(&self.lock(), id)
    }

    fn load_recipe(conn: &Connection, id: i64) -> Result<Recipe> {
        conn.query_row(
            &format!("SELECT {RECIPE_COLS} FROM recipes WHERE id = ?1"),
            [id],
            recipe_from_row,
        )
        .optional()?
        .ok_or(Error::NotFound(id))
    }

    pub fn create_recipe(&self, input: NewRecipe) -> Result<Recipe> {
        let conn = self.lock();
        let now = now();
        conn.execute(
            "INSERT INTO recipes
               (title, description, ingredients, steps, tags, servings,
                prep_minutes, cook_minutes, source_url, notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                input.title,
                input.description,
                input.ingredients,
                input.steps,
                join_tags(&input.tags),
                input.servings,
                input.prep_minutes,
                input.cook_minutes,
                input.source_url,
                input.notes,
                now,
            ],
        )?;
        Self::load_recipe(&conn, conn.last_insert_rowid())
    }

    pub fn update_recipe(&self, id: i64, input: UpdateRecipe) -> Result<Recipe> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE recipes SET
               title = ?1, description = ?2, ingredients = ?3, steps = ?4, tags = ?5,
               servings = ?6, prep_minutes = ?7, cook_minutes = ?8, source_url = ?9,
               notes = ?10, updated_at = ?11
             WHERE id = ?12",
            params![
                input.title,
                input.description,
                input.ingredients,
                input.steps,
                join_tags(&input.tags),
                input.servings,
                input.prep_minutes,
                input.cook_minutes,
                input.source_url,
                input.notes,
                now(),
                id,
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Self::load_recipe(&conn, id)
    }

    pub fn delete_recipe(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        let changed = conn.execute("DELETE FROM recipes WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open(":memory:").expect("open in-memory store")
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

    #[test]
    fn recipe_lifecycle() {
        let s = store();
        let r = s
            .create_recipe(NewRecipe {
                tags: vec!["breakfast".into()],
                servings: Some(2),
                prep_minutes: Some(5),
                cook_minutes: Some(10),
                ..new_recipe("Pancakes")
            })
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
            .unwrap();
        assert_eq!(edited.title, "Buttermilk Pancakes");
        assert_eq!(edited.tags, vec!["breakfast", "weekend"]);
        assert_eq!(edited.prep_minutes, None, "cleared");
        assert_eq!(edited.servings, Some(4));

        assert_eq!(s.recipe(r.id).unwrap().title, "Buttermilk Pancakes");
        s.delete_recipe(r.id).unwrap();
        assert!(matches!(s.recipe(r.id), Err(Error::NotFound(_))));
        assert!(matches!(s.delete_recipe(r.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn list_orders_like_a_cookbook_index() {
        let s = store();
        s.create_recipe(new_recipe("shakshuka")).unwrap();
        s.create_recipe(new_recipe("Aglio e Olio")).unwrap();
        s.create_recipe(new_recipe("Borscht")).unwrap();
        let titles: Vec<_> = s.recipes().unwrap().into_iter().map(|r| r.title).collect();
        assert_eq!(titles, vec!["Aglio e Olio", "Borscht", "shakshuka"]);
    }

    #[test]
    fn tags_survive_the_column_round_trip() {
        let s = store();
        let r = s
            .create_recipe(NewRecipe {
                tags: vec!["dinner".into(), "pasta".into()],
                ..new_recipe("Cacio e Pepe")
            })
            .unwrap();
        assert_eq!(s.recipe(r.id).unwrap().tags, vec!["dinner", "pasta"]);
    }
}
