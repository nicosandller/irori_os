//! Entity history on disk, in the home's `irori.db`.
//!
//! Each real change is appended to `state_history`, so a later start still has it. A sensor
//! that measures or counts also gets a five-minute summary, kept as long as the raw changes,
//! and an hourly summary kept for longer — forever, unless the home says otherwise. One thread
//! owns the connection. Everyone else sends it a message — save, forget, read, or stop — and
//! the messages are applied in the order they were sent, so a read sees the saves that were
//! asked for before it. A burst of saves shares one transaction. When too many saves are
//! waiting, a new change is dropped and counted rather than making the event loop wait.
//! Forgetting an entity and reading its history are not dropped, and they are not stuck behind
//! a full save queue: a missed sample is a gap, and a forgotten delete would leave rows for
//! something that has left the home.
//!
//! Automation traces are not stored here. The Automations extension keeps its own runs.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use irori_types::{EntityId, EntityState, StateClass, Timestamp};
use rusqlite::Connection;

mod stats;

pub use stats::SummaryPoint;

#[cfg(test)]
use jiff::ToSpan as _;

/// How long detailed history is kept when a home does not say otherwise. Ten days, which is
/// Home Assistant's raw window: the changes themselves, and the five-minute summaries.
pub const DEFAULT_RETAIN_DAYS: u32 = 10;

/// The most changes one read returns. The newest are kept, and they come back oldest first.
pub const MAX_RETURNED: usize = 2_000;

/// Saves waiting on the writer. Past this, new changes are dropped.
const QUEUE: usize = 1_024;

/// Saves written in one transaction.
const BURST: usize = 64;

/// How often rows older than the retention window are deleted while the process is up.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// How often closed periods are summarized while the process is up.
const COMPILE_EVERY: Duration = Duration::from_secs(30);

/// Closed periods summarized on one ordinary turn. A restart catches up over many turns, so
/// opening the database does not wait to summarize the whole window.
const COMPILE_BUDGET: usize = 64;

/// Closed periods summarized before answering a summary read. Ten days of five-minute periods
/// is 2880, so one read finishes a sensor that is still inside the detailed window.
const SUMMARY_BUDGET: usize = 4_096;

/// How long to wait before trying a batch again after the write failed.
const RETRY_WRITE: Duration = Duration::from_secs(1);

/// How long this connection waits when another one holds the file. The same wait the rest of
/// the process uses: the timeout belongs to the waiter.
const BUSY: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS state_history (
    id INTEGER PRIMARY KEY,
    entity_id TEXT NOT NULL,
    updated_ns INTEGER NOT NULL,
    state TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS state_history_entity_time
    ON state_history (entity_id, updated_ns, id);

CREATE TABLE IF NOT EXISTS statistic_entities (
    entity_id TEXT PRIMARY KEY,
    class TEXT NOT NULL,
    compiled_until_ns INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS statistics_short_term (
    entity_id TEXT NOT NULL,
    start_ns INTEGER NOT NULL,
    mean REAL,
    min REAL,
    max REAL,
    sum REAL,
    last REAL,
    PRIMARY KEY (entity_id, start_ns)
);
CREATE TABLE IF NOT EXISTS statistics (
    entity_id TEXT NOT NULL,
    start_ns INTEGER NOT NULL,
    mean REAL,
    min REAL,
    max REAL,
    sum REAL,
    last REAL,
    PRIMARY KEY (entity_id, start_ns)
);
";

/// How long history is kept. `retain_days` covers the changes and the five-minute summaries.
/// `summary_days` covers the hourly summaries; `None` keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub retain_days: u32,
    pub summary_days: Option<u32>,
}

impl Retention {
    /// `retain_days` is at least 1. `summary_days`, when set, is at least 1 and at least
    /// `retain_days`: the hourly rows are not deleted while the changes they came from are
    /// still kept. Leave `summary_days` unset to keep the hourly rows.
    pub fn new(retain_days: u32, summary_days: Option<u32>) -> Result<Self, Error> {
        if retain_days == 0 {
            return Err(Error::new("retain_days must be at least 1"));
        }
        if let Some(days) = summary_days {
            if days == 0 {
                return Err(Error::new(
                    "summary_days must be at least 1, or be left unset to keep hourly summaries",
                ));
            }
            if days < retain_days {
                return Err(Error::new(
                    "summary_days must be at least retain_days. Leave it unset to keep hourly summaries",
                ));
            }
        }
        Ok(Self {
            retain_days,
            summary_days,
        })
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            retain_days: DEFAULT_RETAIN_DAYS,
            summary_days: None,
        }
    }
}

/// A change waiting to be written, and the statistic class it had when it was recorded.
struct Change {
    state: EntityState,
    class: Option<StateClass>,
}

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
    Append(Box<Change>),
    Forget(EntityId),
    /// Delete every row whose entity is not in this list. Sent when a listener fell behind and
    /// may have missed a removal.
    ForgetMissing(Vec<EntityId>),
    /// This entity is summarized. The class is recorded once; later notices do not change it.
    Notice {
        entity_id: EntityId,
        class: StateClass,
    },
    Query {
        entity_id: EntityId,
        since: Timestamp,
        reply: std::sync::mpsc::Sender<Vec<EntityState>>,
    },
    Summaries {
        entity_id: EntityId,
        since: Timestamp,
        reply: std::sync::mpsc::Sender<Vec<SummaryPoint>>,
    },
    /// Replace how long history is kept, summarize what a shorter window would otherwise
    /// delete, then delete.
    Configure {
        retention: Retention,
        reply: std::sync::mpsc::Sender<()>,
    },
    Check(std::sync::mpsc::Sender<bool>),
}

