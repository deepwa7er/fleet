//! All SQLite access. The binary is the single writer, so one `Connection`
//! behind a `Mutex` is sufficient and trivially correct.

use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::model::*;
use crate::error::{Error, Result};

const CATEGORY_COLS: &str = "id, name, note, target_count, sort_order, created_at";
const ITEM_COLS: &str = "id, category_id, url, title, store, price_cents, \
                         image_url, status, size, notes, sort_order, created_at, updated_at";

pub struct Store {
    conn: Mutex<Connection>,
}

/// Fields for a new category. `note`/`target_count` are optional.
pub struct NewCategory {
    pub name: String,
    pub note: Option<String>,
    pub target_count: Option<i64>,
}

/// Editable fields of a category (a full replace of the editable set).
pub struct UpdateCategory {
    pub name: String,
    pub note: Option<String>,
    pub target_count: Option<i64>,
}

/// Fields for a new item. Only `category_id` and `url` are required.
pub struct NewItem {
    pub category_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub store: Option<String>,
    pub price_cents: Option<i64>,
    pub image_url: Option<String>,
    pub status: Status,
    pub size: Option<String>,
    pub notes: Option<String>,
}

/// Editable fields of an item (a full replace of the editable set, so moving an
/// item between categories or clearing an optional field is unambiguous).
pub struct UpdateItem {
    pub category_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub store: Option<String>,
    pub price_cents: Option<i64>,
    pub image_url: Option<String>,
    pub status: Status,
    pub size: Option<String>,
    pub notes: Option<String>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse a TEXT column into a string-backed enum, surfacing a precise SQLite
/// conversion error rather than panicking if the DB ever holds a bad value.
fn parse_col<T: FromStr<Err = String>>(s: &str, col: usize) -> rusqlite::Result<T> {
    T::from_str(s).map_err(|msg| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, msg)),
        )
    })
}

