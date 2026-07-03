//! All SQLite access. The binary is the single writer, so one `Connection`
//! behind a `Mutex` is sufficient and trivially correct.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row};

use fleet_common::{Error, Result};

use super::model::*;

const ACTIVITY_COLS: &str = "id, name, category, unit, sort_order, created_at";

pub struct Store {
    conn: Mutex<Connection>,
}

/// Fields for a new catalog activity.
pub struct NewActivity {
    pub name: String,
    pub category: Category,
    pub unit: String,
}

/// One step of a new proposal; its position is its index in the list.
pub struct NewStep {
    pub activity_id: i64,
    pub quantity: f64,
}

/// Fields for a new proposal. `steps` must hold exactly [`SEQUENCE_LEN`]
/// entries with positive quantities — the store rejects anything else.
pub struct NewProposal {
    pub title: String,
    pub author: String,
    pub steps: Vec<NewStep>,
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

fn activity_from_row(row: &Row) -> rusqlite::Result<Activity> {
    Ok(Activity {
        id: row.get(0)?,
        name: row.get(1)?,
        category: parse_col(&row.get::<_, String>(2)?, 2)?,
        unit: row.get(3)?,
        sort_order: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// Ordered schema migrations. Append-only — never edit a past entry; add a new
/// file and a new line here. The DB's `user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/001_init.sql"),
    include_str!("../../migrations/002_seed_activities.sql"),
];

impl Store {
    /// Open (creating if needed) the database at `path` and apply any pending
    /// schema migrations. fleet-common owns the open/migrate invariants (WAL,
    /// the FK-off bracket during migration, migration fingerprinting); foreign
    /// keys are ON afterwards, enforcing ON DELETE CASCADE (deleting a
    /// proposal removes its steps and votes) and ON DELETE RESTRICT (an
    /// activity in use cannot be deleted).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = fleet_common::store::open_migrated(path, MIGRATIONS)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    // ---- activities --------------------------------------------------------

    pub fn activities(&self) -> Result<Vec<Activity>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACTIVITY_COLS} FROM activities ORDER BY sort_order ASC, id ASC"
        ))?;
        let rows = stmt
            .query_map([], activity_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn load_activity(conn: &Connection, id: i64) -> Result<Activity> {
        conn.query_row(
            &format!("SELECT {ACTIVITY_COLS} FROM activities WHERE id = ?1"),
            [id],
            activity_from_row,
        )
        .optional()?
        .ok_or(Error::NotFound(id))
    }

    pub fn create_activity(&self, input: NewActivity) -> Result<Activity> {
        let conn = self.lock();
        // New activities sort after the current maximum so they land at the end.
        let next_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM activities",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO activities (name, category, unit, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                input.name,
                input.category.as_str(),
                input.unit,
                next_order,
                now()
            ],
        )?;
        Self::load_activity(&conn, conn.last_insert_rowid())
    }

    pub fn delete_activity(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        // The steps FK is RESTRICT; check first so the caller gets a message
        // that names the rule instead of a raw constraint failure.
        let in_use: bool = conn
            .query_row(
                "SELECT 1 FROM steps WHERE activity_id = ?1 LIMIT 1",
                [id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if in_use {
            return Err(Error::BadRequest(format!(
                "activity #{id} is used by a proposal; delete those proposals first"
            )));
        }
        let changed = conn.execute("DELETE FROM activities WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    // ---- proposals ---------------------------------------------------------

    /// Every proposal with steps and tally, ranked by votes (ties keep the
    /// earlier proposal ahead). `voted` reflects `voter` when given.
    pub fn proposals(&self, voter: Option<&str>) -> Result<Vec<Proposal>> {
        let conn = self.lock();
        Self::proposal_views(&conn, voter, None)
    }

    pub fn create_proposal(&self, input: NewProposal) -> Result<Proposal> {
        if input.steps.len() != SEQUENCE_LEN {
            return Err(Error::BadRequest(format!(
                "a proposal is exactly {SEQUENCE_LEN} steps; got {}",
                input.steps.len()
            )));
        }
        for (i, step) in input.steps.iter().enumerate() {
            if !(step.quantity.is_finite() && step.quantity > 0.0) {
                return Err(Error::BadRequest(format!(
                    "step {} quantity must be a positive number",
                    i + 1
                )));
            }
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO proposals (title, author, created_at) VALUES (?1, ?2, ?3)",
            params![input.title, input.author, now()],
        )?;
        let id = tx.last_insert_rowid();
        for (i, step) in input.steps.iter().enumerate() {
            Self::require_activity(&tx, step.activity_id)?;
            tx.execute(
                "INSERT INTO steps (proposal_id, position, activity_id, quantity)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, (i + 1) as i64, step.activity_id, step.quantity],
            )?;
        }
        tx.commit()?;
        Self::load_proposal(&conn, id, None)
    }

    pub fn delete_proposal(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        // Steps and votes cascade via the foreign keys.
        let changed = conn.execute("DELETE FROM proposals WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    // ---- votes -------------------------------------------------------------

    /// Cast `voter`'s vote on a proposal. Idempotent — voting is set
    /// membership, so a double-click can't double-count.
    pub fn cast_vote(&self, proposal_id: i64, voter: &str) -> Result<Proposal> {
        let conn = self.lock();
        Self::require_proposal(&conn, proposal_id)?;
        conn.execute(
            "INSERT INTO votes (proposal_id, voter, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (proposal_id, voter) DO NOTHING",
            params![proposal_id, voter, now()],
        )?;
        Self::load_proposal(&conn, proposal_id, Some(voter))
    }

    /// Retract `voter`'s vote. Idempotent for the same reason casting is.
    pub fn retract_vote(&self, proposal_id: i64, voter: &str) -> Result<Proposal> {
        let conn = self.lock();
        Self::require_proposal(&conn, proposal_id)?;
        conn.execute(
            "DELETE FROM votes WHERE proposal_id = ?1 AND voter = ?2",
            params![proposal_id, voter],
        )?;
        Self::load_proposal(&conn, proposal_id, Some(voter))
    }

    // ---- shared loaders ----------------------------------------------------

    fn require_activity(conn: &Connection, id: i64) -> Result<()> {
        let exists: bool = conn
            .query_row("SELECT 1 FROM activities WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(Error::BadRequest(format!("no activity #{id}")))
        }
    }

    fn require_proposal(conn: &Connection, id: i64) -> Result<()> {
        let exists: bool = conn
            .query_row("SELECT 1 FROM proposals WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(Error::NotFound(id))
        }
    }

    fn load_proposal(conn: &Connection, id: i64, voter: Option<&str>) -> Result<Proposal> {
        Self::proposal_views(conn, voter, Some(id))?
            .into_iter()
            .next()
            .ok_or(Error::NotFound(id))
    }

    /// Assemble proposal views (tally + ordered steps + the voter's own mark),
    /// for the whole board or a single proposal.
    fn proposal_views(
        conn: &Connection,
        voter: Option<&str>,
        only: Option<i64>,
    ) -> Result<Vec<Proposal>> {
        let mut steps_by_proposal: HashMap<i64, Vec<Step>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT s.proposal_id, s.position, s.activity_id,
                        a.name, a.category, a.unit, s.quantity
                 FROM steps s JOIN activities a ON a.id = s.activity_id
                 WHERE ?1 IS NULL OR s.proposal_id = ?1
                 ORDER BY s.proposal_id ASC, s.position ASC",
            )?;
            let rows = stmt.query_map([only], |row| {
                let proposal_id: i64 = row.get(0)?;
                let step = Step {
                    position: row.get(1)?,
                    activity_id: row.get(2)?,
                    activity: row.get(3)?,
                    category: parse_col(&row.get::<_, String>(4)?, 4)?,
                    unit: row.get(5)?,
                    quantity: row.get(6)?,
                };
                Ok((proposal_id, step))
            })?;
            for row in rows {
                let (proposal_id, step) = row?;
                steps_by_proposal.entry(proposal_id).or_default().push(step);
            }
        }

        let own_votes: HashSet<i64> = match voter {
            Some(voter) => {
                let mut stmt = conn.prepare("SELECT proposal_id FROM votes WHERE voter = ?1")?;
                let ids = stmt
                    .query_map([voter], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                ids
            }
            None => HashSet::new(),
        };

        let mut stmt = conn.prepare(
            "SELECT p.id, p.title, p.author, p.created_at, COUNT(v.voter) AS votes
             FROM proposals p LEFT JOIN votes v ON v.proposal_id = p.id
             WHERE ?1 IS NULL OR p.id = ?1
             GROUP BY p.id
             ORDER BY votes DESC, p.id ASC",
        )?;
        let proposals = stmt
            .query_map([only], |row| {
                let id: i64 = row.get(0)?;
                Ok(Proposal {
                    id,
                    title: row.get(1)?,
                    author: row.get(2)?,
                    created_at: row.get(3)?,
                    votes: row.get(4)?,
                    voted: own_votes.contains(&id),
                    steps: steps_by_proposal.remove(&id).unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(proposals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open(":memory:").expect("open in-memory store")
    }

    /// A valid ten-step sequence over the seeded catalog.
    fn ten_steps(s: &Store) -> Vec<NewStep> {
        let activities = s.activities().unwrap();
        (0..SEQUENCE_LEN)
            .map(|i| NewStep {
                activity_id: activities[i % activities.len()].id,
                quantity: (i + 1) as f64,
            })
            .collect()
    }

    fn propose(s: &Store, title: &str) -> Proposal {
        s.create_proposal(NewProposal {
            title: title.into(),
            author: "cap".into(),
            steps: ten_steps(s),
        })
        .unwrap()
    }

    #[test]
    fn fresh_db_is_seeded_with_the_starting_catalog() {
        let s = store();
        let acts = s.activities().unwrap();
        assert_eq!(acts.len(), 10, "seed migration populates the catalog");
        assert_eq!(acts[0].name, "Donuts eaten", "ordered by sort_order");
        assert!(acts.iter().any(|a| a.category == Category::VideoGames));
    }

    #[test]
    fn activity_create_and_delete() {
        let s = store();
        let a = s
            .create_activity(NewActivity {
                name: "Cartwheels".into(),
                category: Category::Physical,
                unit: "cartwheels".into(),
            })
            .unwrap();
        assert_eq!(a.category, Category::Physical);
        assert!(
            a.sort_order > 100,
            "new activities append after the seed rows"
        );

        s.delete_activity(a.id).unwrap();
        assert!(matches!(s.delete_activity(a.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn a_proposal_is_exactly_ten_steps() {
        let s = store();
        let mut nine = ten_steps(&s);
        nine.pop();
        let err = s.create_proposal(NewProposal {
            title: "too short".into(),
            author: "cap".into(),
            steps: nine,
        });
        assert!(matches!(err, Err(Error::BadRequest(_))));

        let p = propose(&s, "the gauntlet");
        assert_eq!(p.steps.len(), SEQUENCE_LEN);
        assert_eq!(
            p.steps.iter().map(|st| st.position).collect::<Vec<_>>(),
            (1..=SEQUENCE_LEN as i64).collect::<Vec<_>>(),
            "positions are 1..=10 in list order"
        );
    }

    #[test]
    fn step_quantities_must_be_positive_and_activities_real() {
        let s = store();
        let mut steps = ten_steps(&s);
        steps[3].quantity = 0.0;
        let err = s.create_proposal(NewProposal {
            title: "zero".into(),
            author: "cap".into(),
            steps,
        });
        assert!(matches!(err, Err(Error::BadRequest(_))));

        let mut steps = ten_steps(&s);
        steps[0].activity_id = 9999;
        let err = s.create_proposal(NewProposal {
            title: "ghost activity".into(),
            author: "cap".into(),
            steps,
        });
        assert!(matches!(err, Err(Error::BadRequest(_))));
        assert!(
            s.proposals(None).unwrap().is_empty(),
            "a failed create leaves nothing behind (transactional)"
        );
    }

    #[test]
    fn voting_is_idempotent_set_membership() {
        let s = store();
        let p = propose(&s, "the gauntlet");

        let after = s.cast_vote(p.id, "ada").unwrap();
        assert_eq!(after.votes, 1);
        assert!(after.voted);

        let again = s.cast_vote(p.id, "ada").unwrap();
        assert_eq!(again.votes, 1, "double-cast does not double-count");

        let other = s.cast_vote(p.id, "grace").unwrap();
        assert_eq!(other.votes, 2);

        let retracted = s.retract_vote(p.id, "ada").unwrap();
        assert_eq!(retracted.votes, 1);
        assert!(!retracted.voted);
        let again = s.retract_vote(p.id, "ada").unwrap();
        assert_eq!(again.votes, 1, "re-retract is a no-op");

        assert!(matches!(s.cast_vote(9999, "ada"), Err(Error::NotFound(_))));
    }

    #[test]
    fn the_board_ranks_by_votes_with_earlier_proposals_winning_ties() {
        let s = store();
        let first = propose(&s, "first");
        let second = propose(&s, "second");
        let third = propose(&s, "third");

        s.cast_vote(third.id, "ada").unwrap();
        s.cast_vote(third.id, "grace").unwrap();
        s.cast_vote(second.id, "ada").unwrap();
        s.cast_vote(first.id, "grace").unwrap();

        let board = s.proposals(Some("ada")).unwrap();
        assert_eq!(
            board.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![third.id, first.id, second.id],
            "votes desc; first-vs-second tie keeps the earlier proposal ahead"
        );
        assert_eq!(
            board.iter().map(|p| p.voted).collect::<Vec<_>>(),
            vec![true, false, true]
        );
    }

    #[test]
    fn deleting_a_proposal_cascades_and_frees_its_activities() {
        let s = store();
        let p = propose(&s, "the gauntlet");
        s.cast_vote(p.id, "ada").unwrap();

        let used = p.steps[0].activity_id;
        assert!(
            matches!(s.delete_activity(used), Err(Error::BadRequest(_))),
            "an activity in use cannot be deleted"
        );

        s.delete_proposal(p.id).unwrap();
        assert!(s.proposals(None).unwrap().is_empty());
        s.delete_activity(used).unwrap();
    }
}
