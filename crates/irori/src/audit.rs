//! The activity log: who did what, kept apart from history (`docs/specs/access.md` §3).
//!
//! History is lossy by design: a queue that drops under load, a few days kept, everything about
//! a device forgotten when it is removed. A record of who changed what must not be, so this has a
//! table of its own in `irori.db`, outside the tables a backup leaves out with history
//! ([`TABLES`]), and a writer of its own.
//!
//! Rows are written by one thread that takes them in batches. A row for which the answer must
//! not go out first (a sign-in, a change to people) is written and waited for
//! ([`Audit::record_and_wait`]); the rest are queued ([`Audit::record`]). The queue is bounded,
//! and a row that does not fit is counted and said in the log itself, so a gap is never silent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use irori_types::{
    AuditActor, AuditEntry, AuditPage, ContextId, ExtensionId, MOST_AUDIT_ROWS, Named, Outcome,
    Timestamp, TokenId, UserId,
};
use rusqlite::{Connection, OptionalExtension as _, params};

/// The tables the activity log lives in, for whoever needs to tell it from the rest of
/// `irori.db`: a backup that sets it apart, or a restore that leaves it as it is.
pub const TABLES: &[&str] = &["audit"];

/// Rows waiting for the writer before one is counted as lost.
const QUEUE: usize = 8192;
/// The most rows written in one transaction.
const BATCH: usize = 256;
/// How long to wait before writing a batch again after it failed.
const RETRY: Duration = Duration::from_secs(1);
/// How many times a batch is tried before it is given up.
const TRIES: u32 = 5;
/// How many rows about somebody who is nobody are written in a minute. Anyone who can reach
/// Irori can make these, so they are bounded; the ones over are counted and said once, and
/// the rows about people who are known are never held back by this.
const NOBODY_PER_MINUTE: u32 = 30;
/// How often the log is pruned when nothing is being written.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);
/// Rows written between prunes while they keep coming.
const PRUNE_AFTER_ROWS: u64 = 500;
/// When the log is over its size, it is cut down to this share of it, so the cutting does not
/// happen again at the next row.
const KEEP_OF_MAX_PERCENT: u64 = 90;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ns INTEGER NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id TEXT,
    actor_name TEXT,
    actor_person_id TEXT,
    actor_person_name TEXT,
    action TEXT NOT NULL,
    target TEXT,
    outcome TEXT NOT NULL,
    detail TEXT,
    context_id TEXT,
    client TEXT
);
CREATE INDEX IF NOT EXISTS audit_at ON audit (at_ns, id);
CREATE INDEX IF NOT EXISTS audit_context ON audit (context_id) WHERE context_id IS NOT NULL;
";

/// What to write down. The time is taken when it is queued.
#[derive(Debug, Clone)]
pub struct Row {
    pub actor: AuditActor,
    pub action: String,
    pub target: Option<String>,
    pub outcome: Outcome,
    pub detail: Option<String>,
    pub context_id: Option<ContextId>,
    pub client: Option<String>,
}

impl Row {
    pub fn new(actor: AuditActor, action: impl Into<String>, outcome: Outcome) -> Self {
        Self {
            actor,
            action: action.into(),
            target: None,
            outcome,
            detail: None,
            context_id: None,
            client: None,
        }
    }

    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// How long and how much is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    pub days: u32,
    pub max_rows: u32,
}

impl Keep {
    /// What `irori.toml` says to keep: a row is held at least as long as history is, because a
    /// state change's "who" is read back from here.
    pub fn from_settings(
        audit: &irori_config::AuditSection,
        recorder: &irori_config::RecorderSection,
    ) -> Self {
        Self {
            days: audit.days_given(recorder),
            max_rows: audit.max_rows,
        }
    }
}

impl Default for Keep {
    fn default() -> Self {
        Self {
            days: 90,
            max_rows: 100_000,
        }
    }
}

/// What the page or the command line asks of the log.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// A person, by id: what they did, and what programs made for them did.
    pub user: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<Outcome>,
    pub since: Option<Timestamp>,
    /// Only rows older than this one, to read on from where a page ended.
    pub before: Option<u64>,
    /// At most this many; never more than [`MOST_AUDIT_ROWS`].
    pub limit: Option<usize>,
}

