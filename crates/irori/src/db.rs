//! Opens the home's SQLite database.
//!
//! Everything Irori keeps is in this one file, `irori.db`: sign-ins, tokens, the assistant's
//! chats, the activity log, what protocols remember, and entity history (on the recorder's own
//! connection, `irori-recorder`). Which tables it has is decided by its version
//! ([`migrations`]); [`open`] brings it up to date when Irori starts, and refuses a file a newer
//! Irori wrote.

pub mod migrations;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use rusqlite::Connection;

/// How long a connection waits when another one is writing `irori.db`.
///
/// The timeout belongs to the connection that is waiting, not the one holding the lock. History
/// batches are the usual holder, so a session, a token, or a protocol value that arrives during
/// one waits this long instead of failing the write.
pub const BUSY: Duration = Duration::from_secs(5);

pub fn wait_when_busy(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY)
}

#[derive(Debug)]
pub struct Database {
    pub path: PathBuf,
    pub journal_mode: String,
}

/// Creates `data_dir` if needed, opens `irori.db` in WAL mode, checks it is writable, and brings
/// it to this build's version. A file a newer Irori wrote is refused before anything is written
/// to it.
pub fn open(data_dir: &Path) -> anyhow::Result<Database> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;
    let path = data_dir.join("irori.db");
    let conn = Connection::open(&path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    wait_when_busy(&conn)?;
    let found = migrations::version(&conn)
        .with_context(|| format!("failed to read database {}", path.display()))?;
    if found > migrations::LATEST {
        bail!("{}", too_new(&path, found));
    }

    let journal_mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))?;
    ensure!(
        journal_mode.eq_ignore_ascii_case("wal"),
        "database {} refused WAL mode (got {journal_mode})",
        path.display()
    );
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    migrations::run(&conn, migrations::MIGRATIONS).map_err(|error| match error {
        migrations::Error::Newer { found, .. } => anyhow::anyhow!("{}", too_new(&path, found)),
        other => anyhow::Error::new(other).context(format!("database {}", path.display())),
    })?;
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('created_by_version', ?1)",
        [crate::build_info::VERSION],
    )?;

    Ok(Database { path, journal_mode })
}

/// What Irori says when it won't start on a file a newer Irori wrote, and the ways out.
fn too_new(path: &Path, found: u32) -> String {
    format!(
        "{path} was written by a newer Irori: it is at database version {found}, and this Irori \
         ({ours}) knows versions up to {latest}. Starting would risk losing what the newer one \
         wrote, so Irori won't start on it. If you went back to an older Irori (`irori.previous`), \
         run the newer one again; or, to stay on this one, restore the backup made before the \
         update with `irori backup restore pre-update-…` (`irori backup list` shows its name).",
        path = path.display(),
        ours = crate::build_info::VERSION,
        latest = migrations::LATEST,
    )
}

/// Opens another connection to `irori.db`, for a part of Irori that keeps its own: it waits when
/// the file is busy, and has every table this build expects. [`open`] has already brought the
/// file up to date by the time any of these run, so here that is one read; a file nobody has
/// opened yet (a test's) is brought up here, and a newer one is refused.
pub fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    wait_when_busy(&conn)?;
    migrations::run(&conn, migrations::MIGRATIONS).map_err(|error| match error {
        migrations::Error::Sqlite(error) => error,
        newer @ migrations::Error::Newer { .. } => rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(newer.to_string()),
        ),
    })?;
    Ok(conn)
}

/// The version of the database at `path`, read without changing it.
pub fn version_of(path: &Path) -> anyhow::Result<u32> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("couldn't open {}", path.display()))?;
    wait_when_busy(&conn)?;
    Ok(migrations::version(&conn)?)
}

/// Protocols' small private values, kept in `irori.db` so they outlast restarts
/// (`docs/specs/protocols.md` §5). One connection behind a lock: these are a few writes a
/// minute at most — a switch flipped, a key paired — not a stream.
#[derive(Debug)]
pub struct SqliteStorage {
    conn: std::sync::Mutex<Connection>,
}