fn category_from_row(row: &Row) -> rusqlite::Result<Category> {
    Ok(Category {
        id: row.get(0)?,
        name: row.get(1)?,
        note: row.get(2)?,
        target_count: row.get(3)?,
        sort_order: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn item_from_row(row: &Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        category_id: row.get(1)?,
        url: row.get(2)?,
        title: row.get(3)?,
        store: row.get(4)?,
        price_cents: row.get(5)?,
        image_url: row.get(6)?,
        status: parse_col(&row.get::<_, String>(7)?, 7)?,
        size: row.get(8)?,
        notes: row.get(9)?,
        sort_order: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// Ordered schema migrations. Append-only — never edit a past entry; add a new
/// file and a new line here. The DB's `user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/001_init.sql"),
    include_str!("../../migrations/002_seed_categories.sql"),
];

/// Apply every migration whose index is at or beyond the DB's recorded
/// `user_version`, bumping it after each. A new DB starts at version 0 and gets
/// them all; an existing DB only runs the ones it hasn't seen. Each migration
/// and its version bump commit together, so a crash mid-migration can never
/// leave a half-applied schema.
fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        if (i as i64) < version {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        // PRAGMA values can't be bound; `i + 1` is a controlled integer.
        // `user_version` is part of the DB header and is transactional, so it
        // rolls back with the rest if the commit never lands.
        tx.execute_batch(&format!("PRAGMA user_version = {};", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

impl Store {
    /// Open (creating if needed) the database at `path` and apply any pending
    /// schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&mut conn)?;
        // Enforce ON DELETE CASCADE (deleting a category removes its items) for
        // normal operation. None of the migrations rebuild tables, so this can
        // stay on throughout.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    // ---- categories -------------------------------------------------------

    pub fn categories(&self) -> Result<Vec<Category>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CATEGORY_COLS} FROM categories ORDER BY sort_order ASC, id ASC"
        ))?;
        let rows = stmt
            .query_map([], category_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn load_category(conn: &Connection, id: i64) -> Result<Category> {
        conn.query_row(
            &format!("SELECT {CATEGORY_COLS} FROM categories WHERE id = ?1"),
            [id],
            category_from_row,
        )
        .optional()?
        .ok_or(Error::NotFound(id))
    }

    pub fn create_category(&self, input: NewCategory) -> Result<Category> {
        let conn = self.lock();
        // New categories sort after the current maximum so they land at the end.
        let next_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM categories",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO categories (name, note, target_count, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![input.name, input.note, input.target_count, next_order, now()],
        )?;
        Self::load_category(&conn, conn.last_insert_rowid())
    }

    pub fn update_category(&self, id: i64, input: UpdateCategory) -> Result<Category> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE categories SET name = ?1, note = ?2, target_count = ?3 WHERE id = ?4",
            params![input.name, input.note, input.target_count, id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Self::load_category(&conn, id)
    }

    pub fn delete_category(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        // Items cascade via the foreign key.
        let changed = conn.execute("DELETE FROM categories WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    // ---- items ------------------------------------------------------------

    /// Every item across every category, in display order. The web view groups
    /// them by `category_id` client-side, keeping counts derived from one source.
    pub fn items(&self) -> Result<Vec<Item>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items ORDER BY category_id ASC, sort_order ASC, id ASC"
        ))?;
        let rows = stmt
            .query_map([], item_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn load_item(conn: &Connection, id: i64) -> Result<Item> {
        conn.query_row(
            &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
            [id],
            item_from_row,
        )
        .optional()?
        .ok_or(Error::NotFound(id))
    }

    fn require_category(conn: &Connection, id: i64) -> Result<()> {
        let exists: bool = conn
            .query_row("SELECT 1 FROM categories WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(Error::BadRequest(format!("no category #{id}")))
        }
    }

    pub fn create_item(&self, input: NewItem) -> Result<Item> {
        let conn = self.lock();
        Self::require_category(&conn, input.category_id)?;
        // Append within the destination category.
        let next_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM items WHERE category_id = ?1",
            [input.category_id],
            |r| r.get(0),
        )?;
        let now = now();
        conn.execute(
            "INSERT INTO items
               (category_id, url, title, store, price_cents, image_url, status,
                size, notes, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                input.category_id,
                input.url,
                input.title,
                input.store,
                input.price_cents,
                input.image_url,
                input.status.as_str(),
                input.size,
                input.notes,
                next_order,
                now,
            ],
        )?;
        Self::load_item(&conn, conn.last_insert_rowid())
    }

    pub fn update_item(&self, id: i64, input: UpdateItem) -> Result<Item> {
        let conn = self.lock();
        Self::require_category(&conn, input.category_id)?;
        let changed = conn.execute(
            "UPDATE items SET
               category_id = ?1, url = ?2, title = ?3, store = ?4, price_cents = ?5,
               image_url = ?6, status = ?7, size = ?8, notes = ?9, updated_at = ?10
             WHERE id = ?11",
            params![
                input.category_id,
                input.url,
                input.title,
                input.store,
                input.price_cents,
                input.image_url,
                input.status.as_str(),
                input.size,
                input.notes,
                now(),
                id,
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Self::load_item(&conn, id)
    }

    /// Quick status change (the common table-row action) without rewriting the
    /// whole item.
    pub fn set_item_status(&self, id: i64, status: Status) -> Result<Item> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE items SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now(), id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Self::load_item(&conn, id)
    }

    pub fn delete_item(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        let changed = conn.execute("DELETE FROM items WHERE id = ?1", [id])?;
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

    #[test]
    fn fresh_db_is_seeded_with_the_classic_wardrobe() {
        let s = store();
        let cats = s.categories().unwrap();
        assert_eq!(cats.len(), 13, "seed migration populates the standard wardrobe");
        assert_eq!(cats[0].name, "Outerwear", "ordered by sort_order");
        assert!(cats.iter().all(|c| c.target_count.is_some()));
    }

    #[test]
    fn create_update_delete_category() {
        let s = store();
        let c = s
            .create_category(NewCategory {
                name: "Hats".into(),
                note: None,
                target_count: Some(2),
            })
            .unwrap();
        assert_eq!(c.name, "Hats");
        assert_eq!(c.target_count, Some(2));

        let updated = s
            .update_category(
                c.id,
                UpdateCategory {
                    name: "Headwear".into(),
                    note: Some("caps and beanies".into()),
                    target_count: None,
                },
            )
            .unwrap();
        assert_eq!(updated.name, "Headwear");
        assert_eq!(updated.note.as_deref(), Some("caps and beanies"));
        assert_eq!(updated.target_count, None);

        s.delete_category(c.id).unwrap();
        assert!(matches!(s.delete_category(c.id), Err(Error::NotFound(_))));
    }

    fn any_category(s: &Store) -> i64 {
        s.categories().unwrap()[0].id
    }

    #[test]
    fn item_lifecycle() {
        let s = store();
        let cat = any_category(&s);
        let it = s
            .create_item(NewItem {
                category_id: cat,
                url: "https://example.com/coat".into(),
                title: Some("Wool topcoat".into()),
                store: Some("Uniqlo".into()),
                price_cents: Some(19990),
                image_url: None,
                status: Status::Wishlist,
                size: Some("M".into()),
                notes: None,
            })
            .unwrap();
        assert_eq!(it.status, Status::Wishlist);
        assert_eq!(it.price_cents, Some(19990));

        // Quick status toggle.
        let owned = s.set_item_status(it.id, Status::Owned).unwrap();
        assert_eq!(owned.status, Status::Owned);

        // Full edit clears an optional field and moves nothing.
        let edited = s
            .update_item(
                it.id,
                UpdateItem {
                    category_id: cat,
                    url: it.url.clone(),
                    title: Some("Grey wool topcoat".into()),
                    store: None,
                    price_cents: Some(17500),
                    image_url: None,
                    status: Status::Ordered,
                    size: Some("M".into()),
                    notes: Some("on sale".into()),
                },
            )
            .unwrap();
        assert_eq!(edited.title.as_deref(), Some("Grey wool topcoat"));
        assert_eq!(edited.store, None, "cleared");
        assert_eq!(edited.status, Status::Ordered);

        assert_eq!(s.items().unwrap().len(), 1);
        s.delete_item(it.id).unwrap();
        assert!(s.items().unwrap().is_empty());
    }

    #[test]
    fn item_requires_an_existing_category() {
        let s = store();
        let bad = s.create_item(NewItem {
            category_id: 9999,
            url: "https://example.com/x".into(),
            title: None,
            store: None,
            price_cents: None,
            image_url: None,
            status: Status::Wishlist,
            size: None,
            notes: None,
        });
        assert!(matches!(bad, Err(Error::BadRequest(_))));
    }

    #[test]
    fn deleting_a_category_cascades_to_its_items() {
        let s = store();
        let cat = any_category(&s);
        s.create_item(NewItem {
            category_id: cat,
            url: "https://example.com/x".into(),
            title: None,
            store: None,
            price_cents: None,
            image_url: None,
            status: Status::Wishlist,
            size: None,
            notes: None,
        })
        .unwrap();
        assert_eq!(s.items().unwrap().len(), 1);
        s.delete_category(cat).unwrap();
        assert!(s.items().unwrap().is_empty(), "items cascade with the category");
    }
}
