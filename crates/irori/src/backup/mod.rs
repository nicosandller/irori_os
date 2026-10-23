//! Backup and restore (`docs/specs/backup.md`).
//!
//! A backup is one file holding `manifest.json`, `config/` and `data/`, as a gzipped tar, and
//! sealed with a recovery key unless that is turned off. The database is never copied as a file:
//! it is in WAL mode with several connections open, so a snapshot is made with `VACUUM INTO`,
//! which is consistent and stops nothing.
//!
//! The archive is read in this process and every member is written by us, into a directory we
//! made: a link in it is refused, a name that leaves the directory is refused, and what comes
//! out is counted against the room there is, so a small file can't fill a disk.
//!
//! A restore is staged, then applied the next time Irori starts, before the database or the
//! config are opened (`apply_pending`). Nothing replaces files a running server has open.

pub mod crypt;
pub mod dest;
pub mod schedule;
pub mod settings;

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, bail, ensure};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The archive layout this build writes and reads.
const FORMAT: u32 = 2;
/// What a scheduled or requested backup is named: `irori-backup-<UTC date>-<time>`.
pub const PREFIX: &str = "irori-backup-";
/// What restore keeps of the home it is about to replace. Not pruned with the others.
pub const SAFETY_PREFIX: &str = "pre-restore-";
/// What is made just before an update. Kept three at a time, apart from the scheduled ones.
pub const UPDATE_PREFIX: &str = "pre-update-";
const SUFFIX_PLAIN: &str = ".tar.gz";
const SUFFIX_SEALED: &str = ".tar.gz.enc";
pub const DEFAULT_DIR: &str = "backups";
const PENDING: &str = "restore-pending";
const RESULT: &str = "restore-result.json";
/// The most an archive may unpack to, however much room there is.
const MAX_UNPACKED: u64 = 64 << 30;
const MAX_ENTRIES: usize = 500_000;

/// Top-level entries of the data directory that a backup leaves out: models and logs that are
/// big or rebuilt, installed packages (fetched again for this machine's CPU), the CLI's pointer
/// to a running server, and the backups and a restore waiting to happen.
const SKIPPED_DATA: &[&str] = &[
    "ollama",
    "ollama.log",
    "ollama.pid",
    "extensions",
    "cli.url",
    DEFAULT_DIR,
    PENDING,
    RESULT,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub irori_version: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    /// Packages that were installed. They are not in the archive: they are fetched again.
    pub extensions: Vec<String>,
    pub encrypted: bool,
    /// Whether entity history was put in.
    pub history: bool,
    /// Whether the activity log is in the database: who did what in the home, as it stood. A
    /// restore leaves the one the home has unless it is asked to bring this one.
    #[serde(default)]
    pub activity: bool,
}

/// What a listing shows about a file, without opening it.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub bytes: u64,
    pub modified: String,
    pub encrypted: bool,
    /// `None` for a file that couldn't be read as a backup.
    pub manifest: Option<Manifest>,
}

/// The backup is sealed and the key wasn't given.
#[derive(Debug)]
pub struct NeedsKey;

impl std::fmt::Display for NeedsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this backup is encrypted: give its recovery key")
    }
}

impl std::error::Error for NeedsKey {}

/// What to put in a backup.
#[derive(Debug, Clone, Copy)]
pub struct Plan<'a> {
    /// Seal with this key. `None` writes it readable.
    pub key: Option<&'a str>,
    pub include_history: bool,
}

impl Plan<'_> {
    pub const PLAIN: Plan<'static> = Plan {
        key: None,
        include_history: true,
    };
}

/// Where backups kept on this machine go.
pub fn dir(data: &Path) -> PathBuf {
    data.join(DEFAULT_DIR)
}

/// Writes a backup of this home into `into` and returns its path. Safe while the server runs.
pub fn create(config: &Path, data: &Path, into: &Path, plan: Plan) -> anyhow::Result<PathBuf> {
    create_named(config, data, into, PREFIX, plan)
}

