//! Entity history on disk.
//!
//! The running process used to remember the last day of changes in memory, and forget them when
//! it exited. This crate is that diary: each real change is appended to `state_history` in the
//! home's `irori.db`, and a later start reads it back.
//!
//! One thread owns the connection. Everyone else sends it a message — save, forget, read, or
//! stop — and the messages are applied in the order they were sent, so a read sees the saves
//! that were asked for before it. A burst of saves shares one transaction. When the queue is
//! full, a new change is dropped and counted rather than making the event loop wait. Forgetting
//! an entity and reading its history do wait, because dropping either of those would be a wrong
//! answer rather than a missed sample.
//!
//! Automation traces are not stored here. The Automations extension keeps its own runs.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use irori_types::{EntityId, EntityState, Timestamp};
use jiff::ToSpan as _;
use rusqlite::Connection;

/// How long readings are kept when a home does not say otherwise.
pub const DEFAULT_RETAIN_DAYS: u32 = 7;

/// The most changes one read returns. The newest are kept, and they come back oldest first.
pub const MAX_RETURNED: usize = 2_000;

/// Saves waiting on the writer. Past this, new changes are dropped.
const QUEUE: usize = 1_024;

/// Saves written in one transaction.
const BURST: usize = 64;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS state_history (
    id INTEGER PRIMARY KEY,
    entity_id TEXT NOT NULL,
    updated_ns INTEGER NOT NULL,
    state TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS state_history_entity_time
    ON state_history (entity_id, updated_ns, id);
";

/// Why the recorder could not be opened.
#[derive(Debug)]
pub struct Error {
    message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

enum Job {
    Append(Box<EntityState>),
    Forget(EntityId),
    Query {
        entity_id: EntityId,
        since: Timestamp,
        reply: std::sync::mpsc::Sender<Vec<EntityState>>,
    },
    Check(std::sync::mpsc::Sender<bool>),
    Shutdown,
}

/// The diary. Cloning shares the writer thread. The thread stops when the last clone is dropped.
#[derive(Clone)]
pub struct Recorder {
    shared: Arc<Shared>,
}

struct Shared {
    tx: SyncSender<Job>,
    shutdown: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for Recorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Recorder")
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wakes a thread that is waiting. If the queue is already full, the flag is what it
        // sees once it finishes the batch it is in.
        let _ = self.tx.try_send(Job::Shutdown);
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle
            && let Err(error) = handle.join()
        {
            tracing::error!("the recorder thread panicked: {error:?}");
        }
    }
}

impl Recorder {
    /// Opens `path` (the home's `irori.db`) and starts the writer. `retain_days` is at least 1.
    /// Rows older than that are deleted before this returns, and about once an hour after.
    pub fn open(path: &Path, retain_days: u32) -> Result<Self, Error> {
        if retain_days == 0 {
            return Err(Error::new("retain_days must be at least 1"));
        }
        let path = path.to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (tx, rx) = sync_channel(QUEUE);
        let shutdown = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let handle = std::thread::Builder::new()
            .name("irori-recorder".to_owned())
            .spawn(move || match open_connection(&path) {
                Ok(connection) => {
                    let mut last_write_ok = true;
                    if let Err(error) = prune(&connection, retain_days) {
                        last_write_ok = false;
                        tracing::warn!(%error, "old history could not be deleted");
                    }
                    let _ = ready_tx.send(Ok(()));
                    serve(
                        connection,
                        rx,
                        shutdown_for_thread,
                        retain_days,
                        last_write_ok,
                    );
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|error| Error::new(format!("could not start the recorder thread: {error}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shared: Arc::new(Shared {
                    tx,
                    shutdown,
                    dropped,
                    thread: Mutex::new(Some(handle)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => {
                let _ = handle.join();
                Err(Error::new(
                    "the recorder thread stopped before the database was ready",
                ))
            }
        }
    }

    /// Remembers one change. If the writer is behind, the change is dropped and a warning is
    /// logged (the first time, and every thousand after).
    pub fn append(&self, state: EntityState) {
        match self.shared.tx.try_send(Job::Append(Box::new(state))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let dropped = self.shared.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 || dropped.is_multiple_of(1_000) {
                    tracing::warn!(
                        dropped,
                        "the recorder fell behind and dropped state changes"
                    );
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::warn!("the recorder has stopped; a state change was not saved");
            }
        }
    }

    /// Deletes every stored change for this entity. Waits until the delete is in the queue, so
    /// it lands after the changes that were already saved and is not itself dropped.
    pub fn forget(&self, entity_id: &EntityId) {
        if self.shared.tx.send(Job::Forget(entity_id.clone())).is_err() {
            tracing::warn!("the recorder has stopped; a removed entity's history was not deleted");
        }
    }

    /// Changes for `entity_id` with `last_updated >= since`, oldest first. At most
    /// [`MAX_RETURNED`], keeping the newest when there are more.
    pub fn since(&self, entity_id: &EntityId, since: Timestamp) -> Vec<EntityState> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self
            .shared
            .tx
            .send(Job::Query {
                entity_id: entity_id.clone(),
                since,
                reply: reply_tx,
            })
            .is_err()
        {
            tracing::warn!("the recorder has stopped; history could not be read");
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }

    /// Whether a trivial read works and the last write succeeded.
    pub fn check(&self) -> bool {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self.shared.tx.send(Job::Check(reply_tx)).is_err() {
            return false;
        }
        reply_rx.recv().unwrap_or(false)
    }
}

fn open_connection(path: &Path) -> Result<Connection, Error> {
    let connection = Connection::open(path)
        .map_err(|error| Error::new(format!("failed to open {}: {error}", path.display())))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| Error::new(format!("failed to set a busy timeout: {error}")))?;
    let journal: String = connection
        .pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))
        .map_err(|error| Error::new(format!("failed to set WAL mode: {error}")))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(Error::new(format!(
            "{} refused WAL mode (got {journal})",
            path.display()
        )));
    }
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| Error::new(format!("failed to set synchronous mode: {error}")))?;
    connection
        .execute_batch(SCHEMA)
        .map_err(|error| Error::new(format!("failed to prepare history: {error}")))?;
    Ok(connection)
}

fn serve(
    mut connection: Connection,
    rx: Receiver<Job>,
    shutdown: Arc<AtomicBool>,
    retain_days: u32,
    mut last_write_ok: bool,
) {
    let mut last_prune = Instant::now();
    let mut batch = Vec::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            drain(
                &mut connection,
                &rx,
                &mut batch,
                &mut last_write_ok,
                &shutdown,
            );
            break;
        }
        let first = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                if last_prune.elapsed() >= Duration::from_secs(60 * 60) {
                    note_write(&mut last_write_ok, prune(&connection, retain_days));
                    last_prune = Instant::now();
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut jobs = vec![first];
        while jobs.len() < BURST {
            match rx.try_recv() {
                Ok(job) => jobs.push(job),
                Err(_) => break,
            }
        }
        for job in jobs {
            apply(
                job,
                &mut connection,
                &mut batch,
                &mut last_write_ok,
                &shutdown,
            );
        }
        write_batch(&mut connection, &mut batch, &mut last_write_ok);
    }
}