impl SqliteStorage {
    pub fn open(db: &Database) -> anyhow::Result<Self> {
        let conn = connect(&db.path)
            .with_context(|| format!("failed to open database {}", db.path.display()))?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl irori_protocol::Storage for SqliteStorage {
    fn load(
        &self,
        extension: &irori_types::ExtensionId,
        key: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        use rusqlite::OptionalExtension as _;
        let text: Option<String> = self
            .conn()
            .query_row(
                "SELECT value FROM extension_kv WHERE extension = ?1 AND key = ?2",
                [extension.as_str(), key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        text.map(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
            .transpose()
    }

    fn store(
        &self,
        extension: &irori_types::ExtensionId,
        key: &str,
        value: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let conn = self.conn();
        match value {
            Some(value) => conn.execute(
                "INSERT INTO extension_kv (extension, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (extension, key) DO UPDATE SET value = excluded.value",
                [extension.as_str(), key, &value.to_string()],
            ),
            None => conn.execute(
                "DELETE FROM extension_kv WHERE extension = ?1 AND key = ?2",
                [extension.as_str(), key],
            ),
        }
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn clear(&self, extension: &irori_types::ExtensionId) -> Result<(), String> {
        self.conn()
            .execute(
                "DELETE FROM extension_kv WHERE extension = ?1",
                [extension.as_str()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_wal_mode_and_is_reopenable() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let data = dir.path().join("nested/data");

        let db = open(&data)?;
        assert_eq!(db.journal_mode, "wal");
        assert!(db.path.exists());

        // Opening an existing database must not fail or reset it.
        open(&data)?;
        Ok(())
    }

    fn tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?
            .query_map([], |row| row.get(0))?
            .collect()
    }

    /// A new home's file is made at the latest version, with every table.
    #[test]
    fn a_new_database_starts_at_the_latest_version() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(dir.path())?;
        let conn = Connection::open(&db.path)?;
        assert_eq!(migrations::version(&conn)?, migrations::LATEST);
        let tables = tables(&conn)?;
        for table in [
            "meta",
            "extension_kv",
            "sessions",
            "tokens",
            "assistant_message",
            "audit",
            "state_history",
            "statistics",
        ] {
            assert!(tables.iter().any(|t| t == table), "{table} in {tables:?}");
        }
        Ok(())
    }

    /// A file Irori 1.5 or earlier wrote has no version. It is taken as version 1 as it is: its
    /// rows stay, and the tables a part of Irori it never used would have made are added.
    #[test]
    fn a_database_from_before_versions_keeps_its_rows() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("irori.db");
        {
            // Made the way 1.5 made them, each by the part of Irori that used it; this home
            // never opened the assistant.
            let conn = Connection::open(&path)?;
            conn.pragma_update(None, "journal_mode", "wal")?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta VALUES ('created_by_version', '1.4.2');
                 CREATE TABLE IF NOT EXISTS extension_kv (
                     extension TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
                     PRIMARY KEY (extension, key));
                 INSERT INTO extension_kv VALUES ('helpers', 'toggle.guests', 'true');
                 CREATE TABLE IF NOT EXISTS sessions (
                     token_hash TEXT PRIMARY KEY, user TEXT NOT NULL, expires INTEGER NOT NULL);
                 INSERT INTO sessions VALUES ('h', 'owner', 99);
                 CREATE TABLE IF NOT EXISTS state_history (
                     id INTEGER PRIMARY KEY, entity_id TEXT NOT NULL,
                     updated_ns INTEGER NOT NULL, state TEXT NOT NULL);
                 CREATE INDEX IF NOT EXISTS state_history_entity_time
                     ON state_history (entity_id, updated_ns, id);
                 INSERT INTO state_history (entity_id, updated_ns, state)
                     VALUES ('light.hall', 1, '{}');",
            )?;
            assert_eq!(migrations::version(&conn)?, 0);
        }
        open(dir.path())?;
        let conn = Connection::open(&path)?;
        assert_eq!(migrations::version(&conn)?, migrations::LATEST);
        let one = |q: &str| conn.query_row(q, [], |row| row.get::<_, String>(0));
        assert_eq!(
            one("SELECT value FROM meta WHERE key = 'created_by_version'")?,
            "1.4.2",
            "the version that made it is still the one that made it"
        );
        assert_eq!(one("SELECT value FROM extension_kv")?, "true");
        assert_eq!(one("SELECT user FROM sessions")?, "owner");
        assert_eq!(one("SELECT entity_id FROM state_history")?, "light.hall");
        assert!(tables(&conn)?.iter().any(|t| t == "assistant_message"));
        Ok(())
    }

    /// A file a newer Irori wrote (after going back to `irori.previous`, say) is refused with a
    /// way out, and nothing is written to it.
    #[test]
    fn a_database_from_a_newer_irori_is_refused_and_left_alone() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("irori.db");
        let newer = migrations::LATEST + 1;
        Connection::open(&path)?.pragma_update(None, "user_version", newer)?;

        let error = format!("{:#}", open(dir.path()).expect_err("refused"));
        assert!(error.contains("newer Irori"), "{error}");
        assert!(error.contains("irori.previous"), "{error}");
        assert!(error.contains("irori backup restore"), "{error}");

        let conn = Connection::open(&path)?;
        assert_eq!(migrations::version(&conn)?, newer);
        assert_eq!(tables(&conn)?, Vec::<String>::new(), "nothing was made");
        let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        assert_eq!(journal, "delete", "not even the journal mode was changed");
        // A part of Irori that opens the file on its own refuses it too.
        let error = connect(&path).expect_err("refused").to_string();
        assert!(error.contains("newer Irori"), "{error}");
        Ok(())
    }

    /// What a protocol keeps is there after Irori restarts, and only for that protocol.
    #[test]
    fn stored_values_outlast_a_restart_and_stay_private() -> anyhow::Result<()> {
        use irori_protocol::Storage as _;
        let dir = tempfile::tempdir()?;
        let helpers = irori_types::ExtensionId::try_from("helpers")?;
        let other = irori_types::ExtensionId::try_from("esphome")?;
        {
            let storage = SqliteStorage::open(&open(dir.path())?)?;
            storage
                .store(&helpers, "toggle.guests", Some(&serde_json::json!(true)))
                .map_err(anyhow::Error::msg)?;
            storage
                .store(&helpers, "toggle.guests", Some(&serde_json::json!(false)))
                .map_err(anyhow::Error::msg)?;
        }
        let storage = SqliteStorage::open(&open(dir.path())?)?;
        assert_eq!(
            storage
                .load(&helpers, "toggle.guests")
                .map_err(anyhow::Error::msg)?,
            Some(serde_json::json!(false))
        );
        assert_eq!(
            storage
                .load(&other, "toggle.guests")
                .map_err(anyhow::Error::msg)?,
            None
        );
        storage
            .store(&helpers, "toggle.guests", None)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            storage
                .load(&helpers, "toggle.guests")
                .map_err(anyhow::Error::msg)?,
            None
        );
        Ok(())
    }
}