enum Message {
    Row(Box<Stamped>, Option<tokio::sync::oneshot::Sender<()>>),
    /// The size or age it is kept to changed: prune now.
    Prune,
}

#[derive(Debug)]
struct Stamped {
    at: Timestamp,
    row: Row,
}

#[derive(Debug)]
struct Shared {
    db: PathBuf,
    keep: Mutex<Keep>,
    /// Rows that did not fit in the queue, not yet said in the log.
    lost: AtomicU64,
    /// Tries by somebody who is nobody (a wrong password, a request with no sign-in) that were
    /// over [`NOBODY_PER_MINUTE`], not yet said in the log.
    unwritten_tries: AtomicU64,
    /// When the minute `tries_this_minute` counts began, and how many were written in it.
    nobody: Mutex<(std::time::Instant, u32)>,
}

/// The log, as the rest of Irori holds it. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Audit {
    shared: Arc<Shared>,
    queue: SyncSender<Message>,
}

impl Audit {
    /// Opens the log in `db`, making its table if it is not there, and starts its writer.
    pub fn open(db: &Path) -> anyhow::Result<Self> {
        let conn = connect(db)?;
        drop(conn);
        let (queue, waiting) = std::sync::mpsc::sync_channel(QUEUE);
        let shared = Arc::new(Shared {
            db: db.to_path_buf(),
            keep: Mutex::new(Keep::default()),
            lost: AtomicU64::new(0),
            unwritten_tries: AtomicU64::new(0),
            nobody: Mutex::new((std::time::Instant::now(), 0)),
        });
        let writer = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("irori-audit".to_owned())
            .spawn(move || write_forever(&writer, &waiting))?;
        Ok(Self { shared, queue })
    }

    /// Changes how long and how much is kept, and prunes to it at once.
    pub fn keep(&self, keep: Keep) {
        {
            let mut held = self
                .shared
                .keep
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if *held == keep {
                return;
            }
            *held = keep;
        }
        let _ = self.queue.try_send(Message::Prune);
    }