pub fn create_named(
    config: &Path,
    data: &Path,
    into: &Path,
    prefix: &str,
    plan: Plan,
) -> anyhow::Result<PathBuf> {
    make_private_dir(into)?;
    let now = jiff::Timestamp::now();
    let stamp = now.strftime("%Y%m%d-%H%M%S").to_string();
    let suffix = if plan.key.is_some() {
        SUFFIX_SEALED
    } else {
        SUFFIX_PLAIN
    };
    // Two in the same second (a double click, a restore straight after another) both get kept.
    let mut target = into.join(format!("{prefix}{stamp}{suffix}"));
    for n in 2..100 {
        if !target.exists() {
            break;
        }
        target = into.join(format!("{prefix}{stamp}-{n}{suffix}"));
    }
    ensure!(!target.exists(), "{} already exists", target.display());

    // Next to the result, so the finished file is moved by a rename and nobody sees half of it.
    // Dot-named, so a listing never takes it for a backup.
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(into)
        .context("couldn't make a staging directory")?;
    let root = staging.path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;

    // Dot-entries at the top of the config directory are someone else's: `.git`, mostly.
    copy_tree(config, &root.join("config"), &|rel| {
        rel.components().count() == 1 && starts_with_dot(rel)
    })?;
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
                || first.starts_with('.')
                || own_dir.as_deref().is_some_and(|own| own == first.as_str()))
    })?;
    snapshot_database(
        &data.join("irori.db"),
        &staged_data.join("irori.db"),
        plan.include_history,
    )?;

    let manifest = Manifest {
        format: FORMAT,
        irori_version: crate::build_info::VERSION.to_owned(),
        created_at: now.to_string(),
        extensions: installed_extensions(data),
        encrypted: plan.key.is_some(),
        history: plan.include_history,
        // It is in the database snapshot whole: unlike history, it is small, and it is the one
        // record of who changed the home.
        activity: true,
    };
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let part = into.join(format!(
        ".{}.part",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let written = (|| -> anyhow::Result<()> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)?;
        match plan.key {
            Some(key) => {
                let sealed = crypt::encrypt_to(io::BufWriter::new(file), key, manifest.clone())?;
                let sealed = build_archive(sealed, root)?;
                sealed.finish()?.flush()?;
            }
            None => {
                let mut file = build_archive(io::BufWriter::new(file), root)?;
                file.flush()?;
            }
        }
        Ok(())
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&part);
        return Err(error.context("couldn't write the backup"));
    }
    fs::rename(&part, &target)?;
    Ok(target)
}

fn build_archive<W: Write>(out: W, staging: &Path) -> io::Result<W> {
    let gz = GzEncoder::new(out, Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);
    tar.follow_symlinks(false);
    // First, so a listing finds it without reading on.
    tar.append_path_with_name(staging.join("manifest.json"), "manifest.json")?;
    tar.append_dir_all("config", staging.join("config"))?;
    tar.append_dir_all("data", staging.join("data"))?;
    tar.into_inner()?.finish()
}

/// What a file is, from its first bytes and (for a readable one) its first member. Needs no key.
pub fn inspect(path: &Path) -> anyhow::Result<(bool, Manifest)> {
    let mut file = fs::File::open(path)?;
    let mut head = [0_u8; 8];
    let got = file.read(&mut head)?;
    if crypt::is_encrypted(&head[..got]) {
        let mut file = fs::File::open(path)?;
        let (header, _) = crypt::read_header(&mut file)?;
        return Ok((true, header.meta));
    }
    let file = fs::File::open(path)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut first = archive
        .entries()
        .context("not a backup")?
        .next()
        .context("the file is empty")?
        .context("not a backup")?;
    ensure!(
        first.path()?.as_os_str() == "manifest.json",
        "{} has no manifest.json first; it isn't an Irori backup",
        path.display()
    );
    let mut bytes = Vec::new();
    (&mut first).take(1 << 20).read_to_end(&mut bytes)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("manifest.json isn't readable")?;
    Ok((false, manifest))
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
            let info = inspect(&item.path()).ok();
            let stamp = jiff::Timestamp::try_from(modified).ok()?;
            Some((
                modified,
                Entry {
                    encrypted: info.as_ref().map_or(name.ends_with(SUFFIX_SEALED), |i| i.0),
                    manifest: info.map(|i| i.1),
                    name,
                    bytes: meta.len(),
                    modified: stamp.to_string(),
                },
            ))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.name.cmp(&a.1.name)));
    found.into_iter().map(|(_, entry)| entry).collect()
}