/// Applies whatever is already queued, then commits. Used when stopping, so a change that was
/// accepted before shutdown is not left in the channel.
fn drain(
    connection: &mut Connection,
    rx: &Receiver<Job>,
    batch: &mut Vec<EntityState>,
    last_write_ok: &mut bool,
    shutdown: &AtomicBool,
) {
    while let Ok(job) = rx.try_recv() {
        apply(job, connection, batch, last_write_ok, shutdown);
    }
    write_batch(connection, batch, last_write_ok);
}

fn apply(
    job: Job,
    connection: &mut Connection,
    batch: &mut Vec<EntityState>,
    last_write_ok: &mut bool,
    shutdown: &AtomicBool,
) {
    match job {
        Job::Append(state) => batch.push(*state),
        Job::Forget(entity_id) => {
            write_batch(connection, batch, last_write_ok);
            note_write(last_write_ok, forget(connection, &entity_id));
        }
        Job::Query {
            entity_id,
            since,
            reply,
        } => {
            write_batch(connection, batch, last_write_ok);
            let rows = match read(connection, &entity_id, since) {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(%error, "history could not be read");
                    Vec::new()
                }
            };
            let _ = reply.send(rows);
        }
        Job::Check(reply) => {
            let readable = connection
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .is_ok();
            let _ = reply.send(*last_write_ok && readable);
        }
        Job::Shutdown => shutdown.store(true, Ordering::SeqCst),
    }
}

fn write_batch(
    connection: &mut Connection,
    batch: &mut Vec<EntityState>,
    last_write_ok: &mut bool,
) {
    if batch.is_empty() {
        return;
    }
    note_write(last_write_ok, insert_many(connection, batch));
    batch.clear();
}

fn note_write(last_write_ok: &mut bool, result: Result<(), String>) {
    match result {
        Ok(()) => *last_write_ok = true,
        Err(error) => {
            *last_write_ok = false;
            tracing::warn!(%error, "history could not be written");
        }
    }
}