struct Queue {
    jobs: VecDeque<Job>,
    /// `Append` jobs in `jobs`. Other jobs do not count: a removal has to get in even when
    /// saves have filled the queue.
    appends: usize,
}

struct Mailbox {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// False once the writer has stopped, so a caller does not wait for an answer that will
    /// never come.
    alive: AtomicBool,
}

/// The diary. Cloning shares the writer thread. The thread stops when the last clone is dropped.
#[derive(Clone)]
pub struct Recorder {
    shared: Arc<Shared>,
}

struct Shared {
    mailbox: Arc<Mailbox>,
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
        // Wakes a thread that is waiting. The flag is what it sees if it is inside a batch.
        self.mailbox.wake.notify_all();
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle
            && let Err(error) = handle.join()
        {
            tracing::error!("the recorder thread panicked: {error:?}");
        }
    }
}

/// Answers anyone still waiting when the writer stops, including after a panic.
struct MarkDead(Arc<Mailbox>);

impl Drop for MarkDead {
    fn drop(&mut self) {
        let mut queue = lock(&self.0.queue);
        self.0.alive.store(false, Ordering::SeqCst);
        for job in queue.jobs.drain(..) {
            abandon(job);
        }
    }
}

fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abandon(job: Job) {
    match job {
        Job::Query { reply, .. } => {
            let _ = reply.send(Vec::new());
        }
        Job::Summaries { reply, .. } => {
            let _ = reply.send(Vec::new());
        }
        Job::Check(reply) => {
            let _ = reply.send(false);
        }
        Job::Configure { reply, .. } => {
            let _ = reply.send(());
        }
        Job::Append(_) | Job::Forget(_) | Job::ForgetMissing(_) | Job::Notice { .. } => {}
    }
}

impl Recorder {
    /// Opens `path` (the home's `irori.db`) and starts the writer. `retain_days` is at least 1.
    /// Hourly summaries are kept. Rows older than the detailed window are deleted before this
    /// returns, and about once an hour after, even while saves keep arriving. A sensor that is
    /// still being summarized keeps its rows until that summary has caught up.
    pub fn open(path: &Path, retain_days: u32) -> Result<Self, Error> {
        Self::open_keeping(path, Retention::new(retain_days, None)?)
    }

    /// Opens `path` and keeps history for `retention`.
    pub fn open_keeping(path: &Path, retention: Retention) -> Result<Self, Error> {
        Self::start(path, retention, PRUNE_EVERY)
    }

    /// `prune_every` is how often retention runs while the process is up. Tests pass a short one.
    #[cfg(test)]
    fn open_pruning_every(
        path: &Path,
        retain_days: u32,
        prune_every: Duration,
    ) -> Result<Self, Error> {
        Self::start(path, Retention::new(retain_days, None)?, prune_every)
    }