/// Whether `name` is something this module wrote, and so something a request may name.
pub fn is_backup_name(name: &str) -> bool {
    (name.starts_with(PREFIX) || name.starts_with(SAFETY_PREFIX) || name.starts_with(UPDATE_PREFIX))
        && (name.ends_with(SUFFIX_PLAIN) || name.ends_with(SUFFIX_SEALED))
        && name.len() < 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !name.contains("..")
}

/// Which `names` (newest first, `irori-backup-*` only) retention deletes: all past the first
/// `keep`, and any older than `keep_days` days, but never the newest.
pub fn expired(
    names_and_times: &[(String, jiff::Timestamp)],
    prefix: &str,
    keep: u32,
    keep_days: Option<u32>,
    now: jiff::Timestamp,
) -> Vec<String> {
    names_and_times
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .enumerate()
        .filter(|(index, (_, at))| {
            *index > 0
                && (*index >= keep.max(1) as usize
                    || keep_days.is_some_and(|days| {
                        now.as_second() - at.as_second() > i64::from(days) * 86_400
                    }))
        })
        .map(|(_, (name, _))| name.clone())
        .collect()
}

/// Deletes backups past retention in `dir`. Safety copies stay. Returns how many went.
pub fn prune(dir: &Path, keep: u32, keep_days: Option<u32>) -> usize {
    prune_kind(dir, PREFIX, keep, keep_days)
}

/// Retention for one kind of backup, by its name's prefix.
pub fn prune_kind(dir: &Path, prefix: &str, keep: u32, keep_days: Option<u32>) -> usize {
    let entries: Vec<(String, jiff::Timestamp)> = list(dir)
        .into_iter()
        .filter_map(|entry| {
            let at = entry.modified.parse().ok()?;
            Some((entry.name, at))
        })
        .collect();
    let mut removed = 0;
    for name in expired(&entries, prefix, keep, keep_days, jiff::Timestamp::now()) {
        if fs::remove_file(dir.join(&name)).is_ok() {
            removed += 1;
        }
    }
    removed
}

// ----- opening and unpacking -----

/// The archive's bytes, opened: decrypted if it is sealed.
fn open_stream(path: &Path, key: Option<&str>) -> anyhow::Result<Box<dyn Read>> {
    let mut file = io::BufReader::new(fs::File::open(path)?);
    let head = io::BufRead::fill_buf(&mut file)?;
    if !crypt::is_encrypted(&head[..head.len().min(8)]) {
        return Ok(Box::new(file));
    }
    let key = key.filter(|k| !k.is_empty()).ok_or(NeedsKey)?;
    let (header, aad) = crypt::read_header(&mut file)?;
    let reader = crypt::decrypt_from(file, key, &header, aad).map_err(anyhow::Error::msg)?;
    Ok(Box::new(reader))
}

/// Writes the archive's members into `dest`, which must be new and empty, and returns its
/// manifest. Every member is a regular file or a directory under `manifest.json`, `config/` or
/// `data/`; anything else is refused, and so is more than `max_bytes` of content.
fn unpack(path: &Path, key: Option<&str>, dest: &Path, max_bytes: u64) -> anyhow::Result<Manifest> {
    let reader = open_stream(path, key)?;
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    let mut total = 0_u64;
    let mut count = 0_usize;
    let mut manifest = None;
    for entry in archive.entries().context("not a backup")? {
        let mut entry = entry.map_err(wrong_key)?;
        count += 1;
        ensure!(count <= MAX_ENTRIES, "the backup holds too many files");
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            continue;
        }
        let rel = member_path(&entry.path()?)?;
        let target = dest.join(&rel);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
            continue;
        }
        ensure!(
            kind.is_file(),
            "the backup holds something other than files and directories: {}",
            rel.display()
        );
        let room = max_bytes.saturating_sub(total);
        ensure!(
            entry.size() <= room,
            "the backup unpacks to more than there is room for"
        );
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
            .with_context(|| format!("{} is in the backup twice", rel.display()))?;
        // One byte past the room is how a member that lied about its size is caught.
        let wrote = io::copy(&mut (&mut entry).take(room + 1), &mut out).map_err(wrong_key)?;
        ensure!(
            wrote <= room,
            "the backup unpacks to more than there is room for"
        );
        total += wrote;
        if rel == Path::new("manifest.json") {
            let bytes = fs::read(&target)?;
            manifest = Some(
                serde_json::from_slice::<Manifest>(&bytes)
                    .context("manifest.json isn't readable")?,
            );
        }
    }
    manifest.context("there is no manifest.json; this isn't an Irori backup")
}

