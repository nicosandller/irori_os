//! Backup and restore (`docs/specs/backup.md`).
//!
//! A backup is one `.tar.gz` holding `manifest.json`, `config/` and `data/`. The database is
//! never copied as a file: it is in WAL mode with several connections open, so a snapshot is
//! made with `VACUUM INTO`, which is consistent and needs the server to stop for nothing.
//! `tar` is the system's, as in `packages.rs`, and is used to read members one at a time only
//! after every name in the archive has been checked.

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, bail, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The archive layout this build writes and reads.
const FORMAT: u32 = 1;
/// What a scheduled or requested backup is named: `irori-backup-<UTC date>-<time>.tar.gz`.
const PREFIX: &str = "irori-backup-";
/// What restore keeps of the home it is about to replace. Not pruned with the others.
const SAFETY_PREFIX: &str = "pre-restore-";
const SUFFIX: &str = ".tar.gz";
const DEFAULT_DIR: &str = "backups";

/// Top-level entries of the data directory that a backup leaves out: models and logs that are
/// big or rebuilt, installed packages (reinstalled from `official.toml`), the CLI's pointer to
/// a running server, and the backups themselves.
const SKIPPED_DATA: &[&str] = &[
    "ollama",
    "ollama.log",
    "ollama.pid",
    "extensions",
    "cli.url",
    DEFAULT_DIR,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub irori_version: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    /// Packages that were installed. They are not in the archive: install them again.
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub bytes: u64,
    pub modified: String,
    /// `None` for a file whose manifest couldn't be read.
    pub manifest: Option<Manifest>,
}

/// Where backups go: `[backup] dir` against the data directory, or `<data>/backups`.
pub fn dir(data: &Path, configured: Option<&Path>) -> PathBuf {
    data.join(configured.unwrap_or_else(|| Path::new(DEFAULT_DIR)))
}

/// Writes a backup of this home into `into` and returns its path. Safe while the server runs.
pub fn create(config: &Path, data: &Path, into: &Path) -> anyhow::Result<PathBuf> {
    create_named(config, data, into, PREFIX)
}

fn create_named(config: &Path, data: &Path, into: &Path, prefix: &str) -> anyhow::Result<PathBuf> {
    make_private_dir(into)?;
    let now = jiff::Timestamp::now();
    let stamp = now.strftime("%Y%m%d-%H%M%S").to_string();
    let target = into.join(format!("{prefix}{stamp}{SUFFIX}"));
    ensure!(!target.exists(), "{} already exists", target.display());

    // Next to the result, so the finished file is moved by a rename and nobody sees half of it.
    // Dot-named, so `list` never takes it for a backup.
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(into)
        .context("couldn't make a staging directory")?;
    let root = staging.path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;

    copy_tree(config, &root.join("config"), &|_| false)?;
    let staged_data = root.join("data");
    fs::create_dir_all(&staged_data)?;
    // Where these backups go, when that is inside the data directory, is left out too.
    let own_dir = into
        .strip_prefix(data)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_owned());
    copy_tree(data, &staged_data, &|rel| {
        let mut parts = rel.components();
        let Some(first) = parts
            .next()
            .and_then(|c| c.as_os_str().to_str().map(str::to_owned))
        else {
            return true;
        };
        parts.next().is_none()
            && (SKIPPED_DATA.contains(&first.as_str())
                || first.starts_with("irori.db")
                || first.starts_with(".restore-")
                || own_dir.as_deref().is_some_and(|own| own == first.as_str()))
    })?;
    snapshot_database(&data.join("irori.db"), &staged_data.join("irori.db"))?;

    let manifest = Manifest {
        format: FORMAT,
        irori_version: crate::build_info::VERSION.to_owned(),
        created_at: now.to_string(),
        extensions: installed_extensions(data),
    };
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let part = into.join(format!(".{prefix}{stamp}.part"));
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&part)
        .arg("-C")
        .arg(root)
        .args(["manifest.json", "config", "data"])
        // macOS tar otherwise adds `._*` AppleDouble members.
        .env("COPYFILE_DISABLE", "1")
        .status()
        .context("couldn't run tar")?;
    if !status.success() {
        let _ = fs::remove_file(&part);
        bail!("tar failed to write the backup");
    }
    fs::set_permissions(&part, fs::Permissions::from_mode(0o600))?;
    fs::rename(&part, &target)?;
    Ok(target)
}