fn insert_many(connection: &mut Connection, states: &[EntityState]) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    {
        let mut statement = transaction
            .prepare("INSERT INTO state_history (entity_id, updated_ns, state) VALUES (?1, ?2, ?3)")
            .map_err(|error| error.to_string())?;
        for state in states {
            let Some(updated) = nanos(state.last_updated) else {
                tracing::warn!(
                    entity = %state.entity_id,
                    "a state change has a timestamp this recorder cannot store"
                );
                continue;
            };
            let json = match serde_json::to_string(state) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!(
                        entity = %state.entity_id,
                        %error,
                        "a state change could not be stored"
                    );
                    continue;
                }
            };
            statement
                .execute(rusqlite::params![state.entity_id.as_str(), updated, json])
                .map_err(|error| error.to_string())?;
        }
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn forget(connection: &Connection, entity_id: &EntityId) -> Result<(), String> {
    connection
        .execute(
            "DELETE FROM state_history WHERE entity_id = ?1",
            [entity_id.as_str()],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn read(
    connection: &Connection,
    entity_id: &EntityId,
    since: Timestamp,
) -> Result<Vec<EntityState>, String> {
    let Some(since_ns) = nanos(since) else {
        return Ok(Vec::new());
    };
    let mut statement = connection
        .prepare(
            "SELECT state FROM state_history
             WHERE entity_id = ?1 AND updated_ns >= ?2
             ORDER BY updated_ns DESC, id DESC
             LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![
                entity_id.as_str(),
                since_ns,
                i64::try_from(MAX_RETURNED).expect("the history cap fits in a SQL limit")
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())?;
    let mut states = Vec::new();
    for row in rows {
        let text = row.map_err(|error| error.to_string())?;
        match serde_json::from_str::<EntityState>(&text) {
            Ok(state) => states.push(state),
            Err(error) => tracing::warn!(%error, "skipped a history row that does not parse"),
        }
    }
    states.reverse();
    Ok(states)
}

fn prune(connection: &Connection, retain_days: u32) -> Result<(), String> {
    let Some(cutoff) = cutoff_ns(retain_days) else {
        // A span this long does not fit in a timestamp. Keeping the rows is the safe failure.
        tracing::warn!(retain_days, "history retention is too long to apply");
        return Ok(());
    };
    connection
        .execute("DELETE FROM state_history WHERE updated_ns < ?1", [cutoff])
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn cutoff_ns(retain_days: u32) -> Option<i64> {
    // A timestamp has no calendar, so a day here is 24 hours. `Span::days` is refused on a
    // timestamp for that reason.
    let hours = i64::from(retain_days).checked_mul(24)?;
    let cutoff = jiff::Timestamp::now().checked_sub(hours.hours()).ok()?;
    i64::try_from(cutoff.as_nanosecond()).ok()
}

fn nanos(timestamp: Timestamp) -> Option<i64> {
    i64::try_from(timestamp.as_jiff().as_nanosecond()).ok()
}

#[cfg(test)]
mod tests {
    use irori_types::{Availability, Context, Origin, State, SwitchState};

    use super::*;

    fn entity(name: &str) -> EntityId {
        name.parse().expect("a valid entity id")
    }

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a valid timestamp")
    }

    fn now() -> Timestamp {
        Timestamp::from_jiff(jiff::Timestamp::now())
    }

    fn later(base: Timestamp, millis: i64) -> Timestamp {
        Timestamp::from_jiff(
            base.as_jiff()
                .checked_add(millis.milliseconds())
                .expect("the test timestamp stays in range"),
        )
    }

    fn sample(name: &str, on: bool, at: Timestamp) -> EntityState {
        EntityState {
            entity_id: entity(name),
            availability: Availability::Available,
            state: Some(State::Switch(SwitchState { on })),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9"
                    .parse()
                    .expect("a valid context id"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    fn open(dir: &tempfile::TempDir) -> Recorder {
        Recorder::open(&dir.path().join("irori.db"), DEFAULT_RETAIN_DAYS)
            .expect("the recorder opens")
    }

    #[test]
    fn a_change_is_there_after_the_recorder_reopens() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let when = now();
        let plug = entity("switch.plug");
        {
            let recorder = Recorder::open(&path, DEFAULT_RETAIN_DAYS).expect("open");
            recorder.append(sample("switch.plug", true, when));
            let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].state, Some(State::Switch(SwitchState { on: true })));
            assert!(recorder.check());
        }
        let recorder = Recorder::open(&path, DEFAULT_RETAIN_DAYS).expect("reopen");
        let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
        assert_eq!(seen.len(), 1, "the change survived the restart");
        assert_eq!(seen[0].last_updated, when);
        assert!(recorder.check());
    }

    #[test]
    fn changes_come_back_oldest_first_and_only_for_that_entity() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let recorder = open(&dir);
        let plug = entity("switch.plug");
        let first = now();
        recorder.append(sample("switch.plug", true, first));
        recorder.append(sample("switch.plug", false, later(first, 1)));
        recorder.append(sample("switch.other", true, later(first, 2)));
        let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].state, Some(State::Switch(SwitchState { on: true })));
        assert_eq!(
            seen[1].state,
            Some(State::Switch(SwitchState { on: false }))
        );
        assert!(
            recorder
                .since(&entity("switch.nobody"), at("1970-01-01T00:00:00Z"))
                .is_empty()
        );
    }

    #[test]
    fn a_read_can_ask_for_only_the_recent_part() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let recorder = open(&dir);
        let plug = entity("switch.plug");
        let first = now();
        let second = later(first, 5_000);
        recorder.append(sample("switch.plug", true, first));
        recorder.append(sample("switch.plug", false, second));
        let seen = recorder.since(&plug, second);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].last_updated, second);
    }

    #[test]
    fn forgetting_an_entity_removes_its_rows_and_they_stay_gone() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let plug = entity("switch.plug");
        let other = entity("switch.other");
        let when = now();
        {
            let recorder = Recorder::open(&path, DEFAULT_RETAIN_DAYS).expect("open");
            recorder.append(sample("switch.plug", true, when));
            recorder.append(sample("switch.other", false, when));
            recorder.forget(&plug);
            assert!(recorder.since(&plug, at("1970-01-01T00:00:00Z")).is_empty());
            assert_eq!(recorder.since(&other, at("1970-01-01T00:00:00Z")).len(), 1);
        }
        let recorder = Recorder::open(&path, DEFAULT_RETAIN_DAYS).expect("reopen");
        assert!(recorder.since(&plug, at("1970-01-01T00:00:00Z")).is_empty());
        assert_eq!(recorder.since(&other, at("1970-01-01T00:00:00Z")).len(), 1);
    }

    #[test]
    fn a_read_keeps_the_newest_when_there_are_more_than_the_cap() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let recorder = open(&dir);
        let plug = entity("switch.plug");
        let start = now();
        let total = MAX_RETURNED + 10;
        for i in 0..total {
            recorder.append(sample(
                "switch.plug",
                true,
                later(start, i64::try_from(i).expect("the index fits")),
            ));
            if i % 100 == 0 {
                assert!(recorder.check(), "the writer is still accepting changes");
            }
        }
        let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
        assert_eq!(seen.len(), MAX_RETURNED);
        let oldest_kept = later(
            start,
            i64::try_from(total - MAX_RETURNED).expect("the index fits"),
        );
        assert_eq!(seen[0].last_updated, oldest_kept);
        assert_eq!(
            seen.last().expect("the cap is not zero").last_updated,
            later(start, i64::try_from(total - 1).expect("the index fits"))
        );
    }

    #[test]
    fn rows_older_than_retention_are_gone_on_the_next_open() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let plug = entity("switch.plug");
        {
            let recorder = Recorder::open(&path, 1).expect("open");
            recorder.append(sample("switch.plug", true, at("2020-01-01T00:00:00Z")));
            recorder.append(sample("switch.plug", false, now()));
            // Both are stored. Retention is applied when the file is opened, which is the
            // restart, and once an hour while running.
            assert_eq!(recorder.since(&plug, at("1970-01-01T00:00:00Z")).len(), 2);
        }
        let recorder = Recorder::open(&path, 1).expect("reopen");
        let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
        assert_eq!(seen.len(), 1, "the 2020 reading was past one day");
        assert_ne!(seen[0].last_updated, at("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn a_row_that_does_not_parse_is_skipped() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder = Recorder::open(&path, DEFAULT_RETAIN_DAYS).expect("open");
        let when = now();
        recorder.append(sample("switch.plug", true, when));
        let planted = Connection::open(&path).expect("a second connection");
        planted
            .execute(
                "INSERT INTO state_history (entity_id, updated_ns, state) VALUES (?1, ?2, ?3)",
                (
                    "switch.plug",
                    nanos(when).expect("the timestamp fits"),
                    "not json",
                ),
            )
            .expect("the bad row is planted");
        let seen = recorder.since(&entity("switch.plug"), at("1970-01-01T00:00:00Z"));
        assert_eq!(seen.len(), 1, "the unreadable row is not a change");
        assert_eq!(seen[0].state, Some(State::Switch(SwitchState { on: true })));
    }

    #[test]
    fn zero_days_of_retention_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let error = Recorder::open(&dir.path().join("irori.db"), 0).expect_err("refused");
        assert_eq!(error.to_string(), "retain_days must be at least 1");
    }
}