    /// Whether a row about somebody who is nobody may be written now. Anything else may.
    fn allowed(&self, row: &Row) -> bool {
        if !matches!(row.actor, AuditActor::Nobody) {
            return true;
        }
        let mut held = self
            .shared
            .nobody
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if held.0.elapsed() >= Duration::from_secs(60) {
            *held = (std::time::Instant::now(), 0);
        }
        if held.1 >= NOBODY_PER_MINUTE {
            self.shared.unwritten_tries.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        held.1 += 1;
        true
    }

    /// Queues a row. It does not wait for it to be written.
    pub fn record(&self, row: Row) {
        if !self.allowed(&row) {
            return;
        }
        let message = Message::Row(Box::new(stamp(row)), None);
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) =
            self.queue.try_send(message)
        {
            self.shared.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Writes a row and waits until it is on disk: for what the answer must not go out before.
    /// A writer that has stopped is the same as a row that did not fit.
    pub async fn record_and_wait(&self, row: Row) {
        if !self.allowed(&row) {
            return;
        }
        let (done, written) = tokio::sync::oneshot::channel();
        let message = Message::Row(Box::new(stamp(row)), Some(done));
        match self.queue.try_send(message) {
            Ok(()) => {
                let _ = written.await;
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.shared.lost.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The newest rows that match, newest first.
    pub fn list(&self, query: &Query) -> Result<AuditPage, String> {
        let conn = connect(&self.shared.db).map_err(|e| e.to_string())?;
        let limit = query.limit.unwrap_or(200).clamp(1, MOST_AUDIT_ROWS);
        let mut sql = String::from(
            "SELECT id, at_ns, actor_kind, actor_id, actor_name, actor_person_id, \
             actor_person_name, action, target, outcome, detail, context_id, client \
             FROM audit WHERE 1 = 1",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(user) = &query.user {
            sql.push_str(" AND (actor_id = ? AND actor_kind = 'person' OR actor_person_id = ?)");
            args.push(user.clone().into());
            args.push(user.clone().into());
        }
        if let Some(action) = &query.action {
            sql.push_str(" AND action = ?");
            args.push(action.clone().into());
        }
        if let Some(outcome) = query.outcome {
            sql.push_str(" AND outcome = ?");
            args.push(outcome.as_str().to_owned().into());
        }
        if let Some(since) = query.since {
            sql.push_str(" AND at_ns >= ?");
            args.push(nanos(since).into());
        }
        if let Some(before) = query.before {
            sql.push_str(" AND id < ?");
            args.push(i64::try_from(before).unwrap_or(i64::MAX).into());
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        args.push(i64::try_from(limit + 1).unwrap_or(i64::MAX).into());
        let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut entries: Vec<AuditEntry> = statement
            .query_map(rusqlite::params_from_iter(args), read_row)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        let older = (entries.len() > limit).then(|| {
            entries.truncate(limit);
            entries.last().map_or(0, |entry| entry.id)
        });
        Ok(AuditPage { entries, older })
    }

    /// Who sent the commands whose calls had these contexts, for history to say who turned
    /// something on. Only commands a person or a program sent are in the log.
    pub fn who_for(&self, contexts: &[ContextId]) -> HashMap<ContextId, AuditActor> {
        let mut found = HashMap::new();
        if contexts.is_empty() {
            return found;
        }
        let Ok(conn) = connect(&self.shared.db) else {
            return found;
        };
        let Ok(mut statement) = conn.prepare(
            "SELECT id, at_ns, actor_kind, actor_id, actor_name, actor_person_id, \
             actor_person_name, action, target, outcome, detail, context_id, client \
             FROM audit WHERE context_id = ?1 AND action = 'command' AND outcome = 'ok'",
        ) else {
            return found;
        };
        for context in contexts {
            let entry = statement
                .query_row([context.as_str()], read_row)
                .optional()
                .ok()
                .flatten();
            if let Some(entry) = entry {
                found.insert(context.clone(), entry.actor);
            }
        }
        found
    }

    /// How many rows there are. For tests and the page's "N rows kept".
    pub fn count(&self) -> u64 {
        connect(&self.shared.db)
            .and_then(|conn| conn.query_row("SELECT count(*) FROM audit", [], |row| row.get(0)))
            .ok()
            .and_then(|n: i64| u64::try_from(n).ok())
            .unwrap_or(0)
    }
}

fn stamp(row: Row) -> Stamped {
    Stamped {
        at: Timestamp::from_jiff(jiff::Timestamp::now()),
        row,
    }
}

fn connect(db: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db)?;
    crate::db::wait_when_busy(&conn)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn nanos(at: Timestamp) -> i64 {
    i64::try_from(at.as_jiff().as_nanosecond()).unwrap_or(i64::MAX)
}

fn from_nanos(ns: i64) -> Timestamp {
    jiff::Timestamp::from_nanosecond(i128::from(ns))
        .map_or_else(|_| Timestamp::from_jiff(jiff::Timestamp::UNIX_EPOCH), Timestamp::from_jiff)
}

/// Splits an actor into the columns it is kept in.
#[allow(clippy::type_complexity)]
fn columns(
    actor: &AuditActor,
) -> (
    &'static str,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match actor {
        AuditActor::Person { id, name } => (
            "person",
            Some(id.as_str().to_owned()),
            Some(name.clone()),
            None,
            None,
        ),
        AuditActor::Token { id, name, person } => (
            "token",
            Some(id.as_str().to_owned()),
            Some(name.clone()),
            person.as_ref().map(|p| p.id.as_str().to_owned()),
            person.as_ref().map(|p| p.name.clone()),
        ),
        AuditActor::Extension { id } => ("extension", Some(id.as_str().to_owned()), None, None, None),
        AuditActor::Irori => ("irori", None, None, None, None),
        AuditActor::Nobody => ("nobody", None, None, None, None),
    }
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
    let kind: String = row.get(2)?;
    let id: Option<String> = row.get(3)?;
    let name: Option<String> = row.get(4)?;
    let person_id: Option<String> = row.get(5)?;
    let person_name: Option<String> = row.get(6)?;
    let actor = match kind.as_str() {
        "person" => id
            .and_then(|id| UserId::try_from(id).ok())
            .map(|id| AuditActor::Person {
                id,
                name: name.clone().unwrap_or_default(),
            }),
        "token" => id.and_then(|id| TokenId::try_from(id).ok()).map(|id| {
            AuditActor::Token {
                id,
                name: name.clone().unwrap_or_default(),
                person: person_id
                    .and_then(|id| UserId::try_from(id).ok())
                    .map(|id| Named {
                        id,
                        name: person_name.clone().unwrap_or_default(),
                    }),
            }
        }),
        "extension" => id
            .and_then(|id| ExtensionId::try_from(id).ok())
            .map(|id| AuditActor::Extension { id }),
        "irori" => Some(AuditActor::Irori),
        _ => None,
    }
    .unwrap_or(AuditActor::Nobody);
    let outcome: String = row.get(9)?;
    let at_ns: i64 = row.get(1)?;
    let id: i64 = row.get(0)?;
    Ok(AuditEntry {
        id: u64::try_from(id).unwrap_or(0),
        at: from_nanos(at_ns),
        actor,
        action: row.get(7)?,
        target: row.get(8)?,
        outcome: outcome.parse().unwrap_or(Outcome::Failed),
        detail: row.get(10)?,
        context_id: row
            .get::<_, Option<String>>(11)?
            .and_then(|id| ContextId::try_from(id).ok()),
        client: row.get(12)?,
    })
}

fn insert(conn: &Connection, at: Timestamp, row: &Row) -> rusqlite::Result<()> {
    let (kind, id, name, person_id, person_name) = columns(&row.actor);
    conn.execute(
        "INSERT INTO audit (at_ns, actor_kind, actor_id, actor_name, actor_person_id, \
         actor_person_name, action, target, outcome, detail, context_id, client) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            nanos(at),
            kind,
            id,
            name,
            person_id,
            person_name,
            row.action,
            row.target,
            row.outcome.as_str(),
            row.detail,
            row.context_id.as_ref().map(ContextId::as_str),
            row.client,
        ],
    )?;
    Ok(())
}

/// Takes rows from the queue and writes them, a batch at a time, until the log is dropped.
fn write_forever(shared: &Shared, waiting: &Receiver<Message>) {
    let mut conn = loop {
        match connect(&shared.db) {
            Ok(conn) => break conn,
            Err(error) => {
                tracing::warn!(%error, "couldn't open the activity log; trying again");
                std::thread::sleep(RETRY);
            }
        }
    };
    let mut since_prune = 0u64;
    prune(&mut conn, shared);
    loop {
        let first = match waiting.recv_timeout(PRUNE_EVERY) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => {
                prune(&mut conn, shared);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut messages = vec![first];
        while messages.len() < BATCH {
            match waiting.try_recv() {
                Ok(message) => messages.push(message),
                Err(_) => break,
            }
        }
        let mut rows = Vec::new();
        let mut acks = Vec::new();
        let mut prune_now = false;
        for message in messages {
            match message {
                Message::Row(stamped, ack) => {
                    rows.push(*stamped);
                    acks.extend(ack);
                }
                Message::Prune => prune_now = true,
            }
        }
        let lost = shared.lost.swap(0, Ordering::Relaxed);
        if lost > 0 {
            tracing::warn!(lost, "the activity log fell behind and lost rows");
            rows.push(Stamped {
                at: Timestamp::from_jiff(jiff::Timestamp::now()),
                row: Row::new(AuditActor::Irori, "activity_lost", Outcome::Failed)
                    .detail(format!("{lost} rows could not be written: the log fell behind")),
            });
        }
        let unwritten = shared.unwritten_tries.swap(0, Ordering::Relaxed);
        if unwritten > 0 {
            rows.push(Stamped {
                at: Timestamp::from_jiff(jiff::Timestamp::now()),
                row: Row::new(AuditActor::Nobody, "tries_unwritten", Outcome::Denied).detail(
                    format!(
                        "{unwritten} more tries by nobody who was signed in were not written: \
                         more than {NOBODY_PER_MINUTE} a minute"
                    ),
                ),
            });
        }
        if !rows.is_empty() {
            let written = write_batch(&mut conn, shared, &rows);
            since_prune += rows.len() as u64;
            // Said even when it could not be written: nobody should wait on a log that is gone.
            let _ = written;
        }
        for ack in acks {
            let _ = ack.send(());
        }
        if prune_now || since_prune >= PRUNE_AFTER_ROWS {
            prune(&mut conn, shared);
            since_prune = 0;
        }
    }
}

fn write_batch(conn: &mut Connection, shared: &Shared, rows: &[Stamped]) -> bool {
    for attempt in 1..=TRIES {
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            for stamped in rows {
                insert(&tx, stamped.at, &stamped.row)?;
            }
            tx.commit()
        })();
        match result {
            Ok(()) => return true,
            Err(error) => {
                tracing::warn!(%error, attempt, "couldn't write to the activity log");
                std::thread::sleep(RETRY);
                if let Ok(fresh) = connect(&shared.db) {
                    *conn = fresh;
                }
            }
        }
    }
    tracing::error!(
        rows = rows.len(),
        "gave up writing to the activity log; these rows are lost"
    );
    false
}

/// Takes out rows that are too old, then the oldest if there are too many, and says so.
fn prune(conn: &mut Connection, shared: &Shared) {
    let keep = *shared.keep.lock().unwrap_or_else(PoisonError::into_inner);
    let cutoff = jiff::Timestamp::now()
        .checked_sub(jiff::SignedDuration::from_hours(i64::from(keep.days) * 24))
        .map_or(0, |at| i64::try_from(at.as_nanosecond()).unwrap_or(0));
    if let Err(error) = conn.execute("DELETE FROM audit WHERE at_ns < ?1", [cutoff]) {
        tracing::warn!(%error, "couldn't prune the activity log by age");
        return;
    }
    let max = i64::from(keep.max_rows);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM audit", [], |row| row.get(0))
        .unwrap_or(0);
    if rows <= max {
        return;
    }
    let keep_rows = max * i64::try_from(KEEP_OF_MAX_PERCENT).unwrap_or(90) / 100;
    let cut = conn.execute(
        "DELETE FROM audit WHERE id <= (SELECT id FROM audit ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [keep_rows],
    );
    let Ok(removed) = cut else {
        tracing::warn!("couldn't trim the activity log to its size");
        return;
    };
    let busiest = busiest(conn);
    let mut detail = format!(
        "{removed} of the oldest rows were removed to keep the log under {max} rows ([audit] max_rows)"
    );
    if let Some(actor) = busiest {
        detail.push_str(&format!("; most of what is kept is from {actor}"));
    }
    let _ = insert(
        conn,
        Timestamp::from_jiff(jiff::Timestamp::now()),
        &Row::new(AuditActor::Irori, "activity_trimmed", Outcome::Ok).detail(detail),
    );
}

/// The actor that wrote more than half of what is kept, if there is one: a program that calls
/// the API every few seconds is recorded by design, and this says which.
fn busiest(conn: &Connection) -> Option<AuditActor> {
    let (row, count): (AuditEntry, i64) = conn
        .query_row(
            "SELECT id, at_ns, actor_kind, actor_id, actor_name, actor_person_id, \
             actor_person_name, action, target, outcome, detail, context_id, client, count(*) AS c \
             FROM audit WHERE actor_kind IN ('person', 'token', 'extension') \
             GROUP BY actor_kind, actor_id ORDER BY c DESC LIMIT 1",
            [],
            |row| Ok((read_row(row)?, row.get(13)?)),
        )
        .ok()?;
    let total: i64 = conn
        .query_row("SELECT count(*) FROM audit", [], |row| row.get(0))
        .ok()?;
    (count * 2 > total).then_some(row.actor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, name: &str) -> AuditActor {
        AuditActor::Person {
            id: UserId::try_from(id).expect("valid"),
            name: name.to_owned(),
        }
    }

    fn open() -> anyhow::Result<(tempfile::TempDir, Audit)> {
        let dir = tempfile::tempdir()?;
        let audit = Audit::open(&dir.path().join("irori.db"))?;
        Ok((dir, audit))
    }

    #[tokio::test]
    async fn a_row_waited_for_is_there_when_the_wait_ends() -> anyhow::Result<()> {
        let (_dir, audit) = open()?;
        audit
            .record_and_wait(
                Row::new(person("leo", "Leo"), "sign_in", Outcome::Ok).detail("from the hall"),
            )
            .await;
        let page = audit.list(&Query::default()).map_err(anyhow::Error::msg)?;
        assert_eq!(page.entries.len(), 1);
        let row = &page.entries[0];
        assert_eq!(row.action, "sign_in");
        assert_eq!(row.actor.to_string(), "Leo");
        assert_eq!(row.detail.as_deref(), Some("from the hall"));
        assert_eq!(page.older, None);
        Ok(())
    }

    /// The name a row carries is the name at the time: the person can be renamed, or removed
    /// and made again, and the row still reads as it did.
    #[tokio::test]
    async fn a_row_keeps_who_they_were_and_a_token_keeps_its_person() -> anyhow::Result<()> {
        let (_dir, audit) = open()?;
        let tablet = AuditActor::Token {
            id: TokenId::try_from("tablet").expect("valid"),
            name: "Tablet".to_owned(),
            person: Some(Named {
                id: UserId::try_from("nico").expect("valid"),
                name: "Nico".to_owned(),
            }),
        };
        audit
            .record_and_wait(Row::new(tablet.clone(), "command", Outcome::Ok).target("light.hall"))
            .await;
        let page = audit.list(&Query::default()).map_err(anyhow::Error::msg)?;
        assert_eq!(page.entries[0].actor, tablet);
        // What Nico did, programs made for Nico included.
        let by_nico = audit
            .list(&Query {
                user: Some("nico".to_owned()),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        assert_eq!(by_nico.entries.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn the_newest_come_first_and_a_page_says_where_the_next_begins() -> anyhow::Result<()> {
        let (_dir, audit) = open()?;
        for n in 0..5 {
            audit
                .record_and_wait(
                    Row::new(person("leo", "Leo"), "command", Outcome::Ok).target(format!("e{n}")),
                )
                .await;
        }
        let first = audit
            .list(&Query {
                limit: Some(2),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        let targets = |page: &AuditPage| {
            page.entries
                .iter()
                .map(|e| e.target.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(&first), ["e4", "e3"]);
        let next = audit
            .list(&Query {
                limit: Some(2),
                before: first.older,
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        assert_eq!(targets(&next), ["e2", "e1"]);
        Ok(())
    }

    #[tokio::test]
    async fn rows_are_found_by_action_and_outcome() -> anyhow::Result<()> {
        let (_dir, audit) = open()?;
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "command", Outcome::Denied))
            .await;
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "command", Outcome::Ok))
            .await;
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "sign_in", Outcome::Ok))
            .await;
        let denied = audit
            .list(&Query {
                outcome: Some(Outcome::Denied),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        assert_eq!(denied.entries.len(), 1);
        let commands = audit
            .list(&Query {
                action: Some("command".to_owned()),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        assert_eq!(commands.entries.len(), 2);
        Ok(())
    }

    /// A command's row carries the call's context, and a state change's `parent_id` finds it.
    #[tokio::test]
    async fn a_calls_context_finds_who_sent_it() -> anyhow::Result<()> {
        let (_dir, audit) = open()?;
        let context = ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid");
        let mut row = Row::new(person("leo", "Leo"), "command", Outcome::Ok).target("light.hall");
        row.context_id = Some(context.clone());
        audit.record_and_wait(row).await;
        let mut denied = Row::new(person("mia", "Mia"), "command", Outcome::Denied);
        let other = ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8JA").expect("valid");
        denied.context_id = Some(other.clone());
        audit.record_and_wait(denied).await;
        let found = audit.who_for(&[context.clone(), other.clone()]);
        assert_eq!(found.get(&context).map(ToString::to_string).as_deref(), Some("Leo"));
        assert!(!found.contains_key(&other), "a command that was refused turned nothing on");
        Ok(())
    }

    /// Old rows go by age, then the oldest go by size; the cut says so, and names the program
    /// that wrote most of what is left.
    #[tokio::test]
    async fn the_log_is_kept_to_its_size_and_says_what_filled_it() -> anyhow::Result<()> {
        let (dir, audit) = open()?;
        audit.keep(Keep {
            days: 30,
            max_rows: 1000,
        });
        let flood = AuditActor::Token {
            id: TokenId::try_from("poller").expect("valid"),
            name: "Poller".to_owned(),
            person: None,
        };
        for _ in 0..1100 {
            audit.record(Row::new(flood.clone(), "command", Outcome::Ok));
        }
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "sign_in", Outcome::Ok))
            .await;
        // The writer prunes after a run of rows; ask it to once more and wait for it.
        audit.keep(Keep {
            days: 31,
            max_rows: 1000,
        });
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "sign_out", Outcome::Ok))
            .await;
        let conn = connect(&dir.path().join("irori.db"))?;
        let rows: i64 = conn.query_row("SELECT count(*) FROM audit", [], |r| r.get(0))?;
        assert!(rows <= 1000, "{rows} rows are kept");
        let page = audit
            .list(&Query {
                action: Some("activity_trimmed".to_owned()),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        let said = page
            .entries
            .first()
            .and_then(|e| e.detail.clone())
            .unwrap_or_default();
        assert!(said.contains("max_rows"), "{said}");
        assert!(said.contains("Poller"), "{said}");
        Ok(())
    }

    /// Anyone who can reach Irori can make rows about nobody, so only so many are written in a
    /// minute, and the rest are counted and said once.
    #[tokio::test]
    async fn rows_about_nobody_are_bounded_and_the_rest_are_counted() -> anyhow::Result<()> {
        let (_dir, audit) = open()?;
        for _ in 0..(NOBODY_PER_MINUTE + 25) {
            audit
                .record_and_wait(Row::new(AuditActor::Nobody, "sign_in", Outcome::Denied))
                .await;
        }
        // A person who is known is never held back by it.
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "sign_in", Outcome::Ok))
            .await;
        let page = audit
            .list(&Query {
                limit: Some(MOST_AUDIT_ROWS),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        let nobody = page
            .entries
            .iter()
            .filter(|e| e.actor == AuditActor::Nobody && e.action == "sign_in")
            .count();
        assert_eq!(nobody, NOBODY_PER_MINUTE as usize);
        assert!(page.entries.iter().any(|e| e.actor.to_string() == "Leo"));
        // Said once, at the next write.
        audit
            .record_and_wait(Row::new(person("leo", "Leo"), "sign_out", Outcome::Ok))
            .await;
        let said = audit
            .list(&Query {
                action: Some("tries_unwritten".to_owned()),
                ..Query::default()
            })
            .map_err(anyhow::Error::msg)?;
        assert_eq!(said.entries.len(), 1);
        assert!(
            said.entries[0]
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("25 more")),
            "{:?}",
            said.entries[0].detail
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_row_older_than_the_days_kept_is_removed() -> anyhow::Result<()> {
        let (dir, audit) = open()?;
        {
            let conn = connect(&dir.path().join("irori.db"))?;
            let long_ago = nanos(Timestamp::from_second(1_000_000_000).expect("in range"));
            conn.execute(
                "INSERT INTO audit (at_ns, actor_kind, action, outcome) \
                 VALUES (?1, 'irori', 'old', 'ok')",
                [long_ago],
            )?;
        }
        assert_eq!(audit.count(), 1);
        audit.keep(Keep {
            days: 10,
            max_rows: 5000,
        });
        audit
            .record_and_wait(Row::new(AuditActor::Irori, "new", Outcome::Ok))
            .await;
        let page = audit.list(&Query::default()).map_err(anyhow::Error::msg)?;
        assert_eq!(
            page.entries.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
            ["new"]
        );
        Ok(())
    }
}