/// Backups in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, Entry)> = read
        .flatten()
        .filter_map(|item| {
            let name = item.file_name().to_str()?.to_owned();
            if !is_backup_name(&name) {
                return None;
            }
            let meta = item.metadata().ok()?;
            let modified = meta.modified().ok()?;
            let manifest = read_manifest(&item.path()).ok();
            let stamp = jiff::Timestamp::try_from(modified).ok()?;
            Some((
                modified,
                Entry {
                    name,
                    bytes: meta.len(),
                    modified: stamp.to_string(),
                    manifest,
                },
            ))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.name.cmp(&a.1.name)));
    found.into_iter().map(|(_, entry)| entry).collect()
}

/// Whether `name` is something this module wrote, and so something a request may name.
pub fn is_backup_name(name: &str) -> bool {
    (name.starts_with(PREFIX) || name.starts_with(SAFETY_PREFIX))
        && name.ends_with(SUFFIX)
        && !name.contains(['/', '\\'])
        && !name.contains("..")
}

/// Deletes all but the newest `keep` scheduled/requested backups. Safety copies stay.
pub fn prune(dir: &Path, keep: u32) -> usize {
    let mut removed = 0;
    for entry in list(dir)
        .into_iter()
        .filter(|entry| entry.name.starts_with(PREFIX))
        .skip(keep.max(1) as usize)
    {
        if fs::remove_file(dir.join(&entry.name)).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// How long ago the newest backup was made, if there is one.
pub fn newest_age(dir: &Path) -> Option<Duration> {
    let newest = list(dir)
        .into_iter()
        .find(|entry| entry.name.starts_with(PREFIX))?;
    let at: jiff::Timestamp = newest.modified.parse().ok()?;
    let now = jiff::Timestamp::now();
    Some(Duration::from_secs(
        now.as_second().saturating_sub(at.as_second()).max(0) as u64,
    ))
}

/// Takes a backup every `every_hours` while Irori runs, and once at the start if the newest is
/// older than that. A failure is logged and tried again at the next time.
pub async fn schedule(settings: irori_config::BackupSection, config: PathBuf, data: PathBuf) {
    let every = Duration::from_secs(u64::from(settings.every_hours.max(1)) * 3600);
    let into = dir(&data, settings.dir.as_deref());
    let mut wait = match newest_age(&into) {
        Some(age) => every.saturating_sub(age),
        None => Duration::from_secs(60),
    };
    loop {
        tokio::time::sleep(wait).await;
        wait = every;
        let (config, data, into, keep) =
            (config.clone(), data.clone(), into.clone(), settings.keep);
        let done = tokio::task::spawn_blocking(move || {
            let file = create(&config, &data, &into)?;
            Ok::<_, anyhow::Error>((file, prune(&into, keep)))
        })
        .await;
        match done {
            Ok(Ok((file, pruned))) => {
                tracing::info!(file = %file.display(), pruned, "backup written");
            }
            Ok(Err(error)) => tracing::warn!(%error, "scheduled backup failed"),
            Err(error) => tracing::warn!(%error, "scheduled backup failed"),
        }
    }
}

/// What a restore did.
#[derive(Debug, Serialize)]
pub struct Restored {
    pub manifest: Manifest,
    /// The copy of the home as it was, when there was one to keep.
    pub safety_copy: Option<PathBuf>,
}

/// Replaces `config` and the state in `data` with the contents of `archive`.
///
/// Meant for a server that is not running: the caller checks. Everything that can be refused is
/// refused before the first file is touched, and the home as it was is kept as a backup first.
pub fn restore(archive: &Path, config: &Path, data: &Path) -> anyhow::Result<Restored> {
    ensure!(archive.is_file(), "{} is not a file", archive.display());
    check_members(archive)?;
    let manifest = read_manifest(archive)?;
    ensure!(
        manifest.format == FORMAT,
        "this backup is format {}, and this Irori reads format {FORMAT}",
        manifest.format
    );
    ensure!(
        !newer_than_us(&manifest.irori_version),
        "this backup was made by Irori {}, which is newer than this one ({}); update Irori first",
        manifest.irori_version,
        crate::build_info::VERSION
    );

    fs::create_dir_all(data)?;
    let staging = tempfile::Builder::new()
        .prefix(".restore-")
        .tempdir_in(data)
        .context("couldn't make a staging directory")?;
    let root = staging.path();
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(root)
        .status()
        .context("couldn't run tar")?;
    ensure!(status.success(), "tar couldn't read the backup");

    let staged_db = root.join("data/irori.db");
    if staged_db.exists() {
        let conn =
            Connection::open_with_flags(&staged_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let verdict: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        ensure!(
            verdict == "ok",
            "the backup's database is damaged: {verdict}"
        );
    }
    let mut store = irori_config::Store::new(root.join("config"));
    let problems = store.reload();
    ensure!(
        problems.is_empty(),
        "the backup's config doesn't load: {}",
        problems
            .iter()
            .map(|p| format!("{}: {}", p.file, p.reason))
            .collect::<Vec<_>>()
            .join("; ")
    );

    // Past here the home changes. Keep the one that is there.
    let had_home = config.is_dir() || data.join("irori.db").exists();
    let safety_copy = if had_home {
        let into = dir(data, None);
        Some(
            create_named(config, data, &into, SAFETY_PREFIX)
                .context("couldn't keep the current home first")?,
        )
    } else {
        None
    };

    // The config directory is replaced whole: a file the backup doesn't have is a file that
    // wasn't there.
    if config.exists() {
        for item in fs::read_dir(config)? {
            remove_any(&item?.path())?;
        }
    }
    copy_tree(&root.join("config"), config, &|_| false)?;

    // In the data directory only what a backup holds is replaced.
    for stale in ["irori.db", "irori.db-wal", "irori.db-shm"] {
        let path = data.join(stale);
        if path.exists() {
            fs::remove_file(&path)?;
        }
    }
    let staged_data = root.join("data");
    if staged_data.is_dir() {
        for item in fs::read_dir(&staged_data)? {
            let item = item?;
            let target = data.join(item.file_name());
            if target.exists() {
                remove_any(&target)?;
            }
            if item.file_type()?.is_dir() {
                copy_tree(&item.path(), &target, &|_| false)?;
            } else {
                fs::copy(item.path(), &target)?;
            }
        }
    }
    Ok(Restored {
        manifest,
        safety_copy,
    })
}

/// Whether a server is answering at `<data>/cli.url`. Restore refuses then: the files are open.
pub fn server_running(data: &Path) -> bool {
    let Ok(url) = fs::read_to_string(data.join("cli.url")) else {
        return false;
    };
    let url = url.trim();
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .trim_end_matches('/');
    let Ok(mut addrs) = std::net::ToSocketAddrs::to_socket_addrs(host) else {
        return false;
    };
    addrs
        .any(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok())
}

fn snapshot_database(from: &Path, to: &Path) -> anyhow::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    let conn = Connection::open_with_flags(from, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("couldn't open {}", from.display()))?;
    crate::db::wait_when_busy(&conn)?;
    let target = to
        .to_str()
        .context("the staging path isn't valid UTF-8")?
        .replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{target}'"))
        .context("couldn't snapshot the database")?;
    Ok(())
}

fn installed_extensions(data: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(crate::packages::packages_dir(data))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|item| item.path().is_dir())
        .filter_map(|item| item.file_name().to_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

/// Every member must be a plain file or directory under `manifest.json`, `config/` or `data/`,
/// with a name that can't leave the directory it is extracted into.
fn check_members(archive: &Path) -> anyhow::Result<()> {
    let listing = output("tar", &["-tvzf".as_ref(), archive.as_os_str()])?;
    ensure!(!listing.trim().is_empty(), "the backup is empty");
    for line in listing.lines() {
        let kind = line.chars().next();
        ensure!(
            matches!(kind, Some('-' | 'd')),
            "the backup holds something other than files and directories: {line}"
        );
    }
    let names = output("tar", &["-tzf".as_ref(), archive.as_os_str()])?;
    for name in names.lines() {
        let trimmed = name.trim_start_matches("./");
        let mut parts = trimmed.split('/').filter(|part| !part.is_empty());
        let first = parts.next().unwrap_or("");
        ensure!(
            !name.starts_with('/')
                && !trimmed.split('/').any(|part| part == ".." || part == ".")
                && matches!(first, "manifest.json" | "config" | "data"),
            "the backup has a suspicious member `{name}`"
        );
    }
    Ok(())
}

fn read_manifest(archive: &Path) -> anyhow::Result<Manifest> {
    let bytes = Command::new("tar")
        .arg("-xzOf")
        .arg(archive)
        .arg("manifest.json")
        .stderr(Stdio::null())
        .output()
        .context("couldn't run tar")?;
    ensure!(
        bytes.status.success(),
        "{} has no manifest.json; it isn't an Irori backup",
        archive.display()
    );
    serde_json::from_slice(&bytes.stdout).context("manifest.json isn't readable")
}

/// `0.0.0` is a development build and reads anything.
fn newer_than_us(theirs: &str) -> bool {
    fn parts(version: &str) -> Option<(u64, u64, u64)> {
        let core = version.split(['-', '+']).next()?;
        let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
        Some((it.next()??, it.next()??, it.next()??))
    }
    match (parts(theirs), parts(crate::build_info::VERSION)) {
        (Some(theirs), Some(ours)) if ours != (0, 0, 0) => theirs > ours,
        _ => false,
    }
}

fn output(cmd: &str, args: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("couldn't run {cmd}"))?;
    ensure!(
        out.status.success(),
        "{cmd} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("couldn't create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Copies regular files and directories. A link is left behind; `skip` is given each path
/// relative to `from`. Files are created private: they hold secrets.
fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> anyhow::Result<()> {
    fn walk(
        root: &Path,
        dir: &Path,
        to: &Path,
        skip: &dyn Fn(&Path) -> bool,
    ) -> anyhow::Result<()> {
        for item in fs::read_dir(dir)? {
            let item = item?;
            let path = item.path();
            let rel = path.strip_prefix(root)?;
            if skip(rel) {
                continue;
            }
            let kind = item.file_type()?;
            let target = to.join(rel);
            if kind.is_dir() {
                fs::create_dir_all(&target)?;
                walk(root, &path, to, skip)?;
            } else if kind.is_file() {
                copy_private(&path, &target)?;
            }
        }
        Ok(())
    }
    fs::create_dir_all(to)?;
    if from.is_dir() {
        walk(from, from, to, skip)?;
    }
    Ok(())
}

fn copy_private(from: &Path, to: &Path) -> anyhow::Result<()> {
    let mut source = fs::File::open(from)?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(to)?;
    std::io::copy(&mut source, &mut target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config dir and a data dir with a real WAL database, a package, and some noise.
    fn home() -> anyhow::Result<tempfile::TempDir> {
        let tmp = tempfile::tempdir()?;
        let (config, data) = (tmp.path().join("config"), tmp.path().join("data"));
        fs::create_dir_all(config.join("flows"))?;
        fs::write(config.join("areas.toml"), "[areas.hall]\nname = \"Hall\"\n")?;
        fs::write(config.join("flows/a.json"), "{}")?;
        let db = crate::db::open(&data)?;
        let conn = Connection::open(&db.path)?;
        conn.execute_batch("CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (42)")?;
        // Left open on purpose: the snapshot has to work with a live writer.
        std::mem::forget(conn);
        fs::create_dir_all(data.join("extension-data/zigbee"))?;
        fs::write(data.join("extension-data/zigbee/key"), "network-key")?;
        fs::create_dir_all(data.join("extensions/demo"))?;
        fs::write(data.join("extensions/demo/bin"), "big")?;
        fs::create_dir_all(data.join("ollama"))?;
        fs::write(data.join("ollama/model"), "huge")?;
        fs::write(data.join("cli.url"), "http://127.0.0.1:1")?;
        Ok(tmp)
    }

    fn members(archive: &Path) -> anyhow::Result<String> {
        output("tar", &["-tzf".as_ref(), archive.as_os_str()])
    }

    #[test]
    fn a_backup_holds_state_and_leaves_out_what_is_rebuilt() -> anyhow::Result<()> {
        let tmp = home()?;
        let (config, data) = (tmp.path().join("config"), tmp.path().join("data"));
        let out = create(&config, &data, &dir(&data, None))?;
        assert!(is_backup_name(
            out.file_name().and_then(|n| n.to_str()).unwrap_or_default()
        ));
        assert_eq!(fs::metadata(&out)?.permissions().mode() & 0o777, 0o600);
        let names = members(&out)?;
        for want in [
            "manifest.json",
            "config/areas.toml",
            "config/flows/a.json",
            "data/irori.db",
            "data/extension-data/zigbee/key",
        ] {
            assert!(
                names.lines().any(|n| n.trim_start_matches("./") == want),
                "{want} in\n{names}"
            );
        }
        for not in [
            "ollama",
            "cli.url",
            "data/extensions",
            "backups",
            "-wal",
            "-shm",
            ".staging",
        ] {
            assert!(!names.contains(not), "{not} in\n{names}");
        }
        let manifest = read_manifest(&out)?;
        assert_eq!(manifest.extensions, ["demo"]);
        assert_eq!(list(&dir(&data, None)).len(), 1);
        Ok(())
    }

    #[test]
    fn restore_brings_back_what_was_lost_and_keeps_the_home_it_replaced() -> anyhow::Result<()> {
        let tmp = home()?;
        let (config, data) = (tmp.path().join("config"), tmp.path().join("data"));
        let backup = create(&config, &data, &dir(&data, None))?;
        // Things go wrong after the backup.
        fs::remove_file(config.join("areas.toml"))?;
        fs::write(config.join("extra.toml"), "")?;
        fs::remove_dir_all(data.join("extension-data"))?;
        fs::remove_file(data.join("irori.db"))?;
        let _ = fs::remove_file(data.join("irori.db-wal"));
        let _ = fs::remove_file(data.join("irori.db-shm"));

        let done = restore(&backup, &config, &data)?;
        assert!(config.join("areas.toml").exists());
        assert!(
            !config.join("extra.toml").exists(),
            "a file the backup lacks is gone"
        );
        assert_eq!(
            fs::read_to_string(data.join("extension-data/zigbee/key"))?,
            "network-key"
        );
        let conn = Connection::open(data.join("irori.db"))?;
        assert_eq!(
            conn.query_row("SELECT n FROM t", [], |r| r.get::<_, i64>(0))?,
            42
        );
        // Packages and models weren't touched.
        assert!(data.join("extensions/demo/bin").exists());
        assert!(data.join("ollama/model").exists());
        let safety = done.safety_copy.expect("there was a home to keep");
        assert!(
            safety
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .starts_with(SAFETY_PREFIX)
        );
        assert!(!data.join(".restore-x").exists());
        Ok(())
    }

    #[test]
    fn restore_onto_a_new_machine_needs_no_safety_copy() -> anyhow::Result<()> {
        let tmp = home()?;
        let (config, data) = (tmp.path().join("config"), tmp.path().join("data"));
        let backup = create(&config, &data, &dir(&data, None))?;
        let fresh = tempfile::tempdir()?;
        let (c2, d2) = (fresh.path().join("c"), fresh.path().join("d"));
        let done = restore(&backup, &c2, &d2)?;
        assert!(done.safety_copy.is_none());
        assert!(c2.join("flows/a.json").exists());
        assert!(d2.join("irori.db").exists());
        Ok(())
    }

    #[test]
    fn an_archive_that_could_escape_or_isnt_ours_is_refused_untouched() -> anyhow::Result<()> {
        let tmp = home()?;
        let (config, data) = (tmp.path().join("config"), tmp.path().join("data"));
        let work = tempfile::tempdir()?;

        // A member outside the three roots.
        let tree = work.path().join("t1");
        fs::create_dir_all(&tree)?;
        fs::write(tree.join("manifest.json"), "{}")?;
        fs::write(tree.join("evil"), "x")?;
        let bad = work.path().join("irori-backup-bad.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&bad)
                .arg("-C")
                .arg(&tree)
                .args(["manifest.json", "evil"])
                .status()?
                .success()
        );
        let err = restore(&bad, &config, &data)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("suspicious"), "{err}");

        // A link.
        let tree = work.path().join("t2");
        fs::create_dir_all(tree.join("config"))?;
        fs::write(tree.join("manifest.json"), "{}")?;
        std::os::unix::fs::symlink("/etc/passwd", tree.join("config/link"))?;
        let linked = work.path().join("irori-backup-link.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&linked)
                .arg("-C")
                .arg(&tree)
                .args(["manifest.json", "config"])
                .status()?
                .success()
        );
        let err = restore(&linked, &config, &data)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("other than files"), "{err}");

        // No manifest; not a tarball at all.
        let junk = work.path().join("junk.tar.gz");
        fs::write(&junk, "not a tarball")?;
        assert!(restore(&junk, &config, &data).is_err());

        assert!(config.join("areas.toml").exists(), "nothing was touched");
        Ok(())
    }

    #[test]
    fn a_backup_from_a_newer_irori_is_refused() {
        assert!(newer_than_us("999.0.0") == (crate::build_info::VERSION != "0.0.0"));
        assert!(!newer_than_us("garbage"));
    }

    #[test]
    fn pruning_keeps_the_newest_and_the_safety_copies() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        for name in [
            "irori-backup-20260101-000000.tar.gz",
            "irori-backup-20260102-000000.tar.gz",
            "irori-backup-20260103-000000.tar.gz",
            "pre-restore-20260101-000000.tar.gz",
        ] {
            fs::write(tmp.path().join(name), "x")?;
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(prune(tmp.path(), 1), 2);
        let left: Vec<_> = list(tmp.path()).into_iter().map(|e| e.name).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"irori-backup-20260103-000000.tar.gz".to_owned()));
        assert!(left.contains(&"pre-restore-20260101-000000.tar.gz".to_owned()));
        Ok(())
    }

    #[test]
    fn only_our_own_names_may_be_asked_for() {
        assert!(is_backup_name("irori-backup-20260101-000000.tar.gz"));
        assert!(!is_backup_name("../irori-backup-x.tar.gz"));
        assert!(!is_backup_name("irori-backup-../../x.tar.gz"));
        assert!(!is_backup_name("secrets.toml"));
    }
}