/// A sealed stream that doesn't open surfaces as an I/O error from deep in the reader.
fn wrong_key(error: io::Error) -> anyhow::Error {
    if error.kind() == io::ErrorKind::InvalidData {
        anyhow::anyhow!("{error}")
    } else {
        anyhow::Error::new(error)
    }
}

fn member_path(path: &Path) -> anyhow::Result<PathBuf> {
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => clean.push(name),
            _ => bail!("the backup has a suspicious member `{}`", path.display()),
        }
    }
    let first = clean
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str().map(str::to_owned));
    ensure!(
        matches!(first.as_deref(), Some("manifest.json" | "config" | "data")),
        "the backup has a suspicious member `{}`",
        path.display()
    );
    Ok(clean)
}

/// How much unpacking may write: what there is room for, but at most `MAX_UNPACKED`.
fn room_at(path: &Path) -> u64 {
    use rustix::fs::statvfs;
    let free = statvfs(path)
        .map(|v| v.f_bavail.saturating_mul(v.f_frsize))
        .unwrap_or(MAX_UNPACKED);
    (free / 10 * 9).min(MAX_UNPACKED)
}

fn newer_than_us(theirs: &str) -> bool {
    fn parts(version: &str) -> Option<(u64, u64, u64)> {
        let core = version.split(['-', '+']).next()?;
        let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
        Some((it.next()??, it.next()??, it.next()??))
    }
    // `0.0.0` is a development build and reads anything.
    match (parts(theirs), parts(crate::build_info::VERSION)) {
        (Some(theirs), Some(ours)) if ours != (0, 0, 0) => theirs > ours,
        _ => false,
    }
}

// ----- verify -----

/// What looking inside a backup found.
#[derive(Debug, Serialize)]
pub struct Verified {
    pub manifest: Manifest,
    /// Things to know that don't stop a restore: a config file that doesn't load, say.
    pub warnings: Vec<String>,
    pub files: usize,
}

/// Opens the backup the way a restore would, in a scratch directory, and checks what is in it.
pub fn verify(path: &Path, key: Option<&str>, scratch_in: &Path) -> anyhow::Result<Verified> {
    fs::create_dir_all(scratch_in)?;
    let scratch = tempfile::Builder::new()
        .prefix(".verify-")
        .tempdir_in(scratch_in)?;
    let manifest = unpack(path, key, scratch.path(), room_at(scratch_in))?;
    checked(&manifest, scratch.path())
}

fn checked(manifest: &Manifest, root: &Path) -> anyhow::Result<Verified> {
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
    // A config that doesn't load is what the home was. Say so; don't refuse to give it back.
    let mut store = irori_config::Store::new(root.join("config"));
    let warnings = store
        .reload()
        .iter()
        .map(|p| format!("{}: {}", p.file, p.reason))
        .collect();
    let mut warnings: Vec<String> = warnings;
    if opens_the_lan(&root.join("config/irori.toml")) {
        warnings.push(
            "irori.toml in this backup has allow_unauthenticated_lan on: restoring settings turns it back on, and anyone on the network can then use this home without signing in"
                .to_owned(),
        );
    }
    Ok(Verified {
        manifest: manifest.clone(),
        warnings,
        files: count_files(root),
    })
}

/// Whether an `irori.toml` lets the network in without a sign-in.
fn opens_the_lan(file: &Path) -> bool {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|t| t.get("server")?.get("allow_unauthenticated_lan")?.as_bool())
        .unwrap_or(false)
}

fn count_files(root: &Path) -> usize {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path()),
            Ok(_) => 1,
            Err(_) => 0,
        })
        .sum()
}

// ----- restore -----

/// What of a backup to put back. A part left out stays as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parts {
    /// `config/` (rooms, names, flows, users, secrets) and the https certificate.
    pub settings: bool,
    /// What extensions remember (`data/extension-data/`, and any other state beside the database).
    pub extensions: bool,
    /// The database except entity history: sessions, tokens, the assistant's chats, what
    /// extensions keep.
    pub state: bool,
    /// Entity history.
    pub history: bool,
    /// The activity log (`docs/specs/access.md` §3). Like history it stays as it is now unless
    /// asked for: a backup from last month would otherwise take the last month out of the
    /// record of who changed the home.
    #[serde(default)]
    pub activity: bool,
}

