//! All SQLite access. The binary is the single writer, so one `Connection`
//! behind a `Mutex` is sufficient and trivially correct.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};

use fleet_common::{Error, Result};

use super::model::*;

const CATEGORY_COLS: &str = "id, name, sort_order, created_at";
const ACTIVITY_COLS: &str = "id, name, category_id, unit, sort_order, created_at";

pub struct Store {
    conn: Mutex<Connection>,
}

/// Fields for a new catalog activity.
pub struct NewActivity {
    pub name: String,
    pub category_id: i64,
    pub unit: String,
}

/// Fields for a new proposal. `activities` is the countdown in order — the
/// first is done ten times, the last once — and must hold exactly
/// [`COURSE_STEPS`] distinct existing activities; the store rejects anything
/// else.
pub struct NewProposal {
    pub title: String,
    pub author: String,
    pub activities: Vec<i64>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn category_from_row(row: &Row) -> rusqlite::Result<Category> {
    Ok(Category {
        id: row.get(0)?,
        name: row.get(1)?,
        sort_order: row.get(2)?,
        created_at: row.get(3)?,
    })
}

fn activity_from_row(row: &Row) -> rusqlite::Result<Activity> {
    Ok(Activity {
        id: row.get(0)?,
        name: row.get(1)?,
        category_id: row.get(2)?,
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
    include_str!("../../migrations/003_user_categories.sql"),
    include_str!("../../migrations/004_sum_to_ten.sql"),
    include_str!("../../migrations/005_whole_units.sql"),
    include_str!("../../migrations/006_countdown.sql"),
];

impl Store {
    /// Open (creating if needed) the database at `path` and apply any pending
    /// schema migrations. fleet-common owns the open/migrate invariants (WAL,
    /// the FK-off bracket during migration, migration fingerprinting); foreign
    /// keys are ON afterwards, enforcing ON DELETE CASCADE (deleting a
    /// proposal removes its steps and votes) and ON DELETE RESTRICT (an
    /// activity in use / a category with activities cannot be deleted).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = fleet_common::store::open_migrated(path, MIGRATIONS)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    // ---- categories --------------------------------------------------------

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

    /// Category names are the display identity, so duplicates would make the
    /// picker ambiguous; check first for a friendly error (the UNIQUE
    /// constraint is the backstop).
    fn require_unique_category_name(
        conn: &Connection,
        name: &str,
        exclude: Option<i64>,
    ) -> Result<()> {
        let taken: bool = conn
            .query_row(
                "SELECT 1 FROM categories WHERE name = ?1 AND id IS NOT ?2",
                params![name, exclude],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            Err(Error::BadRequest(format!(
                "a category named \"{name}\" already exists"
            )))
        } else {
            Ok(())
        }
    }

    pub fn create_category(&self, name: &str) -> Result<Category> {
        let conn = self.lock();
        Self::require_unique_category_name(&conn, name, None)?;
        // New categories sort after the current maximum so they land at the end.
        let next_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM categories",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO categories (name, sort_order, created_at) VALUES (?1, ?2, ?3)",
            params![name, next_order, now()],
        )?;
        Self::load_category(&conn, conn.last_insert_rowid())
    }

    pub fn rename_category(&self, id: i64, name: &str) -> Result<Category> {
        let conn = self.lock();
        Self::require_unique_category_name(&conn, name, Some(id))?;
        let changed = conn.execute(
            "UPDATE categories SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Self::load_category(&conn, id)
    }

    pub fn delete_category(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        // The activities FK is RESTRICT; check first so the caller gets a
        // message that names the rule instead of a raw constraint failure.
        let in_use: bool = conn
            .query_row(
                "SELECT 1 FROM activities WHERE category_id = ?1 LIMIT 1",
                [id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if in_use {
            return Err(Error::BadRequest(format!(
                "category #{id} still has activities; delete or move them first"
            )));
        }
        let changed = conn.execute("DELETE FROM categories WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound(id));
        }
        Ok(())
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
        Self::require_category(&conn, input.category_id)?;
        // New activities sort after the current maximum so they land at the end.
        let next_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM activities",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO activities (name, category_id, unit, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![input.name, input.category_id, input.unit, next_order, now()],
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
        if input.activities.len() != COURSE_STEPS {
            return Err(Error::BadRequest(format!(
                "a course is a countdown over exactly {COURSE_STEPS} activities; got {}",
                input.activities.len()
            )));
        }
        let distinct: HashSet<i64> = input.activities.iter().copied().collect();
        if distinct.len() != COURSE_STEPS {
            return Err(Error::BadRequest(
                "each countdown step must be a different activity".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO proposals (title, author, created_at) VALUES (?1, ?2, ?3)",
            params![input.title, input.author, now()],
        )?;
        let id = tx.last_insert_rowid();
        for (i, activity_id) in input.activities.iter().enumerate() {
            Self::require_activity(&tx, *activity_id)?;
            tx.execute(
                "INSERT INTO steps (proposal_id, position, activity_id) VALUES (?1, ?2, ?3)",
                params![id, (i + 1) as i64, activity_id],
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
                        a.name, c.name, a.unit
                 FROM steps s
                 JOIN activities a ON a.id = s.activity_id
                 JOIN categories c ON c.id = a.category_id
                 WHERE ?1 IS NULL OR s.proposal_id = ?1
                 ORDER BY s.proposal_id ASC, s.position ASC",
            )?;
            let rows = stmt.query_map([only], |row| {
                let proposal_id: i64 = row.get(0)?;
                let position: i64 = row.get(1)?;
                let step = Step {
                    position,
                    activity_id: row.get(2)?,
                    activity: row.get(3)?,
                    category: row.get(4)?,
                    unit: row.get(5)?,
                    quantity: quantity_for(position),
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

    /// The first `n` seeded activity ids, in catalog order.
    fn first_activities(s: &Store, n: usize) -> Vec<i64> {
        s.activities().unwrap()[..n].iter().map(|a| a.id).collect()
    }

    /// A valid course: a countdown over [`COURSE_STEPS`] distinct activities.
    fn propose(s: &Store, title: &str) -> Proposal {
        s.create_proposal(NewProposal {
            title: title.into(),
            author: "cap".into(),
            activities: first_activities(s, COURSE_STEPS),
        })
        .unwrap()
    }

    #[test]
    fn fresh_db_is_seeded_with_the_starting_catalog() {
        let s = store();
        let cats = s.categories().unwrap();
        assert_eq!(cats.len(), 10, "seed migrations populate the categories");
        assert_eq!(cats[0].name, "Foods", "ordered by sort_order");

        let acts = s.activities().unwrap();
        assert_eq!(acts.len(), 45, "original ten plus the quantifiable list");
        assert_eq!(acts[0].name, "Donuts eaten");
        let chance = cats.iter().find(|c| c.name == "Chance").unwrap();
        assert!(acts.iter().any(|a| a.category_id == chance.id));
    }

    #[test]
    fn category_lifecycle_and_name_uniqueness() {
        let s = store();
        let c = s.create_category("Karaoke marathon").unwrap();
        assert!(c.sort_order > 100, "new categories append after the seeds");

        assert!(matches!(
            s.create_category("Karaoke marathon"),
            Err(Error::BadRequest(_))
        ));

        let renamed = s.rename_category(c.id, "Marathon karaoke").unwrap();
        assert_eq!(renamed.name, "Marathon karaoke");
        assert!(matches!(
            s.rename_category(c.id, "Foods"),
            Err(Error::BadRequest(_)),
        ));

        s.delete_category(c.id).unwrap();
        assert!(matches!(s.delete_category(c.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn a_category_with_activities_cannot_be_deleted() {
        let s = store();
        let c = s.create_category("Cartwheeling").unwrap();
        let a = s
            .create_activity(NewActivity {
                name: "Cartwheels".into(),
                category_id: c.id,
                unit: "cartwheels".into(),
            })
            .unwrap();
        assert!(matches!(s.delete_category(c.id), Err(Error::BadRequest(_))));
        s.delete_activity(a.id).unwrap();
        s.delete_category(c.id).unwrap();
    }

    #[test]
    fn an_activity_requires_an_existing_category() {
        let s = store();
        let bad = s.create_activity(NewActivity {
            name: "Ghost".into(),
            category_id: 9999,
            unit: "ghosts".into(),
        });
        assert!(matches!(bad, Err(Error::BadRequest(_))));
    }

    #[test]
    fn a_course_is_a_countdown_over_ten_distinct_activities() {
        let s = store();
        let nine = s.create_proposal(NewProposal {
            title: "too short".into(),
            author: "cap".into(),
            activities: first_activities(&s, 9),
        });
        assert!(matches!(nine, Err(Error::BadRequest(_))));

        let eleven = s.create_proposal(NewProposal {
            title: "too long".into(),
            author: "cap".into(),
            activities: first_activities(&s, 11),
        });
        assert!(matches!(eleven, Err(Error::BadRequest(_))));

        let mut repeated = first_activities(&s, COURSE_STEPS);
        repeated[9] = repeated[0];
        let dup = s.create_proposal(NewProposal {
            title: "double donuts".into(),
            author: "cap".into(),
            activities: repeated,
        });
        assert!(matches!(dup, Err(Error::BadRequest(_))));

        let p = propose(&s, "the gauntlet");
        assert_eq!(p.steps.len(), COURSE_STEPS);
        assert_eq!(
            p.steps.iter().map(|st| st.position).collect::<Vec<_>>(),
            (1..=COURSE_STEPS as i64).collect::<Vec<_>>(),
            "positions are 1..=10 in list order"
        );
        assert_eq!(
            p.steps.iter().map(|st| st.quantity).collect::<Vec<_>>(),
            (1..=COURSE_STEPS as i64).rev().collect::<Vec<_>>(),
            "the countdown: 10 of the first, 1 of the last"
        );
        assert!(
            p.steps.iter().all(|st| !st.category.is_empty()),
            "steps carry their category's display name"
        );
    }

    #[test]
    fn every_countdown_activity_must_be_real() {
        let s = store();
        let mut activities = first_activities(&s, COURSE_STEPS);
        activities[3] = 9999;
        let err = s.create_proposal(NewProposal {
            title: "ghost activity".into(),
            author: "cap".into(),
            activities,
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
