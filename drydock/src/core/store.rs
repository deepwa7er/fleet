//! All SQLite access. The binary is the single writer, so one `Connection`
//! behind a `Mutex` is sufficient and trivially correct. Every state change
//! runs inside a transaction and is guarded by `core::state` transitions.

use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::model::*;
use super::state::{self, Transition};
use crate::error::{Error, Result};

const TICKET_COLS: &str = "id, title, type, target, state, priority, goal, \
                           acceptance, constraints, branch, pr_url, created_at, updated_at";

pub struct Store {
    conn: Mutex<Connection>,
}

/// Fields supplied by a human creating a ticket.
pub struct NewTicket {
    pub title: String,
    pub ticket_type: TicketType,
    pub target: String,
    pub priority: Priority,
    pub goal: String,
    pub acceptance: Option<String>,
    pub constraints: Option<String>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Escape SQL `LIKE` wildcards (`%`, `_`) and the escape char itself so a search
/// query matches literally. Pair with `ESCAPE '\'` in the LIKE clause.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
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

fn ticket_from_row(row: &Row) -> rusqlite::Result<Ticket> {
    Ok(Ticket {
        id: row.get(0)?,
        title: row.get(1)?,
        ticket_type: parse_col(&row.get::<_, String>(2)?, 2)?,
        target: row.get(3)?,
        state: parse_col(&row.get::<_, String>(4)?, 4)?,
        priority: parse_col(&row.get::<_, String>(5)?, 5)?,
        goal: row.get(6)?,
        acceptance: row.get(7)?,
        constraints: row.get(8)?,
        branch: row.get(9)?,
        pr_url: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn load_ticket(conn: &Connection, id: i64) -> Result<Ticket> {
    conn.query_row(
        &format!("SELECT {TICKET_COLS} FROM tickets WHERE id = ?1"),
        [id],
        ticket_from_row,
    )
    .optional()?
    .ok_or(Error::NotFound(id))
}

/// Ordered schema migrations. Append-only — never edit a past entry; add a new
/// file and a new line here. The DB's `user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/001_init.sql"),
    include_str!("../../migrations/002_closed_state.sql"),
    include_str!("../../migrations/003_investigate_type.sql"),
    include_str!("../../migrations/004_worker_pulse.sql"),
];

impl Store {
    /// Open (creating if needed) the database at `path` and apply any pending
    /// schema migrations. fleet-common owns the open/migrate invariants — WAL
    /// mode, foreign keys OFF across the whole migration pass (the
    /// table-rebuild migrations drop and recreate `tickets`, and an enabled FK
    /// would cascade that drop into `comments`/`events`), migration
    /// fingerprinting, and FKs back ON for normal operation.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = fleet_common::store::open_migrated(path, MIGRATIONS)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    // ---- creation & reads -------------------------------------------------

    pub fn create(&self, input: NewTicket) -> Result<Ticket> {
        let mut conn = self.lock();
        let now = now();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tickets
               (title, type, target, state, priority, goal, acceptance, constraints, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                input.title,
                input.ticket_type.as_str(),
                input.target,
                input.priority.as_str(),
                input.goal,
                input.acceptance,
                input.constraints,
                now,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO events (ticket_id, from_state, to_state, actor, detail, created_at)
             VALUES (?1, NULL, 'open', 'human', 'created', ?2)",
            params![id, now],
        )?;
        let ticket = load_ticket(&tx, id)?;
        tx.commit()?;
        Ok(ticket)
    }

    /// List tickets, optionally filtered by `state` and by a free-text `query`
    /// (a case-insensitive substring of the title, goal, acceptance, or target).
    pub fn list(&self, state: Option<State>, query: Option<&str>) -> Result<Vec<Ticket>> {
        let conn = self.lock();
        // Wrap the query in LIKE wildcards with any literal `%`/`_`/`\` escaped,
        // so user text matches itself. `None`/empty disables the text filter.
        let like = query
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| format!("%{}%", escape_like(q)));
        let sql = format!(
            "SELECT {TICKET_COLS} FROM tickets
             WHERE (?1 IS NULL OR state = ?1)
               AND (?2 IS NULL
                    OR title LIKE ?2 ESCAPE '\\'
                    OR goal LIKE ?2 ESCAPE '\\'
                    OR IFNULL(acceptance, '') LIKE ?2 ESCAPE '\\'
                    OR target LIKE ?2 ESCAPE '\\')
             ORDER BY CASE priority WHEN 'high' THEN 0 WHEN 'med' THEN 1 ELSE 2 END,
                      created_at ASC, id ASC",
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![state.map(|s| s.as_str()), like], ticket_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn detail(&self, id: i64) -> Result<TicketDetail> {
        let conn = self.lock();
        let ticket = load_ticket(&conn, id)?;

        let mut cstmt = conn.prepare(
            "SELECT id, ticket_id, author, kind, body, created_at
             FROM comments WHERE ticket_id = ?1 ORDER BY id ASC",
        )?;
        let comments = cstmt
            .query_map([id], |r| {
                Ok(Comment {
                    id: r.get(0)?,
                    ticket_id: r.get(1)?,
                    author: parse_col(&r.get::<_, String>(2)?, 2)?,
                    kind: parse_col(&r.get::<_, String>(3)?, 3)?,
                    body: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut estmt = conn.prepare(
            "SELECT id, ticket_id, from_state, to_state, actor, detail, created_at
             FROM events WHERE ticket_id = ?1 ORDER BY id ASC",
        )?;
        let events = estmt
            .query_map([id], |r| {
                let from: Option<String> = r.get(2)?;
                Ok(Event {
                    id: r.get(0)?,
                    ticket_id: r.get(1)?,
                    from_state: from.as_deref().map(|s| parse_col(s, 2)).transpose()?,
                    to_state: parse_col(&r.get::<_, String>(3)?, 3)?,
                    actor: parse_col(&r.get::<_, String>(4)?, 4)?,
                    detail: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(TicketDetail {
            ticket,
            comments,
            events,
        })
    }

    /// The worker's selection: reclaim stale claims, then return the
    /// highest-priority, oldest `open` ticket (without claiming it).
    pub fn next(&self, stale_after_hours: i64) -> Result<Option<Ticket>> {
        let mut conn = self.lock();
        let now = now();
        let cutoff =
            (Utc::now() - Duration::hours(stale_after_hours)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let tx = conn.transaction()?;

        // Reclaim: any in-progress ticket not touched since the cutoff is
        // assumed to be a crashed run and returned to the pool. The state
        // strings come from the RECLAIM transition so they stay in one place.
        let reclaim = &state::RECLAIM;
        let stale: Vec<i64> = {
            let mut s = tx.prepare(
                "SELECT id FROM tickets WHERE state = ?1 AND updated_at < ?2",
            )?;
            let ids = s
                .query_map(params![reclaim.from.as_str(), &cutoff], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        for id in stale {
            tx.execute(
                "UPDATE tickets SET state = ?1, updated_at = ?2 WHERE id = ?3",
                params![reclaim.to.as_str(), now, id],
            )?;
            tx.execute(
                "INSERT INTO events (ticket_id, from_state, to_state, actor, detail, created_at)
                 VALUES (?1, ?2, ?3, 'worker', 'reclaimed stale claim', ?4)",
                params![id, reclaim.from.as_str(), reclaim.to.as_str(), now],
            )?;
        }

        let next = tx
            .query_row(
                &format!(
                    "SELECT {TICKET_COLS} FROM tickets WHERE state = 'open'
                     ORDER BY CASE priority WHEN 'high' THEN 0 WHEN 'med' THEN 1 ELSE 2 END,
                              created_at ASC, id ASC
                     LIMIT 1"
                ),
                [],
                ticket_from_row,
            )
            .optional()?;
        tx.commit()?;
        Ok(next)
    }

    // ---- state transitions -----------------------------------------------

    /// Apply a single legal transition atomically: guard the current state,
    /// update the row, append an optional comment, and record the event.
    fn transition(
        &self,
        id: i64,
        t: &Transition,
        actor: Author,
        comment: Option<(CommentKind, &str)>,
        branch: Option<&str>,
        pr_url: Option<&str>,
        detail: Option<&str>,
    ) -> Result<Ticket> {
        let mut conn = self.lock();
        let now = now();
        let tx = conn.transaction()?;

        let changed = tx.execute(
            "UPDATE tickets
               SET state = ?1,
                   updated_at = ?2,
                   branch = COALESCE(?3, branch),
                   pr_url = COALESCE(?4, pr_url)
             WHERE id = ?5 AND state = ?6",
            params![t.to.as_str(), now, branch, pr_url, id, t.from.as_str()],
        )?;

        if changed == 0 {
            // Distinguish "no such ticket" from "wrong state".
            let actual: Option<String> = tx
                .query_row("SELECT state FROM tickets WHERE id = ?1", [id], |r| r.get(0))
                .optional()?;
            return match actual {
                None => Err(Error::NotFound(id)),
                Some(s) => Err(Error::InvalidTransition {
                    id,
                    action: t.action,
                    required: t.from,
                    actual: State::from_str(&s).unwrap_or(t.from),
                }),
            };
        }

        if let Some((kind, body)) = comment {
            tx.execute(
                "INSERT INTO comments (ticket_id, author, kind, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, actor.as_str(), kind.as_str(), body, now],
            )?;
        }
        tx.execute(
            "INSERT INTO events (ticket_id, from_state, to_state, actor, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, t.from.as_str(), t.to.as_str(), actor.as_str(), detail, now],
        )?;

        let ticket = load_ticket(&tx, id)?;
        tx.commit()?;
        Ok(ticket)
    }

    // Worker actions.

    /// Claim a ticket. `branch` is set for feature work; an investigate ticket
    /// produces no branch, so it may be `None`.
    pub fn claim(&self, id: i64, branch: Option<&str>) -> Result<Ticket> {
        match self.transition(id, &state::CLAIM, Author::Worker, None, branch, None, None) {
            // A ticket already in-progress means someone else grabbed it first.
            Err(Error::InvalidTransition {
                actual: State::InProgress,
                ..
            }) => Err(Error::Conflict(id)),
            other => other,
        }
    }

    pub fn needs_input(&self, id: i64, question: &str) -> Result<Ticket> {
        self.transition(
            id,
            &state::NEEDS_INPUT,
            Author::Worker,
            Some((CommentKind::Question, question)),
            None,
            None,
            None,
        )
    }

    pub fn block(&self, id: i64, reason: &str) -> Result<Ticket> {
        self.transition(
            id,
            &state::BLOCK,
            Author::Worker,
            Some((CommentKind::Note, reason)),
            None,
            None,
            None,
        )
    }

    pub fn resolve(&self, id: i64, pr_url: &str) -> Result<Ticket> {
        let note = format!("Opened PR: {pr_url}");
        self.transition(
            id,
            &state::RESOLVE,
            Author::Worker,
            Some((CommentKind::Note, &note)),
            None,
            Some(pr_url),
            None,
        )
    }

    /// An investigate ticket's deliverable: post the findings and move to
    /// in-review for the human to read (the report-type analogue of `resolve`).
    pub fn report(&self, id: i64, findings: &str) -> Result<Ticket> {
        self.transition(
            id,
            &state::REPORT,
            Author::Worker,
            Some((CommentKind::Report, findings)),
            None,
            None,
            None,
        )
    }

    // Human actions.

    pub fn answer(&self, id: i64, body: &str) -> Result<Ticket> {
        self.transition(
            id,
            &state::ANSWER,
            Author::Human,
            Some((CommentKind::Answer, body)),
            None,
            None,
            None,
        )
    }

    pub fn unblock(&self, id: i64, note: Option<&str>) -> Result<Ticket> {
        let comment = note.filter(|s| !s.trim().is_empty()).map(|s| (CommentKind::Note, s));
        self.transition(id, &state::UNBLOCK, Author::Human, comment, None, None, None)
    }

    pub fn mark_done(&self, id: i64) -> Result<Ticket> {
        self.transition(id, &state::MARK_DONE, Author::Human, None, None, None, None)
    }

    /// Human action: abandon a ticket (won't-do). Allowed from any non-terminal
    /// state — unlike the single-source transitions, close has many valid
    /// sources — so it guards on "not already terminal" rather than one `from`.
    /// Records the originating state in the event for the audit trail.
    pub fn close(&self, id: i64, note: Option<&str>) -> Result<Ticket> {
        let mut conn = self.lock();
        let now = now();
        let tx = conn.transaction()?;

        let current: Option<String> = tx
            .query_row("SELECT state FROM tickets WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let current = match current {
            None => return Err(Error::NotFound(id)),
            Some(s) => State::from_str(&s).unwrap_or(State::Closed),
        };
        if matches!(current, State::Done | State::Closed) {
            return Err(Error::BadRequest(format!(
                "ticket #{id} is already {current}; nothing to close"
            )));
        }

        tx.execute(
            "UPDATE tickets SET state = 'closed', updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        if let Some(body) = note.filter(|s| !s.trim().is_empty()) {
            tx.execute(
                "INSERT INTO comments (ticket_id, author, kind, body, created_at)
                 VALUES (?1, 'human', 'note', ?2, ?3)",
                params![id, body, now],
            )?;
        }
        tx.execute(
            "INSERT INTO events (ticket_id, from_state, to_state, actor, detail, created_at)
             VALUES (?1, ?2, 'closed', 'human', 'closed', ?3)",
            params![id, current.as_str(), now],
        )?;

        let ticket = load_ticket(&tx, id)?;
        tx.commit()?;
        Ok(ticket)
    }

    // ---- worker liveness --------------------------------------------------

    /// Record a worker pulse (a heartbeat or an action). Best-effort liveness, so
    /// callers ignore failures rather than failing the underlying request.
    pub fn pulse(&self, ticket_id: Option<i64>, message: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO worker_pulse (ticket_id, message, created_at) VALUES (?1, ?2, ?3)",
            params![ticket_id, message, now()],
        )?;
        Ok(())
    }

    /// The worker view: the newest pulse time (liveness), the ticket currently
    /// being worked (if any), and the most recent pulses (the progress feed).
    pub fn worker_view(&self, recent: usize) -> Result<(Option<String>, Option<Ticket>, Vec<Pulse>)> {
        let conn = self.lock();

        let last_seen: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM worker_pulse",
            [],
            |r| r.get::<_, Option<String>>(0),
        )?;

        // One ticket per run, so at most one in-progress; newest wins if ever not.
        let active = conn
            .query_row(
                &format!(
                    "SELECT {TICKET_COLS} FROM tickets WHERE state = 'in-progress'
                     ORDER BY updated_at DESC LIMIT 1"
                ),
                [],
                ticket_from_row,
            )
            .optional()?;

        let mut stmt = conn.prepare(
            "SELECT id, ticket_id, message, created_at
             FROM worker_pulse ORDER BY id DESC LIMIT ?1",
        )?;
        let recent_pulses = stmt
            .query_map([recent as i64], |r| {
                Ok(Pulse {
                    id: r.get(0)?,
                    ticket_id: r.get(1)?,
                    message: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((last_seen, active, recent_pulses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Author, CommentKind, Priority, State};

    fn store() -> Store {
        Store::open(":memory:").expect("open in-memory store")
    }

    fn mk(s: &Store, title: &str, priority: Priority) -> Ticket {
        s.create(NewTicket {
            title: title.into(),
            ticket_type: TicketType::Feature,
            target: "svc".into(),
            priority,
            goal: "goal".into(),
            acceptance: None,
            constraints: None,
        })
        .expect("create")
    }

    #[test]
    fn create_starts_open_with_creation_event() {
        let s = store();
        let t = mk(&s, "a", Priority::Med);
        assert_eq!(t.state, State::Open);
        assert!(t.branch.is_none() && t.pr_url.is_none());

        let d = s.detail(t.id).unwrap();
        assert_eq!(d.events.len(), 1);
        assert_eq!(d.events[0].from_state, None);
        assert_eq!(d.events[0].to_state, State::Open);
        assert_eq!(d.events[0].actor, Author::Human);
    }

    #[test]
    fn detail_not_found() {
        let s = store();
        assert!(matches!(s.detail(999), Err(Error::NotFound(999))));
    }

    #[test]
    fn next_orders_by_priority_then_age() {
        let s = store();
        let _low = mk(&s, "low", Priority::Low);
        let med1 = mk(&s, "med1", Priority::Med);
        let _med2 = mk(&s, "med2", Priority::Med);
        let high = mk(&s, "high", Priority::High);

        // Highest priority wins regardless of creation order.
        assert_eq!(s.next(3).unwrap().unwrap().id, high.id);
        // With the high one claimed, the older of the two med tickets is next
        // (low priority loses; id tiebreaker makes age deterministic).
        s.claim(high.id, Some("b")).unwrap();
        assert_eq!(s.next(3).unwrap().unwrap().id, med1.id);
    }

    #[test]
    fn next_is_none_when_no_open_tickets() {
        let s = store();
        assert!(s.next(3).unwrap().is_none());
        let t = mk(&s, "a", Priority::Med);
        s.claim(t.id, Some("b")).unwrap();
        // Only ticket is in-progress and not stale → nothing to hand out.
        assert!(s.next(3).unwrap().is_none());
    }

    #[test]
    fn claim_is_compare_and_swap() {
        let s = store();
        let t = mk(&s, "a", Priority::Med);

        let claimed = s.claim(t.id, Some("ticket/1-a")).unwrap();
        assert_eq!(claimed.state, State::InProgress);
        assert_eq!(claimed.branch.as_deref(), Some("ticket/1-a"));

        // Second claim loses the race.
        assert!(matches!(s.claim(t.id, Some("x")), Err(Error::Conflict(_))));
        // Claiming a non-existent ticket is NotFound, not Conflict.
        assert!(matches!(s.claim(404, Some("x")), Err(Error::NotFound(404))));
    }

    #[test]
    fn happy_path_to_done() {
        let s = store();
        let t = mk(&s, "a", Priority::High);
        s.claim(t.id, Some("ticket/1-a")).unwrap();
        let resolved = s.resolve(t.id, "https://example/pr/1").unwrap();
        assert_eq!(resolved.state, State::InReview);
        assert_eq!(resolved.pr_url.as_deref(), Some("https://example/pr/1"));
        let done = s.mark_done(t.id).unwrap();
        assert_eq!(done.state, State::Done);
    }

    #[test]
    fn transitions_require_correct_source_state() {
        let s = store();
        let t = mk(&s, "a", Priority::Med);
        // Can't ask for input on an unclaimed (open) ticket.
        assert!(matches!(
            s.needs_input(t.id, "?"),
            Err(Error::InvalidTransition { .. })
        ));
        // Can't mark done before review.
        assert!(matches!(
            s.mark_done(t.id),
            Err(Error::InvalidTransition { .. })
        ));
        // Can't resolve before claiming.
        assert!(matches!(
            s.resolve(t.id, "pr"),
            Err(Error::InvalidTransition { .. })
        ));
    }

    #[test]
    fn answer_resurfaces_parked_ticket() {
        let s = store();
        let t = mk(&s, "a", Priority::Med);
        s.claim(t.id, Some("b")).unwrap();
        let parked = s.needs_input(t.id, "Global or per-route?").unwrap();
        assert_eq!(parked.state, State::NeedsInput);
        // While parked it is not handed out as the next ticket.
        assert!(s.next(3).unwrap().is_none());

        let answered = s.answer(t.id, "Per-route.").unwrap();
        assert_eq!(answered.state, State::Open);
        // Now it is selectable again.
        assert_eq!(s.next(3).unwrap().unwrap().id, t.id);

        // The thread carries both the question and the answer.
        let d = s.detail(t.id).unwrap();
        let kinds: Vec<_> = d.comments.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec![CommentKind::Question, CommentKind::Answer]);
    }

    #[test]
    fn block_then_unblock() {
        let s = store();
        let t = mk(&s, "a", Priority::Med);
        s.claim(t.id, Some("b")).unwrap();
        assert_eq!(s.block(t.id, "needs a new API").unwrap().state, State::Blocked);
        assert!(s.next(3).unwrap().is_none());
        assert_eq!(s.unblock(t.id, Some("added the API")).unwrap().state, State::Open);
    }

    #[test]
    fn close_from_any_nonterminal_then_rejects_terminal() {
        let s = store();
        // Close from blocked (the motivating case).
        let blocked = mk(&s, "junk", Priority::Low);
        s.claim(blocked.id, Some("b")).unwrap();
        s.block(blocked.id, "nonsense ticket").unwrap();
        let closed = s.close(blocked.id, Some("abandoning")).unwrap();
        assert_eq!(closed.state, State::Closed);
        // Closed tickets aren't handed out and can't be closed again.
        assert!(s.next(3).unwrap().is_none());
        assert!(matches!(s.close(blocked.id, None), Err(Error::BadRequest(_))));

        // Close straight from open, too.
        let open = mk(&s, "also junk", Priority::Low);
        assert_eq!(s.close(open.id, None).unwrap().state, State::Closed);

        // A done ticket can't be closed; a missing ticket is NotFound.
        let done = mk(&s, "real", Priority::High);
        s.claim(done.id, Some("b2")).unwrap();
        s.resolve(done.id, "pr").unwrap();
        s.mark_done(done.id).unwrap();
        assert!(matches!(s.close(done.id, None), Err(Error::BadRequest(_))));
        assert!(matches!(s.close(9999, None), Err(Error::NotFound(9999))));

        // The originating state is recorded in the audit trail.
        let d = s.detail(blocked.id).unwrap();
        let last = d.events.last().unwrap();
        assert_eq!(last.from_state, Some(State::Blocked));
        assert_eq!(last.to_state, State::Closed);
    }

    #[test]
    fn investigate_ticket_claims_without_branch_and_reports() {
        let s = store();
        let t = s
            .create(NewTicket {
                title: "look into X".into(),
                ticket_type: TicketType::Investigate,
                target: "svc".into(),
                priority: Priority::Med,
                goal: "why is X happening".into(),
                acceptance: None,
                constraints: None,
            })
            .unwrap();
        assert_eq!(t.ticket_type, TicketType::Investigate);

        // An investigate ticket claims with no branch…
        let claimed = s.claim(t.id, None).unwrap();
        assert_eq!(claimed.state, State::InProgress);
        assert!(claimed.branch.is_none());

        // …and its deliverable is a report comment, moving it to in-review.
        let reported = s.report(t.id, "Found the cause: a stale cache.").unwrap();
        assert_eq!(reported.state, State::InReview);
        let d = s.detail(t.id).unwrap();
        assert_eq!(d.comments.last().unwrap().kind, CommentKind::Report);

        // The human reads it and marks done.
        assert_eq!(s.mark_done(t.id).unwrap().state, State::Done);
    }

    #[test]
    fn worker_pulse_and_view() {
        let s = store();
        // Nothing yet.
        let (last, active, recent) = s.worker_view(10).unwrap();
        assert!(last.is_none() && active.is_none() && recent.is_empty());

        // A claimed ticket + a heartbeat.
        let t = mk(&s, "x", Priority::Med);
        s.claim(t.id, Some("b")).unwrap();
        s.pulse(Some(t.id), "running tests").unwrap();

        let (last, active, recent) = s.worker_view(10).unwrap();
        assert!(last.is_some());
        assert_eq!(active.unwrap().id, t.id, "in-progress ticket is the active one");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].message, "running tests");
        assert_eq!(recent[0].ticket_id, Some(t.id));
    }

    #[test]
    fn migration_upgrades_an_existing_v0_db() {
        let tmp = std::env::temp_dir().join(format!("drydock-migtest-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        // Simulate a DB written by the old code: only migration 001 applied, and
        // user_version never set (0) — and a row in a state the old CHECK allowed.
        {
            let conn = rusqlite::Connection::open(&tmp).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            let n = now();
            conn.execute(
                "INSERT INTO tickets (title, type, target, state, priority, goal, created_at, updated_at)
                 VALUES ('legacy', 'feature', 'svc', 'blocked', 'low', 'g', ?1, ?1)",
                rusqlite::params![n],
            )
            .unwrap();
            let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, 0, "old code leaves user_version at 0");
        }

        // Reopening via Store runs the pending migration, preserving the row…
        let s = Store::open(&tmp).unwrap();
        let tickets = s.list(None, None).unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].state, State::Blocked);
        // …and 'closed' is now accepted — the whole point.
        assert_eq!(s.close(tickets[0].id, None).unwrap().state, State::Closed);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn migration_preserves_comments_and_events_across_table_rebuilds() {
        // Migrations 002/003 DROP and recreate `tickets` (and 003 recreates
        // `comments`). Both reference rows carry `ON DELETE CASCADE`, so if the
        // rebuild ever ran with foreign keys enabled, dropping `tickets` would
        // silently delete the whole thread and audit trail. This guards that the
        // runner keeps FKs off for the rebuild and that nothing is lost.
        let tmp = std::env::temp_dir().join(format!("drydock-migcascade-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        // A v0 DB (only migration 001) with a ticket plus a comment and an event
        // hanging off it — exactly the rows a cascade would eat.
        {
            let conn = rusqlite::Connection::open(&tmp).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            let n = now();
            conn.execute(
                "INSERT INTO tickets (title, type, target, state, priority, goal, created_at, updated_at)
                 VALUES ('legacy', 'feature', 'svc', 'open', 'low', 'g', ?1, ?1)",
                rusqlite::params![n],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO comments (ticket_id, author, kind, body, created_at)
                 VALUES (1, 'human', 'note', 'remember this', ?1)",
                rusqlite::params![n],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO events (ticket_id, from_state, to_state, actor, detail, created_at)
                 VALUES (1, NULL, 'open', 'human', 'created', ?1)",
                rusqlite::params![n],
            )
            .unwrap();
        }

        // Reopening runs migrations 002–004, rebuilding both tables.
        let s = Store::open(&tmp).unwrap();
        let detail = s.detail(1).unwrap();
        assert_eq!(detail.comments.len(), 1, "comment must survive the rebuild");
        assert_eq!(detail.comments[0].body, "remember this");
        assert!(
            detail.events.iter().any(|e| e.detail.as_deref() == Some("created")),
            "event must survive the rebuild"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn stale_in_progress_is_reclaimed() {
        let s = store();
        let t = mk(&s, "a", Priority::Med);
        s.claim(t.id, Some("b")).unwrap();

        // A negative threshold puts the cutoff in the future, so any in-progress
        // ticket counts as stale — deterministic stand-in for "claimed long ago".
        let next = s.next(-1).unwrap().expect("stale ticket reclaimed");
        assert_eq!(next.id, t.id);
        assert_eq!(next.state, State::Open);

        let d = s.detail(t.id).unwrap();
        let reclaim = d
            .events
            .iter()
            .find(|e| e.detail.as_deref() == Some("reclaimed stale claim"))
            .expect("reclaim event recorded");
        assert_eq!(reclaim.from_state, Some(State::InProgress));
        assert_eq!(reclaim.to_state, State::Open);
    }

    #[test]
    fn list_filters_by_state() {
        let s = store();
        let a = mk(&s, "a", Priority::Med);
        let _b = mk(&s, "b", Priority::Med);
        s.claim(a.id, Some("br")).unwrap();

        assert_eq!(s.list(None, None).unwrap().len(), 2);
        assert_eq!(s.list(Some(State::Open), None).unwrap().len(), 1);
        let inprog = s.list(Some(State::InProgress), None).unwrap();
        assert_eq!(inprog.len(), 1);
        assert_eq!(inprog[0].id, a.id);
    }

    #[test]
    fn list_filters_by_text() {
        let s = store();
        let alpha = mk(&s, "alpha widget", Priority::Med);
        mk(&s, "beta gadget", Priority::Med);
        mk(&s, "100% coverage", Priority::Med);

        // Substring of the title, case-insensitive.
        let hits = s.list(None, Some("WIDGET")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, alpha.id);

        // Empty/whitespace query disables the filter (all tickets).
        assert_eq!(s.list(None, Some("   ")).unwrap().len(), 3);
        assert_eq!(s.list(None, None).unwrap().len(), 3);

        // A literal `%` is escaped — it matches the percent ticket, not everything.
        let pct = s.list(None, Some("100% cov")).unwrap();
        assert_eq!(pct.len(), 1);
        assert!(pct[0].title.starts_with("100%"));

        // State and text filters compose.
        assert_eq!(s.list(Some(State::Open), Some("gadget")).unwrap().len(), 1);
        assert_eq!(s.list(Some(State::InProgress), Some("gadget")).unwrap().len(), 0);
    }
}