impl Parts {
    /// Everything there is, the activity log included: a home with nothing yet (the welcome's
    /// restore), and the way back to a safety copy.
    pub const ALL: Parts = Parts {
        settings: true,
        extensions: true,
        state: true,
        history: true,
        activity: true,
    };

    /// What a restore puts back when nothing is chosen: all of the home, and the log of what
    /// has been done to it stays.
    pub const USUAL: Parts = Parts {
        activity: false,
        ..Parts::ALL
    };

    pub fn any(self) -> bool {
        self.settings || self.extensions || self.state || self.history || self.activity
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Plan2 {
    archive: String,
    parts: Parts,
    manifest: Manifest,
    warnings: Vec<String>,
    staged_at: String,
}

/// What a restore did, written for the page to say after Irori is back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restored {
    pub ok: bool,
    pub at: String,
    pub archive: String,
    pub parts: Parts,
    pub manifest: Option<Manifest>,
    pub warnings: Vec<String>,
    /// The home as it was, kept as a backup, when there was one to keep.
    pub safety_copy: Option<String>,
    pub error: Option<String>,
    /// Extensions that were installed when the backup was made and have been fetched again.
    #[serde(default)]
    pub reinstalled: Vec<String>,
    /// Extensions that need full access to this machine: not fetched until a person approves.
    #[serde(default)]
    pub awaiting_approval: Vec<String>,
    /// Extensions that couldn't be fetched, and why.
    #[serde(default)]
    pub reinstall_failed: Vec<String>,
    /// Whether the extensions above have been looked after yet.
    #[serde(default)]
    pub extensions_handled: bool,
    /// Whether the activity log has been told about it yet.
    #[serde(default)]
    pub audited: bool,
}

/// Opens `archive`, checks it, and leaves it in `<data>/restore-pending` for the next start.
/// Nothing the running server uses is touched.
pub fn stage(
    archive: &Path,
    key: Option<&str>,
    parts: Parts,
    data: &Path,
) -> anyhow::Result<Verified> {
    ensure!(archive.is_file(), "{} is not a file", archive.display());
    fs::create_dir_all(data)?;
    let pending = data.join(PENDING);
    if pending.exists() {
        fs::remove_dir_all(&pending)?;
    }
    fs::create_dir_all(&pending)?;
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o700))?;
    let tree = pending.join("tree");
    fs::create_dir_all(&tree)?;
    let result = (|| {
        let manifest = unpack(archive, key, &tree, room_at(data))?;
        let verified = checked(&manifest, &tree)?;
        let plan = Plan2 {
            archive: archive
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            parts,
            manifest,
            warnings: verified.warnings.clone(),
            staged_at: jiff::Timestamp::now().to_string(),
        };
        // Written last: its presence is what says the tree is whole.
        fs::write(pending.join("plan.json"), serde_json::to_vec_pretty(&plan)?)?;
        Ok(verified)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    result
}

/// Whether a restore is waiting for the next start.
pub fn pending(data: &Path) -> bool {
    data.join(PENDING).join("plan.json").exists()
}

/// Forgets a staged restore.
pub fn cancel_pending(data: &Path) {
    let _ = fs::remove_dir_all(data.join(PENDING));
}

/// What the last restore did, if one has happened here.
pub fn last_restore(data: &Path) -> Option<Restored> {
    serde_json::from_slice(&fs::read(data.join(RESULT)).ok()?).ok()
}

/// Forgets the last restore's result, once it has been shown.
pub fn clear_last_restore(data: &Path) {
    let _ = fs::remove_file(data.join(RESULT));
}

/// Applies a staged restore, if there is one. Called first thing at start, with nothing open.
/// `None` when nothing was waiting.
pub fn apply_pending(config: &Path, data: &Path) -> Option<Restored> {
    let pending = data.join(PENDING);
    let plan: Plan2 = match fs::read(pending.join("plan.json")) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(plan) => plan,
            Err(_) => {
                let _ = fs::remove_dir_all(&pending);
                return None;
            }
        },
        Err(_) => {
            // A tree with no plan was never finished.
            let _ = fs::remove_dir_all(&pending);
            return None;
        }
    };
    let mut done = Restored {
        ok: false,
        at: jiff::Timestamp::now().to_string(),
        archive: plan.archive.clone(),
        parts: plan.parts,
        manifest: Some(plan.manifest.clone()),
        warnings: plan.warnings.clone(),
        safety_copy: None,
        error: None,
        reinstalled: Vec::new(),
        awaiting_approval: Vec::new(),
        reinstall_failed: Vec::new(),
        extensions_handled: false,
        audited: false,
    };
    match swap_in(&pending.join("tree"), config, data, plan.parts) {
        Ok(safety) => {
            done.ok = true;
            done.safety_copy = safety;
        }
        Err(error) => done.error = Some(format!("{error:#}")),
    }
    let _ = fs::remove_dir_all(&pending);
    if let Ok(bytes) = serde_json::to_vec_pretty(&done) {
        let _ = fs::write(data.join(RESULT), bytes);
    }
    Some(done)
}

