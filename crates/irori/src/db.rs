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
}