    fn start(path: &Path, retention: Retention, prune_every: Duration) -> Result<Self, Error> {
        let path = path.to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mailbox = Arc::new(Mailbox {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                appends: 0,
            }),
            wake: Condvar::new(),
            alive: AtomicBool::new(true),
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let mailbox_for_thread = Arc::clone(&mailbox);
        let shutdown_for_thread = Arc::clone(&shutdown);
        let handle = std::thread::Builder::new()
            .name("irori-recorder".to_owned())
            .spawn(move || {
                // Runs on the way out, including a panic, so a waiting read is not left hanging.
                let _mark_dead = MarkDead(Arc::clone(&mailbox_for_thread));
                match open_connection(&path) {
                    Ok(connection) => {
                        let mut last_write_ok = true;
                        if let Err(error) = prune(&connection, &retention) {
                            last_write_ok = false;
                            tracing::warn!(%error, "old history could not be deleted");
                        }
                        let _ = ready_tx.send(Ok(()));
                        serve(
                            connection,
                            &mailbox_for_thread,
                            &shutdown_for_thread,
                            retention,
                            prune_every,
                            last_write_ok,
                        );
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|error| Error::new(format!("could not start the recorder thread: {error}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shared: Arc::new(Shared {
                    mailbox,
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
    /// logged (the first time, and every thousand after). `class` is set for a sensor that
    /// measures or counts, and those are the sensors that get five-minute and hourly summaries.
    pub fn append(&self, state: EntityState) {
        self.append_with(state, None);
    }

    /// [`Self::append`] for a sensor whose `state_class` says it is summarized.
    pub fn append_with(&self, state: EntityState, class: Option<StateClass>) {
        {
            let mut queue = lock(&self.shared.mailbox.queue);
            if !self.shared.mailbox.alive.load(Ordering::SeqCst) {
                tracing::warn!("the recorder has stopped; a state change was not saved");
                return;
            }
            if queue.appends >= QUEUE {
                drop(queue);
                let dropped = self.shared.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 || dropped.is_multiple_of(1_000) {
                    tracing::warn!(
                        dropped,
                        "the recorder fell behind and dropped state changes"
                    );
                }
                return;
            }
            queue
                .jobs
                .push_back(Job::Append(Box::new(Change { state, class })));
            queue.appends += 1;
        }
        self.shared.mailbox.wake.notify_one();
    }

    /// Records that `entity_id` is summarized as `class`, even before its next change. The
    /// first class is the one that sticks.
    pub fn notice(&self, entity_id: &EntityId, class: StateClass) {
        self.enqueue(
            Job::Notice {
                entity_id: entity_id.clone(),
                class,
            },
            "a sensor was not marked for summaries",
        );
    }

    /// Changes how long history is kept, on the running writer. Summarizes what it can before
    /// a shorter window deletes the changes. Returns false when the writer has stopped.
    pub fn configure(&self, retention: Retention) -> bool {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if !self.enqueue_waiting(Job::Configure {
            retention,
            reply: reply_tx,
        }) {
            tracing::warn!("the recorder has stopped; retention was not changed");
            return false;
        }
        reply_rx.recv().is_ok()
    }

    /// Deletes every stored change for this entity. The delete is queued behind the saves
    /// already accepted, and it is not dropped when those saves have filled the queue.
    pub fn forget(&self, entity_id: &EntityId) {
        self.enqueue(
            Job::Forget(entity_id.clone()),
            "a removed entity's history was not deleted",
        );
    }

    /// Deletes history for every entity that is not in `live`. Same ordering rules as
    /// [`Self::forget`]: it lands after saves already accepted, and a full save queue does not
    /// drop it or make the caller wait.
    pub fn forget_missing(&self, live: &[EntityId]) {
        self.enqueue(
            Job::ForgetMissing(live.to_vec()),
            "history for entities that left could not be deleted",
        );
    }

    fn enqueue(&self, job: Job, stopped: &str) {
        {
            let mut queue = lock(&self.shared.mailbox.queue);
            if !self.shared.mailbox.alive.load(Ordering::SeqCst) {
                tracing::warn!("the recorder has stopped; {stopped}");
                return;
            }
            queue.jobs.push_back(job);
        }
        self.shared.mailbox.wake.notify_one();
    }

    /// Changes for `entity_id` with `last_updated >= since`, oldest first. At most
    /// [`MAX_RETURNED`], keeping the newest when there are more.
    pub fn since(&self, entity_id: &EntityId, since: Timestamp) -> Vec<EntityState> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if !self.enqueue_waiting(Job::Query {
            entity_id: entity_id.clone(),
            since,
            reply: reply_tx,
        }) {
            tracing::warn!("the recorder has stopped; history could not be read");
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }

    /// Hourly summaries for `entity_id` with `start >= since`, oldest first. Closed periods are
    /// summarized before the read, up to a few thousand of them, so a chart does not wait for
    /// the half-minute compile pass.
    pub fn summaries(&self, entity_id: &EntityId, since: Timestamp) -> Vec<SummaryPoint> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if !self.enqueue_waiting(Job::Summaries {
            entity_id: entity_id.clone(),
            since,
            reply: reply_tx,
        }) {
            tracing::warn!("the recorder has stopped; summaries could not be read");
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }

    /// Whether a trivial read works and the last write succeeded.
    pub fn check(&self) -> bool {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if !self.enqueue_waiting(Job::Check(reply_tx)) {
            return false;
        }
        reply_rx.recv().unwrap_or(false)
    }

    /// Queues `job` unless the writer has stopped. The caller holds the reply channel.
    fn enqueue_waiting(&self, job: Job) -> bool {
        {
            let mut queue = lock(&self.shared.mailbox.queue);
            if !self.shared.mailbox.alive.load(Ordering::SeqCst) {
                return false;
            }
            queue.jobs.push_back(job);
        }
        self.shared.mailbox.wake.notify_one();
        true
    }
}

fn open_connection(path: &Path) -> Result<Connection, Error> {
    let connection = Connection::open(path)
        .map_err(|error| Error::new(format!("failed to open {}: {error}", path.display())))?;
    connection
        .busy_timeout(BUSY)
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
    mailbox: &Mailbox,
    shutdown: &AtomicBool,
    mut retention: Retention,
    prune_every: Duration,
    mut last_write_ok: bool,
) {
    let mut last_prune = Instant::now();
    let mut last_compile = Instant::now();
    let mut batch = Vec::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            let rest = {
                let mut queue = lock(&mailbox.queue);
                queue.appends = 0;
                std::mem::take(&mut queue.jobs)
            };
            for job in rest {
                apply(
                    job,
                    &mut connection,
                    &mut batch,
                    &mut last_write_ok,
                    &mut retention,
                );
            }
            write_batch(&mut connection, &mut batch, &mut last_write_ok);
            break;
        }
        // Checked on every turn, not only after a quiet minute. A page that polls health, or
        // an entity that keeps changing, would otherwise never leave the queue idle long
        // enough for the old timeout path to delete anything.
        if last_compile.elapsed() >= COMPILE_EVERY {
            write_batch(&mut connection, &mut batch, &mut last_write_ok);
            note_compile(&mut connection, &batch, &mut last_write_ok, COMPILE_BUDGET);
            last_compile = Instant::now();
        }
        if last_prune.elapsed() >= prune_every {
            write_batch(&mut connection, &mut batch, &mut last_write_ok);
            // A prune that succeeds is a write, but it must not paint over a batch that just
            // failed and is waiting to be tried again.
            note_prune(&connection, &retention, &batch, &mut last_write_ok);
            last_prune = Instant::now();
        } else if !batch.is_empty() {
            write_batch(&mut connection, &mut batch, &mut last_write_ok);
        }
        let jobs = recv_burst(
            mailbox,
            shutdown,
            wait_for(last_prune, last_compile, prune_every, batch.is_empty()),
        );
        for job in jobs {
            apply(
                job,
                &mut connection,
                &mut batch,
                &mut last_write_ok,
                &mut retention,
            );
        }
        write_batch(&mut connection, &mut batch, &mut last_write_ok);
    }
}

fn note_compile(
    connection: &mut Connection,
    batch: &[Change],
    last_write_ok: &mut bool,
    budget: usize,
) {
    let Some(now_ns) = now_ns() else {
        return;
    };
    match stats::compile(connection, budget, None, now_ns) {
        Ok(()) if batch.is_empty() => *last_write_ok = true,
        Ok(()) => {}
        Err(error) => {
            *last_write_ok = false;
            tracing::warn!(%error, "history could not be summarized");
        }
    }
}

fn note_prune(
    connection: &Connection,
    retention: &Retention,
    batch: &[Change],
    last_write_ok: &mut bool,
) {
    match prune(connection, retention) {
        Ok(()) if batch.is_empty() => *last_write_ok = true,
        Ok(()) => {}
        Err(error) => {
            *last_write_ok = false;
            tracing::warn!(%error, "old history could not be deleted");
        }
    }
}

/// How long to sleep when nothing is queued. A failed batch is retried soon; otherwise sleep
/// until the next summary pass or the next retention pass, whichever is sooner.
fn wait_for(
    last_prune: Instant,
    last_compile: Instant,
    prune_every: Duration,
    idle: bool,
) -> Duration {
    let until_prune = prune_every.saturating_sub(last_prune.elapsed());
    let until_compile = COMPILE_EVERY.saturating_sub(last_compile.elapsed());
    let until = until_prune.min(until_compile);
    if idle { until } else { until.min(RETRY_WRITE) }
}

fn recv_burst(mailbox: &Mailbox, shutdown: &AtomicBool, wait_for: Duration) -> Vec<Job> {
    let mut queue = lock(&mailbox.queue);
    let started = Instant::now();
    while queue.jobs.is_empty() && !shutdown.load(Ordering::SeqCst) {
        let remaining = wait_for.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let (guard, waited) = mailbox
            .wake
            .wait_timeout(queue, remaining)
            .unwrap_or_else(PoisonError::into_inner);
        queue = guard;
        if waited.timed_out() {
            break;
        }
    }
    let mut jobs = Vec::new();
    while jobs.len() < BURST {
        let Some(job) = queue.jobs.pop_front() else {
            break;
        };
        if matches!(job, Job::Append(_)) {
            queue.appends = queue.appends.saturating_sub(1);
        }
        jobs.push(job);
    }
    jobs
}

fn apply(
    job: Job,
    connection: &mut Connection,
    batch: &mut Vec<Change>,
    last_write_ok: &mut bool,
    retention: &mut Retention,
) {
    match job {
        Job::Append(change) => batch.push(*change),
        Job::Notice { entity_id, class } => {
            let Some(now) = now_ns() else {
                return;
            };
            note_write(
                last_write_ok,
                stats::note_class(connection, &entity_id, class, now),
            );
        }
        Job::Forget(entity_id) => {
            // Drop it from the batch too. A failed write keeps the batch, and writing it later
            // would put the removed entity back.
            batch.retain(|change| change.state.entity_id != entity_id);
            write_batch(connection, batch, last_write_ok);
            note_write(last_write_ok, forget(connection, &entity_id));
        }
        Job::ForgetMissing(live) => {
            let keep: HashSet<&EntityId> = live.iter().collect();
            batch.retain(|change| keep.contains(&change.state.entity_id));
            write_batch(connection, batch, last_write_ok);
            note_write(last_write_ok, forget_missing(connection, &live));
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
        Job::Summaries {
            entity_id,
            since,
            reply,
        } => {
            write_batch(connection, batch, last_write_ok);
            let points = match summarize(connection, &entity_id, since, batch, last_write_ok) {
                Ok(points) => points,
                Err(error) => {
                    tracing::warn!(%error, "summaries could not be read");
                    Vec::new()
                }
            };
            let _ = reply.send(points);
        }
        Job::Configure {
            retention: next,
            reply,
        } => {
            *retention = next;
            write_batch(connection, batch, last_write_ok);
            // Compile before deleting, so a shorter window still turns the changes it is
            // about to drop into hourly rows.
            note_compile(connection, batch, last_write_ok, SUMMARY_BUDGET);
            note_prune(connection, retention, batch, last_write_ok);
            let _ = reply.send(());
        }
        Job::Check(reply) => {
            let readable = connection
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .is_ok();
            let _ = reply.send(*last_write_ok && readable);
        }
    }
}

fn summarize(
    connection: &mut Connection,
    entity_id: &EntityId,
    since: Timestamp,
    batch: &[Change],
    last_write_ok: &mut bool,
) -> Result<Vec<SummaryPoint>, String> {
    if let Some(now) = now_ns() {
        match stats::compile(connection, SUMMARY_BUDGET, Some(entity_id), now) {
            Ok(()) if batch.is_empty() => *last_write_ok = true,
            Ok(()) => {}
            Err(error) => {
                *last_write_ok = false;
                return Err(error);
            }
        }
    }
    let Some(since_ns) = nanos(since) else {
        return Ok(Vec::new());
    };
    stats::summaries(connection, entity_id, since_ns)
}

fn write_batch(connection: &mut Connection, batch: &mut Vec<Change>, last_write_ok: &mut bool) {
    if batch.is_empty() {
        return;
    }
    // Keep the batch when the commit fails. The next turn tries it again. Clearing here would
    // drop changes the queue had already accepted, which is not the full-queue drop policy.
    match insert_many(connection, batch) {
        Ok(()) => {
            *last_write_ok = true;
            batch.clear();
        }
        Err(error) => {
            *last_write_ok = false;
            tracing::warn!(%error, "history could not be written");
        }
    }
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

fn insert_many(connection: &mut Connection, changes: &[Change]) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    {
        let mut statement = transaction
            .prepare("INSERT INTO state_history (entity_id, updated_ns, state) VALUES (?1, ?2, ?3)")
            .map_err(|error| error.to_string())?;
        for change in changes {
            let state = &change.state;
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
    let Some(now) = now_ns() else {
        return transaction.commit().map_err(|error| error.to_string());
    };
    for change in changes {
        if let Some(class) = change.class {
            stats::note_class(&transaction, &change.state.entity_id, class, now)?;
        }
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn forget(connection: &Connection, entity_id: &EntityId) -> Result<(), String> {
    for table in [
        "state_history",
        "statistics_short_term",
        "statistics",
        "statistic_entities",
    ] {
        delete_entity(connection, table, entity_id)?;
    }
    Ok(())
}

fn delete_entity(connection: &Connection, table: &str, entity_id: &EntityId) -> Result<(), String> {
    let sql = match table {
        "state_history" => "DELETE FROM state_history WHERE entity_id = ?1",
        "statistics_short_term" => "DELETE FROM statistics_short_term WHERE entity_id = ?1",
        "statistics" => "DELETE FROM statistics WHERE entity_id = ?1",
        "statistic_entities" => "DELETE FROM statistic_entities WHERE entity_id = ?1",
        _ => return Err(format!("unknown history table {table}")),
    };
    connection
        .execute(sql, [entity_id.as_str()])
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Deletes rows for entities that are not in `live`. A temp table, so a large home is not
/// limited by SQLite's bound-variable cap. An empty `live` deletes every row: nothing in the
/// home means nothing to keep.
fn forget_missing(connection: &mut Connection, live: &[EntityId]) -> Result<(), String> {
    if live.is_empty() {
        connection
            .execute_batch(
                "DELETE FROM state_history;
                 DELETE FROM statistics_short_term;
                 DELETE FROM statistics;
                 DELETE FROM statistic_entities",
            )
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS history_live (entity_id TEXT PRIMARY KEY);
             DELETE FROM history_live",
        )
        .map_err(|error| error.to_string())?;
    {
        let mut statement = transaction
            .prepare("INSERT INTO history_live (entity_id) VALUES (?1)")
            .map_err(|error| error.to_string())?;
        for id in live {
            statement
                .execute([id.as_str()])
                .map_err(|error| error.to_string())?;
        }
    }
    for sql in [
        "DELETE FROM state_history
         WHERE entity_id NOT IN (SELECT entity_id FROM history_live)",
        "DELETE FROM statistics_short_term
         WHERE entity_id NOT IN (SELECT entity_id FROM history_live)",
        "DELETE FROM statistics
         WHERE entity_id NOT IN (SELECT entity_id FROM history_live)",
        "DELETE FROM statistic_entities
         WHERE entity_id NOT IN (SELECT entity_id FROM history_live)",
    ] {
        transaction
            .execute(sql, [])
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
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

fn prune(connection: &Connection, retention: &Retention) -> Result<(), String> {
    let Some(cutoff) = cutoff_ns(retention.retain_days) else {
        // A span this long does not fit in a timestamp. Keeping the rows is the safe failure.
        tracing::warn!(
            retain_days = retention.retain_days,
            "history retention is too long to apply"
        );
        return Ok(());
    };
    prune_before(connection, retention, cutoff)
}

/// Deletes rows older than `cutoff` nanoseconds. `cutoff` is passed in so a test can put an
/// hour across it; [`prune`] is what the writer calls, with the cutoff taken from the clock.
fn prune_before(connection: &Connection, retention: &Retention, cutoff: i64) -> Result<(), String> {
    // A sensor that has not been summarized through the cutoff keeps its raw rows. Deleting
    // them first would leave a hole in the hourly history that nothing can fill back in.
    connection
        .execute(
            "DELETE FROM state_history
             WHERE updated_ns < ?1
               AND entity_id NOT IN (
                   SELECT entity_id FROM statistic_entities WHERE compiled_until_ns < ?1
               )",
            [cutoff],
        )
        .map_err(|error| error.to_string())?;
    // Five-minute rows of an hour that is still open have to stay. The hour is written only
    // when its last period is compiled, and compilation can already be past the cutoff while
    // that hour is unfinished. Deleting those rows would make the hour short, and it is never
    // compiled again. A finished hour starts before `compiled_until` aligned down to the hour.
    connection
        .execute(
            "DELETE FROM statistics_short_term
             WHERE start_ns < ?1
               AND (
                   entity_id NOT IN (SELECT entity_id FROM statistic_entities)
                   OR start_ns < (
                       SELECT compiled_until_ns - (compiled_until_ns % ?2)
                       FROM statistic_entities
                       WHERE statistic_entities.entity_id = statistics_short_term.entity_id
                   )
               )",
            rusqlite::params![cutoff, stats::HOUR_NS],
        )
        .map_err(|error| error.to_string())?;
    if let Some(days) = retention.summary_days {
        let Some(summary_cutoff) = cutoff_ns(days) else {
            tracing::warn!(days, "summary retention is too long to apply");
            return Ok(());
        };
        connection
            .execute(
                "DELETE FROM statistics WHERE start_ns < ?1",
                [summary_cutoff],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn now_ns() -> Option<i64> {
    i64::try_from(jiff::Timestamp::now().as_nanosecond()).ok()
}

fn cutoff_ns(retain_days: u32) -> Option<i64> {
    // A timestamp has no calendar, so a day here is 24 hours. `Span::days` is refused on a
    // timestamp for that reason. `try_hours` instead of `hours`: a `u32` of days can ask for
    // more hours than a span can hold, and the panicking constructor would take the writer
    // thread down before the database was ready.
    let hours = i64::from(retain_days).checked_mul(24)?;
    let span = jiff::Span::new().try_hours(hours).ok()?;
    let cutoff = jiff::Timestamp::now().checked_sub(span).ok()?;
    i64::try_from(cutoff.as_nanosecond()).ok()
}

fn nanos(timestamp: Timestamp) -> Option<i64> {
    i64::try_from(timestamp.as_jiff().as_nanosecond()).ok()
}

#[cfg(test)]
mod tests {
    use irori_types::{Availability, Context, Origin, State, StateClass, SwitchState};

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

    #[test]
    fn retention_runs_while_saves_keep_arriving() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder =
            Recorder::open_pruning_every(&path, 1, Duration::from_millis(200)).expect("open");
        let plug = entity("switch.plug");
        recorder.append(sample("switch.plug", true, at("2020-01-01T00:00:00Z")));
        recorder.append(sample("switch.plug", false, now()));
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            // A check is a job. The old prune ran only after a minute with no jobs at all.
            assert!(recorder.check(), "the writer is still up");
            std::thread::sleep(Duration::from_millis(20));
        }
        let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
        assert_eq!(
            seen.len(),
            1,
            "the 2020 reading was deleted while the writer was busy"
        );
        assert_ne!(seen[0].last_updated, at("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn a_retention_longer_than_a_span_keeps_the_rows() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder = Recorder::open(&path, u32::MAX)
            .expect("a span that does not fit does not stop the writer");
        let when = now();
        recorder.append(sample("switch.plug", true, when));
        let seen = recorder.since(&entity("switch.plug"), at("1970-01-01T00:00:00Z"));
        assert_eq!(seen.len(), 1, "rows stay when the cutoff cannot be built");
        assert!(recorder.check());
    }

    #[test]
    fn entities_that_are_no_longer_in_the_home_lose_their_rows() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let recorder = open(&dir);
        let when = now();
        let plug = entity("switch.plug");
        let other = entity("switch.other");
        recorder.append(sample("switch.plug", true, when));
        recorder.append(sample("switch.other", false, when));
        recorder.forget_missing(std::slice::from_ref(&other));
        assert!(recorder.since(&plug, at("1970-01-01T00:00:00Z")).is_empty());
        assert_eq!(recorder.since(&other, at("1970-01-01T00:00:00Z")).len(), 1);
    }

    #[test]
    fn forgetting_does_not_wait_for_a_full_save_queue() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder = Recorder::open(&path, DEFAULT_RETAIN_DAYS).expect("open");
        // Holds the write lock so the writer blocks inside its first save. The save queue can
        // then fill without the writer draining it.
        let held = Connection::open(&path).expect("a second connection");
        held.execute_batch("BEGIN EXCLUSIVE")
            .expect("the file can be locked");
        let when = now();
        let plug = entity("switch.plug");
        recorder.append(sample("switch.plug", true, when));
        for index in 0..QUEUE {
            recorder.append(sample(
                "switch.plug",
                true,
                later(when, i64::try_from(index + 1).expect("the index fits")),
            ));
        }
        let started = Instant::now();
        recorder.forget(&plug);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "forget waited on the save queue: {:?}",
            started.elapsed()
        );
        drop(held);
        assert!(
            recorder.since(&plug, at("1970-01-01T00:00:00Z")).is_empty(),
            "the delete still landed after the saves that were already queued"
        );
    }

    fn at_ns(ns: i64) -> Timestamp {
        Timestamp::from_jiff(
            jiff::Timestamp::from_nanosecond(i128::from(ns)).expect("the timestamp fits"),
        )
    }

    fn reading(name: &str, value: f64, at: Timestamp) -> EntityState {
        EntityState {
            entity_id: entity(name),
            availability: Availability::Available,
            state: Some(State::Sensor(irori_types::SensorState {
                value: irori_types::SensorValue::Number(value),
            })),
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

    /// An hour that closed three days ago, aligned to the clock.
    fn three_days_ago_hour() -> i64 {
        let now = now_ns().expect("now fits in a timestamp");
        stats::align_down(now, stats::HOUR_NS) - 3 * 24 * stats::HOUR_NS
    }

    #[test]
    fn hourly_summaries_outlive_the_detailed_history() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder =
            Recorder::open_keeping(&path, Retention::new(1, None).expect("one day")).expect("open");
        let sensor = entity("sensor.temperature");
        let old = three_days_ago_hour();
        recorder.append_with(
            reading("sensor.temperature", 21.5, at_ns(old)),
            Some(irori_types::StateClass::Measurement),
        );
        recorder.append_with(
            reading("sensor.temperature", 22.0, now()),
            Some(irori_types::StateClass::Measurement),
        );
        let points = recorder.summaries(&sensor, at("1970-01-01T00:00:00Z"));
        let kept = points
            .iter()
            .find(|point| nanos(point.start) == Some(old))
            .expect("the hour from three days ago was summarized");
        let difference = (kept.value - 21.5).abs();
        assert!(
            difference < 1e-6,
            "the hour averaged 21.5, got {}",
            kept.value
        );
        assert!(
            recorder.configure(Retention::new(1, None).expect("one day")),
            "retention was applied"
        );
        let seen = recorder.since(&sensor, at("1970-01-01T00:00:00Z"));
        assert!(
            seen.iter().all(|state| state.last_updated != at_ns(old)),
            "the raw reading from three days ago was deleted"
        );
        assert!(
            !seen.is_empty(),
            "the reading from now is still inside the day"
        );
        let after = recorder.summaries(&sensor, at("1970-01-01T00:00:00Z"));
        assert!(
            after.iter().any(|point| nanos(point.start) == Some(old)),
            "the hourly row stays when summaries are kept"
        );
        let short: i64 = Connection::open(&path)
            .expect("a second connection")
            .query_row(
                "SELECT COUNT(*) FROM statistics_short_term WHERE start_ns = ?1",
                [old],
                |row| row.get(0),
            )
            .expect("the short-term table can be counted");
        assert_eq!(
            short, 0,
            "five-minute rows are deleted with the raw changes"
        );
    }

    #[test]
    fn hourly_summaries_can_be_kept_for_a_limited_number_of_days() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder = Recorder::open_keeping(&path, Retention::new(1, Some(1)).expect("one day"))
            .expect("open");
        let sensor = entity("sensor.energy");
        let old = three_days_ago_hour();
        recorder.append_with(
            reading("sensor.energy", 100.0, at_ns(old)),
            Some(irori_types::StateClass::TotalIncreasing),
        );
        recorder.append_with(
            reading("sensor.energy", 110.0, now()),
            Some(irori_types::StateClass::TotalIncreasing),
        );
        let before = recorder.summaries(&sensor, at("1970-01-01T00:00:00Z"));
        assert!(
            before.iter().any(|point| nanos(point.start) == Some(old)),
            "the old hour was summarized before retention deleted it"
        );
        assert!(recorder.configure(Retention::new(1, Some(1)).expect("one day")));
        let after = recorder.summaries(&sensor, at("1970-01-01T00:00:00Z"));
        assert!(
            after.iter().all(|point| nanos(point.start) != Some(old)),
            "an hourly row older than summary_days is deleted"
        );
    }

    #[test]
    fn changing_retention_deletes_old_rows_without_a_restart() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let recorder = Recorder::open(&dir.path().join("irori.db"), 10).expect("open");
        let plug = entity("switch.plug");
        recorder.append(sample("switch.plug", true, at("2020-01-01T00:00:00Z")));
        recorder.append(sample("switch.plug", false, now()));
        assert_eq!(recorder.since(&plug, at("1970-01-01T00:00:00Z")).len(), 2);
        assert!(recorder.configure(Retention::new(1, None).expect("one day")));
        let seen = recorder.since(&plug, at("1970-01-01T00:00:00Z"));
        assert_eq!(
            seen.len(),
            1,
            "the 2020 reading was dropped when retention shrank"
        );
        assert_ne!(seen[0].last_updated, at("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn a_sensor_with_no_history_yet_is_still_marked_for_summaries() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let recorder = Recorder::open(&path, 10).expect("open");
        let sensor = entity("sensor.temp");
        recorder.notice(&sensor, StateClass::Measurement);
        assert!(
            recorder.check(),
            "noting a sensor that has not reported yet is not a failed write"
        );
        let connection = rusqlite::Connection::open(&path).expect("the file opens");
        connection
            .busy_timeout(BUSY)
            .expect("the second connection can wait");
        let class: String = connection
            .query_row(
                "SELECT class FROM statistic_entities WHERE entity_id = ?1",
                [sensor.as_str()],
                |row| row.get(0),
            )
            .expect("the sensor is marked");
        assert_eq!(class, "measurement");
    }

    #[test]
    fn summary_retention_cannot_be_shorter_than_the_detailed_history() {
        assert!(Retention::new(0, None).is_err());
        assert!(Retention::new(10, Some(0)).is_err());
        assert!(Retention::new(10, Some(9)).is_err());
        assert_eq!(
            Retention::new(10, Some(10)).expect("equal windows are allowed"),
            Retention {
                retain_days: 10,
                summary_days: Some(10),
            }
        );
        assert!(
            Retention::new(10, None)
                .expect("forever")
                .summary_days
                .is_none()
        );
    }

    #[test]
    fn forgetting_a_sensor_removes_its_summaries() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let recorder = open(&dir);
        let sensor = entity("sensor.temperature");
        let old = three_days_ago_hour();
        recorder.append_with(
            reading("sensor.temperature", 21.5, at_ns(old)),
            Some(irori_types::StateClass::Measurement),
        );
        assert!(
            !recorder
                .summaries(&sensor, at("1970-01-01T00:00:00Z"))
                .is_empty()
        );
        recorder.forget(&sensor);
        assert!(
            recorder
                .summaries(&sensor, at("1970-01-01T00:00:00Z"))
                .is_empty()
        );
    }

    fn memory() -> Connection {
        let connection = Connection::open_in_memory().expect("a memory database");
        connection
            .execute_batch(SCHEMA)
            .expect("the history tables");
        connection
    }

    fn plant(connection: &Connection, state: &EntityState) {
        connection
            .execute(
                "INSERT INTO state_history (entity_id, updated_ns, state) VALUES (?1, ?2, ?3)",
                (
                    state.entity_id.as_str(),
                    nanos(state.last_updated).expect("the timestamp fits"),
                    serde_json::to_string(state).expect("the state can be stored"),
                ),
            )
            .expect("the row is planted");
    }

    fn unavailable(name: &str, value: f64, at: Timestamp) -> EntityState {
        let mut state = reading(name, value, at);
        state.availability = Availability::Unavailable;
        state
    }

    fn short_sum(connection: &Connection, entity_id: &EntityId, start: i64) -> f64 {
        connection
            .query_row(
                "SELECT sum FROM statistics_short_term WHERE entity_id = ?1 AND start_ns = ?2",
                (entity_id.as_str(), start),
                |row| row.get::<_, Option<f64>>(0),
            )
            .expect("the five-minute row was written")
            .expect("the counter's change was recorded")
    }

    #[test]
    fn a_counter_keeps_its_change_when_unavailable_crosses_a_compile() {
        let mut connection = memory();
        let sensor = entity("sensor.energy");
        plant(&connection, &reading("sensor.energy", 100.0, at_ns(1_000)));
        stats::note_class(&connection, &sensor, StateClass::TotalIncreasing, 1_000)
            .expect("the sensor is marked");
        stats::compile(&mut connection, 8, Some(&sensor), stats::FIVE_MIN_NS).expect("period 0");

        // The outage is the newest row when the next compile starts, so the baseline is not
        // the single latest raw row.
        plant(
            &connection,
            &unavailable("sensor.energy", 100.0, at_ns(stats::FIVE_MIN_NS + 1_000)),
        );
        stats::compile(&mut connection, 8, Some(&sensor), 2 * stats::FIVE_MIN_NS)
            .expect("period 1");

        plant(
            &connection,
            &reading(
                "sensor.energy",
                130.0,
                at_ns(2 * stats::FIVE_MIN_NS + 1_000),
            ),
        );
        stats::compile(&mut connection, 8, Some(&sensor), 3 * stats::FIVE_MIN_NS)
            .expect("period 2");

        let sum = short_sum(&connection, &sensor, 2 * stats::FIVE_MIN_NS);
        assert!(
            (sum - 30.0).abs() < 1e-6,
            "the change across the outage was {sum}, not 30"
        );
    }

    #[test]
    fn a_counter_uses_the_compiled_last_when_the_raw_reading_is_gone() {
        let mut connection = memory();
        let sensor = entity("sensor.energy");
        plant(&connection, &reading("sensor.energy", 100.0, at_ns(1_000)));
        stats::note_class(&connection, &sensor, StateClass::TotalIncreasing, 1_000)
            .expect("the sensor is marked");
        stats::compile(&mut connection, 8, Some(&sensor), stats::FIVE_MIN_NS).expect("period 0");
        connection
            .execute("DELETE FROM state_history", [])
            .expect("retention removed the raw rows");

        plant(
            &connection,
            &reading("sensor.energy", 140.0, at_ns(stats::FIVE_MIN_NS + 1_000)),
        );
        stats::compile(&mut connection, 8, Some(&sensor), 2 * stats::FIVE_MIN_NS)
            .expect("period 1");

        let sum = short_sum(&connection, &sensor, stats::FIVE_MIN_NS);
        assert!(
            (sum - 40.0).abs() < 1e-6,
            "the change from the stored last was {sum}, not 40"
        );
    }

    #[test]
    fn a_counter_uses_the_hourly_last_when_the_finer_rows_are_gone() {
        let mut connection = memory();
        let sensor = entity("sensor.energy");
        // The raw rows and the five-minute rows are gone. The hour still stores the last
        // reading, and the next compile starts after that hour.
        connection
            .execute(
                "INSERT INTO statistic_entities (entity_id, class, compiled_until_ns)
                 VALUES ('sensor.energy', 'total_increasing', ?1)",
                [stats::HOUR_NS],
            )
            .expect("the sensor is marked");
        connection
            .execute(
                "INSERT INTO statistics (entity_id, start_ns, mean, min, max, sum, last)
                 VALUES ('sensor.energy', 0, NULL, NULL, NULL, 50.0, 50.0)",
                [],
            )
            .expect("the hour stores the last reading");
        plant(
            &connection,
            &reading("sensor.energy", 80.0, at_ns(stats::HOUR_NS + 1_000)),
        );
        stats::compile(
            &mut connection,
            8,
            Some(&sensor),
            stats::HOUR_NS + stats::FIVE_MIN_NS,
        )
        .expect("the next five minutes");

        let sum = short_sum(&connection, &sensor, stats::HOUR_NS);
        assert!(
            (sum - 30.0).abs() < 1e-6,
            "the change from the hourly last was {sum}, not 30"
        );
    }

    #[test]
    fn prune_keeps_five_minute_rows_of_an_hour_that_is_not_finished() {
        let connection = memory();
        let hour = 10 * stats::HOUR_NS;
        // Twenty minutes into the hour: past the cutoff, still before the hour is rolled up.
        let cutoff = hour + 3 * stats::FIVE_MIN_NS;
        let compiled_until = hour + 4 * stats::FIVE_MIN_NS;
        let finished = hour + stats::HOUR_NS;
        connection
            .execute(
                "INSERT INTO statistic_entities (entity_id, class, compiled_until_ns)
                 VALUES ('sensor.open', 'total_increasing', ?1),
                        ('sensor.done', 'total_increasing', ?2)",
                (compiled_until, finished),
            )
            .expect("the sensors are marked");
        for (id, start, sum) in [
            ("sensor.open", hour - stats::FIVE_MIN_NS, 1.0),
            ("sensor.open", hour, 2.0),
            ("sensor.open", hour + 2 * stats::FIVE_MIN_NS, 3.0),
            ("sensor.open", hour + 4 * stats::FIVE_MIN_NS, 4.0),
            ("sensor.done", hour, 9.0),
        ] {
            connection
                .execute(
                    "INSERT INTO statistics_short_term
                        (entity_id, start_ns, mean, min, max, sum, last)
                     VALUES (?1, ?2, NULL, NULL, NULL, ?3, ?3)",
                    (id, start, sum),
                )
                .expect("a five-minute row");
        }

        prune_before(
            &connection,
            &Retention::new(1, None).expect("one day"),
            cutoff,
        )
        .expect("prune");

        let mut listed = connection
            .prepare(
                "SELECT start_ns FROM statistics_short_term
                 WHERE entity_id = 'sensor.open' ORDER BY start_ns",
            )
            .expect("the table can be read");
        let kept: Vec<i64> = listed
            .query_map([], |row| row.get(0))
            .expect("the rows can be listed")
            .collect::<Result<Vec<_>, _>>()
            .expect("every row parses");
        assert_eq!(
            kept,
            vec![
                hour,
                hour + 2 * stats::FIVE_MIN_NS,
                hour + 4 * stats::FIVE_MIN_NS
            ],
            "the open hour stays, including the periods already past the cutoff"
        );
        let done: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM statistics_short_term WHERE entity_id = 'sensor.done'",
                [],
                |row| row.get(0),
            )
            .expect("the finished sensor can be counted");
        assert_eq!(done, 0, "a finished hour's five-minute rows are deleted");
    }
}