fn swap_in(
    tree: &Path,
    config: &Path,
    data: &Path,
    parts: Parts,
) -> anyhow::Result<Option<String>> {
    fs::create_dir_all(data)?;
    // Keep the home that is there, whole, whatever part is being put back.
    let had_home = config.is_dir() || data.join("irori.db").exists();
    let safety = if had_home {
        let file = create_named(config, data, &dir(data), SAFETY_PREFIX, Plan::PLAIN)
            .context("couldn't keep the current home first, so nothing was replaced")?;
        file.file_name().map(|n| n.to_string_lossy().into_owned())
    } else {
        None
    };

    if let Err(error) = put_back(tree, config, data, parts) {
        // Half a restore is worse than none: put the home back as the safety copy had it.
        let Some(name) = &safety else {
            return Err(error);
        };
        return Err(match roll_back(&dir(data).join(name), config, data) {
            Ok(()) => error.context("the restore failed; your home was put back as it was"),
            Err(back) => error.context(format!(
                "the restore failed, and putting your home back failed too ({back:#}); \
                 {name} in the backups folder holds it as it was"
            )),
        });
    }
    // The safety copies are for a way back, not for ever: the last three stay.
    prune_kind(&dir(data), SAFETY_PREFIX, 3, None);
    Ok(safety)
}

/// Puts the home as `archive` (a safety copy) had it.
fn roll_back(archive: &Path, config: &Path, data: &Path) -> anyhow::Result<()> {
    let tree = data.join("restore-rollback");
    let _ = fs::remove_dir_all(&tree);
    fs::create_dir_all(&tree)?;
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o700))?;
    let result = unpack(archive, None, &tree, room_at(data))
        .and_then(|_| put_back(&tree, config, data, Parts::ALL));
    let _ = fs::remove_dir_all(&tree);
    result
}

/// Puts the chosen parts of the unpacked backup at `tree` in place of the home's own.
fn put_back(tree: &Path, config: &Path, data: &Path, parts: Parts) -> anyhow::Result<()> {
    if parts.settings {
        // Replaced whole: a file the backup lacks is a file that wasn't there. Dot-entries are
        // someone else's (`.git`) and stay.
        if config.exists() {
            for item in fs::read_dir(config)? {
                let item = item?;
                if !item.file_name().to_string_lossy().starts_with('.') {
                    remove_any(&item.path())?;
                }
            }
        }
        copy_tree(&tree.join("config"), config, &|rel| {
            rel.components().count() == 1 && starts_with_dot(rel)
        })?;
        replace_entry(&tree.join("data/tls"), &data.join("tls"))?;
    }
    if parts.extensions {
        let staged = tree.join("data");
        let mut taken = Vec::new();
        if staged.is_dir() {
            for item in fs::read_dir(&staged)? {
                let name = item?.file_name();
                let text = name.to_string_lossy();
                if text.starts_with("irori.db") || text == "tls" || text.starts_with('.') {
                    continue;
                }
                taken.push(name);
            }
        }
        // What the home keeps for its extensions, whole: a backup without `extension-data`
        // means there was none.
        if !taken.iter().any(|n| n == "extension-data") {
            let _ = remove_any(&data.join("extension-data"));
        }
        for name in taken {
            replace_entry(&tree.join("data").join(&name), &data.join(&name))?;
        }
    }
    if parts.state || parts.history || parts.activity {
        compose_database(&data.join("irori.db"), &tree.join("data/irori.db"), parts)?;
    }
    Ok(())
}

