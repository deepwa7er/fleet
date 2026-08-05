//! The snapshot store.
//!
//! Single writer (the sync loop) plus reads from the HTTP handlers, so one
//! connection behind a `Mutex` is the right shape — the fleet's standard, and
//! the data here is a board, not a warehouse.
//!
//! Every type in this module is already public-safe: it was narrowed at the
//! API boundary ([`crate::fizzy`]) and sanitized at ingest
//! ([`crate::sanitize`]). The renderer is therefore free to print any field it
//! holds, which is the point — a renderer that has to remember what not to
//! show will eventually forget.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use fleet_common::http::{Error, Result};
use fleet_common::store::open_migrated;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::schema::MIGRATIONS;

/// Where a card sits on the board. Fizzy's four card sets are disjoint: a
/// column serves `cards.active`, which excludes both closed and postponed
/// cards, and triage is by definition the cards not yet in a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// Fizzy calls it the "stream": cards awaiting triage.
    Triage,
    Column,
    NotNow,
    Closed,
}

impl SectionKind {
    fn as_str(self) -> &'static str {
        match self {
            SectionKind::Triage => "triage",
            SectionKind::Column => "column",
            SectionKind::NotNow => "not_now",
            SectionKind::Closed => "closed",
        }
    }

    fn parse(value: &str) -> SectionKind {
        match value {
            "triage" => SectionKind::Triage,
            "not_now" => SectionKind::NotNow,
            "closed" => SectionKind::Closed,
            // "column", and anything a future schema adds without teaching
            // this function about it: a named section is the safe reading.
            _ => SectionKind::Column,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    /// `/a/<hash>.<ext>`, or `None` when the avatar could not be cached — for
    /// example the SVG initials Fizzy generates for a user with no photo.
    pub avatar: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Card {
    pub number: i64,
    pub title: String,
    pub description_html: String,
    pub image_path: Option<String>,
    pub tags: Vec<String>,
    pub golden: bool,
    pub created_at: String,
    pub last_active_at: String,
    pub creator: Person,
    pub assignees: Vec<Person>,
    pub more_assignees: bool,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub kind: SectionKind,
    pub name: String,
    pub cards: Vec<Card>,
}

#[derive(Debug, Clone)]
pub struct Board {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description_html: String,
    pub sections: Vec<Section>,
}

impl Board {
    pub fn card_count(&self) -> usize {
        self.sections.iter().map(|s| s.cards.len()).sum()
    }

    pub fn card(&self, number: i64) -> Option<(&Card, SectionKind)> {
        self.sections.iter().find_map(|s| {
            s.cards
                .iter()
                .find(|c| c.number == number)
                .map(|c| (c, s.kind))
        })
    }
}

/// How the last sync went, for the page footer.
#[derive(Debug, Clone, Default)]
pub struct SyncState {
    pub last_success_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_error: Option<String>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = open_migrated(path, MIGRATIONS)?;
        // The mutex above serializes writers *inside* one process, but mirror
        // has a second one: `mirror sync` run by hand against the same file
        // while the service's own loop is mid-pass. rusqlite defaults to a
        // busy timeout of zero, so the two colliding would fail outright with
        // "database is locked" rather than waiting out a transaction that
        // takes milliseconds.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Internal("the store mutex was poisoned".into()))
    }

    /// Replace the entire snapshot in one transaction.
    ///
    /// Wholesale replacement rather than a diff, for two reasons. A reader
    /// either sees the previous board or the next one and never a half-applied
    /// mixture; and a board that has been unpublished, or a card that has been
    /// deleted, disappears by simply not being in the new snapshot — no
    /// separate reconciliation pass that could be forgotten or get it wrong.
    pub fn replace(&self, boards: &[Board]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM cards", [])?;
        tx.execute("DELETE FROM sections", [])?;
        tx.execute("DELETE FROM boards", [])?;

        for (board_position, board) in boards.iter().enumerate() {
            tx.execute(
                "INSERT INTO boards (id, slug, name, description_html, position)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    board.id,
                    board.slug,
                    board.name,
                    board.description_html,
                    board_position as i64
                ],
            )?;
            for (section_position, section) in board.sections.iter().enumerate() {
                tx.execute(
                    "INSERT INTO sections (board_id, kind, name, position)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        board.id,
                        section.kind.as_str(),
                        section.name,
                        section_position as i64
                    ],
                )?;
                let section_id = tx.last_insert_rowid();
                for (card_position, card) in section.cards.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO cards (
                             board_id, section_id, number, title, description_html,
                             image_path, tags, golden, created_at, last_active_at,
                             creator_name, creator_avatar, assignees, more_assignees, position
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                        params![
                            board.id,
                            section_id,
                            card.number,
                            card.title,
                            card.description_html,
                            card.image_path,
                            serde_json::to_string(&card.tags).unwrap_or_else(|_| "[]".into()),
                            card.golden as i64,
                            card.created_at,
                            card.last_active_at,
                            card.creator.name,
                            card.creator.avatar,
                            serde_json::to_string(&card.assignees).unwrap_or_else(|_| "[]".into()),
                            card.more_assignees as i64,
                            card_position as i64
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop every mirrored board. The operator's kill switch: `mirror purge`
    /// empties the public page without waiting for a sync to notice that a
    /// board was unpublished — which matters when the laptop that holds Fizzy
    /// is asleep and cannot be asked.
    pub fn purge(&self) -> Result<()> {
        self.replace(&[])
    }

    pub fn boards(&self) -> Result<Vec<Board>> {
        let conn = self.lock()?;
        let mut statement =
            conn.prepare("SELECT id, slug, name, description_html FROM boards ORDER BY position")?;
        let rows = statement
            .query_map([], |row| {
                Ok(Board {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    name: row.get(2)?,
                    description_html: row.get(3)?,
                    sections: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        rows.into_iter()
            .map(|mut board| {
                board.sections = sections_of(&conn, &board.id)?;
                Ok(board)
            })
            .collect()
    }

    pub fn board(&self, slug: &str) -> Result<Option<Board>> {
        let conn = self.lock()?;
        let found = conn
            .query_row(
                "SELECT id, slug, name, description_html FROM boards WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(Board {
                        id: row.get(0)?,
                        slug: row.get(1)?,
                        name: row.get(2)?,
                        description_html: row.get(3)?,
                        sections: Vec::new(),
                    })
                },
            )
            .optional()?;
        match found {
            Some(mut board) => {
                board.sections = sections_of(&conn, &board.id)?;
                Ok(Some(board))
            }
            None => Ok(None),
        }
    }

    /// Every asset path the snapshot references, as bare file names — the
    /// keep-list for [`crate::assets::Cache::retain`].
    pub fn referenced_assets(&self) -> Result<HashSet<String>> {
        let conn = self.lock()?;
        let mut keys = HashSet::new();
        let mut statement =
            conn.prepare("SELECT image_path, creator_avatar, assignees FROM cards")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for path in [
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ]
            .into_iter()
            .flatten()
            {
                keys.insert(file_name(&path));
            }
            let assignees: Vec<Person> =
                serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default();
            for assignee in assignees {
                if let Some(avatar) = assignee.avatar {
                    keys.insert(file_name(&avatar));
                }
            }
        }
        Ok(keys)
    }

    pub fn sync_state(&self) -> Result<SyncState> {
        let conn = self.lock()?;
        Ok(conn.query_row(
            "SELECT last_success_at, last_attempt_at, last_error FROM sync_state WHERE id = 1",
            [],
            |row| {
                Ok(SyncState {
                    last_success_at: row.get(0)?,
                    last_attempt_at: row.get(1)?,
                    last_error: row.get(2)?,
                })
            },
        )?)
    }

    pub fn record_success(&self, at: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sync_state
                SET last_success_at = ?1, last_attempt_at = ?1, last_error = NULL
              WHERE id = 1",
            params![at],
        )?;
        Ok(())
    }

    pub fn record_failure(&self, at: &str, error: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sync_state SET last_attempt_at = ?1, last_error = ?2 WHERE id = 1",
            params![at, error],
        )?;
        Ok(())
    }
}

fn sections_of(conn: &Connection, board_id: &str) -> Result<Vec<Section>> {
    let mut statement =
        conn.prepare("SELECT id, kind, name FROM sections WHERE board_id = ?1 ORDER BY position")?;
    let sections = statement
        .query_map(params![board_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                SectionKind::parse(&row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);

    sections
        .into_iter()
        .map(|(id, kind, name)| {
            Ok(Section {
                kind,
                name,
                cards: cards_of(conn, id)?,
            })
        })
        .collect()
}

fn cards_of(conn: &Connection, section_id: i64) -> Result<Vec<Card>> {
    let mut statement = conn.prepare(
        "SELECT number, title, description_html, image_path, tags, golden,
                created_at, last_active_at, creator_name, creator_avatar,
                assignees, more_assignees
           FROM cards WHERE section_id = ?1 ORDER BY position",
    )?;
    let cards = statement
        .query_map(params![section_id], |row| {
            Ok(Card {
                number: row.get(0)?,
                title: row.get(1)?,
                description_html: row.get(2)?,
                image_path: row.get(3)?,
                tags: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                golden: row.get::<_, i64>(5)? != 0,
                created_at: row.get(6)?,
                last_active_at: row.get(7)?,
                creator: Person {
                    name: row.get(8)?,
                    avatar: row.get(9)?,
                },
                assignees: serde_json::from_str(&row.get::<_, String>(10)?).unwrap_or_default(),
                more_assignees: row.get::<_, i64>(11)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(cards)
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str) -> Person {
        Person {
            name: name.into(),
            avatar: Some("/a/aa.png".into()),
        }
    }

    fn card(number: i64) -> Card {
        Card {
            number,
            title: format!("card {number}"),
            description_html: "<p>body</p>".into(),
            image_path: Some("/a/bb.jpg".into()),
            tags: vec!["tag".into()],
            golden: number == 1,
            created_at: "2026-08-05T00:00:00Z".into(),
            last_active_at: "2026-08-05T01:00:00Z".into(),
            creator: person("creator"),
            assignees: vec![person("assignee")],
            more_assignees: false,
        }
    }

    fn board() -> Board {
        Board {
            id: "b1".into(),
            slug: "playground".into(),
            name: "Playground".into(),
            description_html: "<p>hello</p>".into(),
            sections: vec![
                Section {
                    kind: SectionKind::Triage,
                    name: "Triage".into(),
                    cards: vec![card(1), card(2)],
                },
                Section {
                    kind: SectionKind::Closed,
                    name: "Closed".into(),
                    cards: vec![card(3)],
                },
            ],
        }
    }

    fn store() -> Store {
        let path = std::env::temp_dir().join(format!(
            "mirror-store-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        Store::open(&path).unwrap()
    }

    #[test]
    fn round_trips_a_snapshot() {
        let store = store();
        store.replace(&[board()]).unwrap();

        let read = store.board("playground").unwrap().unwrap();
        assert_eq!(read.name, "Playground");
        assert_eq!(read.card_count(), 3);
        assert_eq!(read.sections[0].kind, SectionKind::Triage);
        assert_eq!(read.sections[1].kind, SectionKind::Closed);
        assert_eq!(read.sections[0].cards[0].tags, vec!["tag".to_string()]);
        assert!(read.sections[0].cards[0].golden);
        assert_eq!(read.sections[0].cards[0].assignees[0].name, "assignee");

        let (found, kind) = read.card(3).unwrap();
        assert_eq!(found.title, "card 3");
        assert_eq!(kind, SectionKind::Closed);
        assert!(read.card(99).is_none());
    }

    #[test]
    fn replacing_removes_what_is_gone() {
        let store = store();
        store.replace(&[board()]).unwrap();
        store.replace(&[]).unwrap();
        assert!(store.board("playground").unwrap().is_none());
        assert!(store.boards().unwrap().is_empty());
        assert!(store.referenced_assets().unwrap().is_empty());
    }

    #[test]
    fn reports_referenced_assets_by_file_name() {
        let store = store();
        store.replace(&[board()]).unwrap();
        let assets = store.referenced_assets().unwrap();
        assert_eq!(
            assets,
            HashSet::from(["aa.png".to_string(), "bb.jpg".to_string()])
        );
    }

    #[test]
    fn tracks_sync_outcomes() {
        let store = store();
        assert!(store.sync_state().unwrap().last_success_at.is_none());
        store
            .record_failure("2026-08-05T00:00:00Z", "laptop asleep")
            .unwrap();
        let state = store.sync_state().unwrap();
        assert_eq!(state.last_error.as_deref(), Some("laptop asleep"));
        assert!(state.last_success_at.is_none());

        store.record_success("2026-08-05T00:05:00Z").unwrap();
        let state = store.sync_state().unwrap();
        assert_eq!(
            state.last_success_at.as_deref(),
            Some("2026-08-05T00:05:00Z")
        );
        assert!(
            state.last_error.is_none(),
            "a success clears the last error"
        );
    }
}
