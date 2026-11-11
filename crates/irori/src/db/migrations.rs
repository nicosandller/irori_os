//! The versions of `irori.db`, oldest first, and what turns each into the next.
//!
//! The file says which version it is in `PRAGMA user_version`. A new file is brought to the
//! latest version when it is first opened; an older one is brought up from where it is; one newer
//! than this build is refused (`super::open`), because this build can't tell what the newer one
//! wrote or what it would lose by writing beside it.
//!
//! **Changing the database is one step: add a [`Migration`] to the end of [`MIGRATIONS`]**, with
//! the next number, and say in `what` what it does. Never edit one that has shipped: a home that
//! already ran it won't run it again, so the edit would only reach new homes. A migration runs in
//! the same transaction as the version it moves to, so it happens whole or not at all, and once.
//!
//! Every table is made here and nowhere else; the code that reads and writes a table just opens
//! the file (`super::connect`). The recorder, in a crate of its own, has a copy of its tables for
//! its own tests; a change to them is a migration here and a change to that copy.

use rusqlite::{Connection, Transaction, TransactionBehavior};

/// One step from the version before it to `version`.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// The version the file is at after this step: one more than the step before.
    pub version: u32,
    /// What the step does, for whoever reads this list.
    pub what: &'static str,
    /// The step. It runs inside the transaction that then sets `user_version`.
    pub apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

/// Every version `irori.db` has had, in order.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    what: "the tables as they were before the file had a version",
    apply: |tx| tx.execute_batch(V1),
}];

/// The version this build writes, and the newest it opens.
pub const LATEST: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Every table as it stood when versions began (Irori 1.5). `IF NOT EXISTS` throughout: a file
/// from before versions has some or all of these, made by the same statements, and keeps them as
/// they are; the ones a part of Irori it never used would have made are made now.
const V1: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS extension_kv (
    extension TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (extension, key)
);

CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    user TEXT NOT NULL,
    expires INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tokens (
    token_hash TEXT PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    user TEXT NOT NULL,
    scopes TEXT NOT NULL,
    extension TEXT,
    created INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS assistant_message (
    scope TEXT NOT NULL,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (scope, seq)
);
CREATE TABLE IF NOT EXISTS assistant_reach (
    user TEXT PRIMARY KEY,
    entities TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS assistant_usage (
    scope TEXT PRIMARY KEY,
    used INTEGER NOT NULL,
    size INTEGER
);

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

CREATE TABLE IF NOT EXISTS state_history (
    id INTEGER PRIMARY KEY,
    entity_id TEXT NOT NULL,
    updated_ns INTEGER NOT NULL,
    state TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS state_history_entity_time
    ON state_history (entity_id, updated_ns, id);

CREATE TABLE IF NOT EXISTS converted_units (
    entity_id TEXT PRIMARY KEY,
    unit TEXT NOT NULL
);
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

/// Why a file wasn't brought up to date.
#[derive(Debug)]
pub enum Error {
    /// The file is at a version this build doesn't know.
    Newer {
        found: u32,
        known: u32,
    },
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Newer { found, known } => write!(
                f,
                "the database is at version {found}, and this Irori ({}) knows versions up to \
                 {known}: a newer Irori wrote it",
                crate::build_info::VERSION
            ),
            Self::Sqlite(error) => write!(f, "couldn't update the database: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Newer { .. } => None,
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// The version `conn`'s file says it is at. 0 is a file from before versions, or a new one.
pub fn version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Brings `conn`'s file to the last version in `list`, running each step it hasn't had, in order,
/// in one transaction. Returns the version it was at. A file already there costs one read and
/// takes no lock; one newer than `list` knows is left alone and refused.
pub fn run(conn: &Connection, list: &[Migration]) -> Result<u32, Error> {
    let latest = list.last().map_or(0, |m| m.version);
    let refuse = |found| Error::Newer {
        found,
        known: latest,
    };
    let seen = version(conn)?;
    if seen == latest {
        return Ok(seen);
    }
    if seen > latest {
        return Err(refuse(seen));
    }
    // Another connection may be doing the same thing: take the write lock, then look again.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let from = version(&tx)?;
    if from > latest {
        return Err(refuse(from));
    }
    for step in list.iter().filter(|m| m.version > from) {
        (step.apply)(&tx)?;
    }
    tx.pragma_update(None, "user_version", latest)?;
    tx.commit()?;
    if from < latest {
        tracing::info!(from, to = latest, "database updated");
    }
    Ok(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is numbered 1, 2, 3… with no gap, and the latest is its last.
    #[test]
    fn versions_count_up_from_one() {
        for (at, step) in MIGRATIONS.iter().enumerate() {
            assert_eq!(step.version as usize, at + 1, "{}", step.what);
        }
        assert_eq!(LATEST as usize, MIGRATIONS.len());
    }

    fn log_step(tx: &Transaction<'_>, n: u32) -> rusqlite::Result<()> {
        tx.execute("INSERT INTO ran (n) VALUES (?1)", [n])?;
        Ok(())
    }

    const STEPS: &[Migration] = &[
        Migration {
            version: 1,
            what: "a log of what ran",
            apply: |tx| {
                tx.execute_batch("CREATE TABLE ran (seq INTEGER PRIMARY KEY, n INTEGER)")?;
                log_step(tx, 1)
            },
        },
        Migration {
            version: 2,
            what: "second",
            apply: |tx| log_step(tx, 2),
        },
        Migration {
            version: 3,
            what: "third",
            apply: |tx| log_step(tx, 3),
        },
    ];

    fn ran(conn: &Connection) -> rusqlite::Result<Vec<u32>> {
        conn.prepare("SELECT n FROM ran ORDER BY seq")?
            .query_map([], |row| row.get(0))?
            .collect()
    }

    /// Steps run in order, each once: again after a restart, and from where an older file was.
    #[test]
    fn steps_run_in_order_and_once() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("x.db");
        {
            let conn = Connection::open(&path)?;
            assert_eq!(run(&conn, &STEPS[..2])?, 0);
            assert_eq!(ran(&conn)?, [1, 2]);
            assert_eq!(version(&conn)?, 2);
        }
        let conn = Connection::open(&path)?;
        assert_eq!(run(&conn, STEPS)?, 2);
        assert_eq!(run(&conn, STEPS)?, 3);
        assert_eq!(ran(&conn)?, [1, 2, 3]);
        assert_eq!(version(&conn)?, 3);
        Ok(())
    }

    /// A step that fails leaves the file as it was: no half-made tables, the old version.
    #[test]
    fn a_failed_step_changes_nothing() -> anyhow::Result<()> {
        const BROKEN: &[Migration] = &[
            STEPS[0],
            Migration {
                version: 2,
                what: "fails",
                apply: |tx| tx.execute_batch("CREATE TABLE half (n INTEGER); NOT SQL"),
            },
        ];
        let conn = Connection::open_in_memory()?;
        assert!(matches!(run(&conn, BROKEN), Err(Error::Sqlite(_))));
        assert_eq!(version(&conn)?, 0);
        let tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 0);
        Ok(())
    }

    /// A file newer than the list is refused and not touched.
    #[test]
    fn a_newer_file_is_refused() -> anyhow::Result<()> {
        let conn = Connection::open_in_memory()?;
        run(&conn, STEPS)?;
        let error = run(&conn, &STEPS[..2]).expect_err("refused");
        assert!(
            matches!(error, Error::Newer { found: 3, known: 2 }),
            "{error}"
        );
        assert_eq!(ran(&conn)?, [1, 2, 3]);
        assert_eq!(version(&conn)?, 3);
        Ok(())
    }
}