/// Builds `irori.db` from the backup's and the current one: each of the three kinds of thing in
/// it (state, history, the activity log) from whichever the parts say. Nothing is replaced until
/// the new file is whole.
fn compose_database(current: &Path, staged: &Path, parts: Parts) -> anyhow::Result<()> {
    if !(parts.state || parts.history || parts.activity) || !staged.exists() {
        return Ok(());
    }
    let out = current.with_file_name("irori.db.restoring");
    let _ = fs::remove_file(&out);
    let sql_path = |p: &Path| -> anyhow::Result<String> {
        Ok(p.to_str()
            .context("the path isn't UTF-8")?
            .replace('\'', "''"))
    };
    if (parts.state && parts.history && parts.activity) || !current.exists() {
        // The backup's database is a finished file of its own (it was written by VACUUM INTO).
        fs::copy(staged, &out)?;
        // A backup from before the activity log existed has no table for it, and must not take
        // the home's own rows with it.
        if current.exists() {
            let conn = Connection::open(&out)?;
            conn.execute_batch(&format!(
                "ATTACH DATABASE '{}' AS other",
                sql_path(current)?
            ))?;
            for table in crate::audit::TABLES {
                let in_backup: i64 = conn.query_row(
                    "SELECT count(*) FROM main.sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )?;
                if in_backup == 0 {
                    copy_table(&conn, table)?;
                }
            }
            conn.execute_batch("DETACH DATABASE other")?;
        }
    } else {
        // The base is the file that has the rest (everything but the two kinds below), which is
        // the backup's when the state is asked for. The tables of a kind the base is not the
        // source of are then copied across from the other file.
        let (base, other) = if parts.state {
            (staged, current)
        } else {
            (current, staged)
        };
        let base_is_staged = parts.state;
        let conn = Connection::open_with_flags(base, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.execute_batch(&format!("VACUUM INTO '{}'", sql_path(&out)?))?;
        drop(conn);
        let conn = Connection::open(&out)?;
        conn.execute_batch(&format!("ATTACH DATABASE '{}' AS other", sql_path(other)?))?;
        for (wanted_from_staged, tables) in [
            (parts.history, irori_recorder::HISTORY_TABLES),
            (parts.activity, crate::audit::TABLES),
        ] {
            if wanted_from_staged == base_is_staged {
                continue;
            }
            for table in tables {
                copy_table(&conn, table)?;
            }
        }
        conn.execute_batch("DETACH DATABASE other")?;
    }
    for stale in ["irori.db", "irori.db-wal", "irori.db-shm"] {
        let _ = fs::remove_file(current.with_file_name(stale));
    }
    fs::rename(&out, current)?;
    Ok(())
}

/// Replaces `table` in `main` with the one in `other`, making it first if `main` has none (a
/// backup from before the table existed must not take the home's own rows with it).
fn copy_table(conn: &Connection, table: &str) -> anyhow::Result<()> {
    let has = |schema: &str| -> anyhow::Result<bool> {
        let n: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"
            ),
            [table],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    };
    if !has("other")? {
        // Nothing to bring across; what `main` has stays.
        return Ok(());
    }
    if !has("main")? {
        let mut schema = conn.prepare(
            "SELECT sql FROM other.sqlite_master WHERE tbl_name = ?1 AND sql IS NOT NULL \
             ORDER BY (type = 'table') DESC",
        )?;
        let statements: Vec<String> = schema
            .query_map([table], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        for statement in statements {
            conn.execute_batch(&statement)?;
        }
    }
    conn.execute_batch(&format!(
        "DELETE FROM main.{table}; INSERT INTO main.{table} SELECT * FROM other.{table};"
    ))?;
    Ok(())
}

/// Replaces `to` with `from`, or removes `to` when the backup has nothing there.
fn replace_entry(from: &Path, to: &Path) -> anyhow::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        remove_any(to)?;
    }
    match fs::symlink_metadata(from) {
        Ok(meta) if meta.is_dir() => copy_tree(from, to, &|_| false)?,
        Ok(meta) if meta.is_file() => copy_private(from, to)?,
        _ => {}
    }
    Ok(())
}

