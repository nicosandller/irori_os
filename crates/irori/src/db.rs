//! Opens the bundled SQLite database. Placeholder until `irori-recorder` owns it (M1.3).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use rusqlite::Connection;

#[derive(Debug)]
pub struct Database {
    pub path: PathBuf,
    pub journal_mode: String,
}

/// Creates `data_dir` if needed, opens `irori.db` in WAL mode, and checks it is writable.
pub fn open(data_dir: &Path) -> anyhow::Result<Database> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;
    let path = data_dir.join("irori.db");
    let conn = Connection::open(&path)
        .with_context(|| format!("failed to open database {}", path.display()))?;

    let journal_mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))?;
    ensure!(
        journal_mode.eq_ignore_ascii_case("wal"),
        "database {} refused WAL mode (got {journal_mode})",
        path.display()
    );
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('created_by_version', ?1)",
        [crate::build_info::VERSION],
    )?;

    Ok(Database { path, journal_mode })
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
        let conn = Connection::open(&db.path)
            .with_context(|| format!("failed to open database {}", db.path.display()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS extension_kv (
                extension TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (extension, key)
            )",
        )?;
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