/// After a restore, writes the row that says it was done, or why it wasn't, into the activity log
/// as it now stands (`docs/specs/access.md` §3). It is Irori that did it, at the start after the
/// one that was asked: the row for the asking is in the log the home had then, which a restore
/// that brings the backup's log back replaces. Once.
pub fn record_restore(audit: &crate::audit::Audit, data: &Path) {
    use irori_types::{AuditActor, Outcome};
    let Some(mut done) = last_restore(data) else {
        return;
    };
    if done.audited {
        return;
    }
    let put_back: Vec<&str> = [
        (done.parts.settings, "settings"),
        (done.parts.extensions, "extension data"),
        (done.parts.state, "sessions and chats"),
        (done.parts.history, "history"),
        (done.parts.activity, "the activity log"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    let mut detail = format!("{}: {}", done.archive, put_back.join(", "));
    if !done.parts.activity {
        detail.push_str("; the activity log was left as it was");
    }
    if let Some(error) = &done.error {
        detail = format!("{}: {error}", done.archive);
    }
    let outcome = if done.ok {
        Outcome::Ok
    } else {
        Outcome::Failed
    };
    audit.record(
        crate::audit::Row::new(AuditActor::Irori, "restore_applied", outcome)
            .target(done.archive.clone())
            .detail(detail),
    );
    done.audited = true;
    if let Ok(bytes) = serde_json::to_vec_pretty(&done) {
        let _ = fs::write(data.join(RESULT), bytes);
    }
}

/// After a restore, fetches again the official extensions the backup's home had installed. One
/// that asks for full access to the machine is not fetched: a person approves that, and a
/// backup is not a person. Runs once; the result is written for the page to say.
pub async fn reinstall_extensions(host: irori_core::ExtensionHost, data: PathBuf) {
    let Some(mut done) = last_restore(&data) else {
        return;
    };
    if !done.ok || done.extensions_handled {
        return;
    }
    let Some(manifest) = done.manifest.clone() else {
        return;
    };
    for name in missing_extensions(&data, &manifest) {
        let Some(item) = irori_types::ExtensionId::try_from(name.as_str())
            .ok()
            .and_then(|id| crate::packages::official_by_id(&id))
        else {
            done.reinstall_failed
                .push(format!("{name}: not an official extension"));
            continue;
        };
        if item.full_access {
            done.awaiting_approval.push(name);
            continue;
        }
        // One somebody is installing right now (the Extensions page, the CLI), or has just
        // installed, isn't a failure: it's on its way, by a different hand.
        match crate::server::place_official(host.clone(), item.clone(), false, false).await {
            Ok(()) => done.reinstalled.push(name),
            Err(crate::server::NotPlaced::Underway(underway)) => done.warnings.push(format!(
                "{name} was already being {} at the same time, and was left to finish that",
                match underway.work {
                    irori_core::PackageWork::Installing => "installed",
                    irori_core::PackageWork::Updating => "updated",
                    irori_core::PackageWork::Uninstalling => "uninstalled",
                }
            )),
            Err(crate::server::NotPlaced::Installed(_)) => done
                .warnings
                .push(format!("{name} was installed meanwhile")),
            Err(why) => done.reinstall_failed.push(format!("{name}: {}", why.why())),
        }
    }
    done.extensions_handled = true;
    if let Ok(bytes) = serde_json::to_vec_pretty(&done) {
        let _ = fs::write(data.join(RESULT), bytes);
    }
}

/// Whether a server is answering at `<data>/cli.url`.
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

// ----- files -----

fn snapshot_database(from: &Path, to: &Path, history: bool) -> anyhow::Result<()> {
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
    let copy = Connection::open(to)?;
    let verdict: String = copy.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    ensure!(
        verdict == "ok",
        "the database is damaged ({verdict}); not backing it up"
    );
    if !history {
        for table in irori_recorder::HISTORY_TABLES {
            let _ = copy.execute_batch(&format!("DELETE FROM {table}"));
        }
        copy.execute_batch("VACUUM")?;
    }
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

/// The official extensions a restore brought back the settings of but not the programs of.
pub fn missing_extensions(data: &Path, manifest: &Manifest) -> Vec<String> {
    let have = installed_extensions(data);
    manifest
        .extensions
        .iter()
        .filter(|id| !have.contains(id))
        .cloned()
        .collect()
}

fn starts_with_dot(rel: &Path) -> bool {
    rel.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("couldn't create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn remove_any(path: &Path) -> io::Result<()> {
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
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut source = fs::File::open(from)?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(to)?;
    io::copy(&mut source, &mut target)?;
    Ok(())
}

#[cfg(test)]
mod tests;
