use super::*;

/// A config dir and a data dir with a real WAL database, a package, and some noise.
fn home() -> anyhow::Result<tempfile::TempDir> {
    let tmp = tempfile::tempdir()?;
    let (config, data) = (tmp.path().join("config"), tmp.path().join("data"));
    fs::create_dir_all(config.join("flows"))?;
    fs::create_dir_all(config.join(".git"))?;
    fs::write(config.join(".git/HEAD"), "ref: refs/heads/main")?;
    fs::write(config.join("areas.toml"), "[areas.hall]\nname = \"Hall\"\n")?;
    fs::write(config.join("flows/a.json"), "{}")?;
    let db = crate::db::open(&data)?;
    let conn = Connection::open(&db.path)?;
    conn.execute_batch(
        "CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (42);
         CREATE TABLE state_history (id INTEGER PRIMARY KEY, entity_id TEXT, updated_ns INTEGER, state TEXT);
         INSERT INTO state_history (entity_id, updated_ns, state) VALUES ('a', 1, 'then');",
    )?;
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

fn paths(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    (tmp.path().join("config"), tmp.path().join("data"))
}

fn names(archive: &Path, key: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut ar = tar::Archive::new(GzDecoder::new(open_stream(archive, key)?));
    let mut out = Vec::new();
    for entry in ar.entries()? {
        out.push(
            entry?
                .path()?
                .to_string_lossy()
                .trim_start_matches("./")
                .to_owned(),
        );
    }
    Ok(out)
}

fn sql(data: &Path, q: &str) -> anyhow::Result<String> {
    let conn = Connection::open(data.join("irori.db"))?;
    Ok(conn.query_row(q, [], |r| r.get::<_, i64>(0))?.to_string())
}

fn restore_now(
    archive: &Path,
    key: Option<&str>,
    parts: Parts,
    config: &Path,
    data: &Path,
) -> anyhow::Result<Restored> {
    stage(archive, key, parts, data)?;
    let done = apply_pending(config, data).context("nothing was staged")?;
    ensure!(done.ok, "the restore failed: {:?}", done.error);
    Ok(done)
}

#[test]
fn a_backup_holds_state_and_leaves_out_what_is_rebuilt_or_not_ours() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let out = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    assert_eq!(fs::metadata(&out)?.permissions().mode() & 0o777, 0o600);
    let all = names(&out, None)?;
    for want in [
        "manifest.json",
        "config/areas.toml",
        "config/flows/a.json",
        "data/irori.db",
        "data/extension-data/zigbee/key",
    ] {
        assert!(all.iter().any(|n| n == want), "{want} in {all:?}");
    }
    for not in [
        "ollama",
        "cli.url",
        "data/extensions",
        "backups",
        "-wal",
        "-shm",
        ".staging",
        ".git",
    ] {
        assert!(!all.iter().any(|n| n.contains(not)), "{not} in {all:?}");
    }
    assert_eq!(all[0], "manifest.json");
    let (sealed, manifest) = inspect(&out)?;
    assert!(!sealed);
    assert_eq!(manifest.extensions, ["demo"]);
    assert_eq!(list(&dir(&data)).len(), 1);
    Ok(())
}

#[test]
fn history_can_be_left_out_to_make_a_smaller_backup() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let out = create(
        &config,
        &data,
        &dir(&data),
        Plan {
            key: None,
            include_history: false,
        },
    )?;
    assert!(!inspect(&out)?.1.history);
    let fresh = tempfile::tempdir()?;
    let (c2, d2) = (fresh.path().join("c"), fresh.path().join("d"));
    restore_now(&out, None, Parts::ALL, &c2, &d2)?;
    assert_eq!(sql(&d2, "SELECT n FROM t")?, "42");
    assert_eq!(sql(&d2, "SELECT count(*) FROM state_history")?, "0");
    Ok(())
}

#[test]
fn a_sealed_backup_needs_its_key_and_says_so_without_it() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let key = crypt::generate_key();
    let out = create(
        &config,
        &data,
        &dir(&data),
        Plan {
            key: Some(&key),
            include_history: true,
        },
    )?;
    assert!(out.to_string_lossy().ends_with(".tar.gz.enc"));
    // Listing needs no key; the file shows nothing of the home.
    let (sealed, manifest) = inspect(&out)?;
    assert!(sealed && manifest.encrypted);
    let bytes = fs::read(&out)?;
    for secret in ["areas.toml", "network-key", "SQLite format"] {
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{secret} is readable"
        );
    }
    let no_key = stage(&out, None, Parts::ALL, &data).expect_err("refused");
    assert!(no_key.downcast_ref::<NeedsKey>().is_some(), "{no_key:#}");
    let wrong = stage(&out, Some("not-it"), Parts::ALL, &data).expect_err("refused");
    assert!(format!("{wrong:#}").contains("wrong key"), "{wrong:#}");
    assert!(!pending(&data), "a refused restore leaves nothing staged");
    // The key, typed loosely, opens it, and the home comes back.
    fs::remove_file(config.join("areas.toml"))?;
    let loose = key.to_lowercase().replace('-', " ");
    let done = restore_now(&out, Some(&loose), Parts::ALL, &config, &data)?;
    assert!(done.ok, "{done:?}");
    assert!(config.join("areas.toml").exists());
    Ok(())
}

#[test]
fn restore_brings_back_what_was_lost_and_keeps_the_home_it_replaced() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    fs::remove_file(config.join("areas.toml"))?;
    fs::write(config.join("extra.toml"), "")?;
    fs::remove_dir_all(data.join("extension-data"))?;
    fs::remove_file(data.join("irori.db"))?;
    let _ = fs::remove_file(data.join("irori.db-wal"));
    let _ = fs::remove_file(data.join("irori.db-shm"));

    let done = restore_now(&backup, None, Parts::ALL, &config, &data)?;
    assert!(done.ok, "{done:?}");
    assert!(config.join("areas.toml").exists());
    assert!(
        !config.join("extra.toml").exists(),
        "a file the backup lacks is gone"
    );
    assert_eq!(
        fs::read_to_string(config.join(".git/HEAD"))?,
        "ref: refs/heads/main",
        "the git history is theirs"
    );
    assert_eq!(
        fs::read_to_string(data.join("extension-data/zigbee/key"))?,
        "network-key"
    );
    assert_eq!(sql(&data, "SELECT n FROM t")?, "42");
    // Packages and models weren't touched.
    assert!(data.join("extensions/demo/bin").exists());
    assert!(data.join("ollama/model").exists());
    let safety = done.safety_copy.expect("there was a home to keep");
    assert!(safety.starts_with(SAFETY_PREFIX) && dir(&data).join(&safety).exists());
    assert!(!pending(&data) && last_restore(&data).is_some_and(|r| r.ok));
    Ok(())
}

#[test]
fn restore_onto_a_new_machine_needs_no_safety_copy() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    let fresh = tempfile::tempdir()?;
    let (c2, d2) = (fresh.path().join("c"), fresh.path().join("d"));
    let done = restore_now(&backup, None, Parts::ALL, &c2, &d2)?;
    assert!(done.safety_copy.is_none());
    assert!(c2.join("flows/a.json").exists() && d2.join("irori.db").exists());
    Ok(())
}

#[test]
fn everything_but_the_history_can_be_restored_keeping_the_history_there_is() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    // Since the backup: the home moved on.
    {
        let conn = Connection::open(data.join("irori.db"))?;
        conn.execute_batch("UPDATE t SET n = 7; INSERT INTO state_history (entity_id, updated_ns, state) VALUES ('a', 2, 'now');")?;
    }
    fs::write(config.join("areas.toml"), "# changed")?;
    let parts = Parts {
        history: false,
        ..Parts::ALL
    };
    restore_now(&backup, None, parts, &config, &data)?;
    assert_eq!(sql(&data, "SELECT n FROM t")?, "42", "state came back");
    assert_eq!(
        sql(&data, "SELECT count(*) FROM state_history")?,
        "2",
        "history stayed"
    );
    assert!(fs::read_to_string(config.join("areas.toml"))?.contains("Hall"));

    // And the other way: only the settings.
    fs::write(config.join("areas.toml"), "# changed again")?;
    let only_settings = Parts {
        settings: true,
        extensions: false,
        state: false,
        history: false,
        activity: false,
    };
    restore_now(&backup, None, only_settings, &config, &data)?;
    assert!(fs::read_to_string(config.join("areas.toml"))?.contains("Hall"));
    assert_eq!(sql(&data, "SELECT n FROM t")?, "42");
    Ok(())
}

/// Now, as the log counts it. A row older than the days it is kept is pruned as the log opens.
fn now() -> i64 {
    i64::try_from(jiff::Timestamp::now().as_nanosecond()).unwrap_or(i64::MAX)
}

/// The activity log is the record of who changed the home, so a restore is not a way to take the
/// last month out of it: it stays as it is unless it is asked for (`docs/specs/access.md` §3).
#[test]
fn the_activity_log_stays_as_it_is_unless_a_restore_asks_for_it() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    // The log, with one row in it, then a backup, then a row more.
    drop(crate::audit::Audit::open(
        &data.join("irori.db"),
        crate::audit::Keep::default(),
    )?);
    let row = |what: &str| -> anyhow::Result<()> {
        Connection::open(data.join("irori.db"))?.execute(
            "INSERT INTO audit (at_ns, actor_kind, action, outcome) VALUES (?1, 'irori', ?2, 'ok')",
            rusqlite::params![now(), what],
        )?;
        Ok(())
    };
    row("then")?;
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    assert!(inspect(&backup)?.1.activity);
    row("since")?;
    let rows = |data: &Path| sql(data, "SELECT count(*) FROM audit");

    // The usual restore: the state goes back, the log is left alone.
    Connection::open(data.join("irori.db"))?.execute_batch("UPDATE t SET n = 7")?;
    restore_now(&backup, None, Parts::USUAL, &config, &data)?;
    assert_eq!(sql(&data, "SELECT n FROM t")?, "42");
    assert_eq!(rows(&data)?, "2", "the row since the backup is still there");

    // Only the settings: the same.
    restore_now(
        &backup,
        None,
        Parts {
            settings: true,
            extensions: false,
            state: false,
            history: false,
            activity: false,
        },
        &config,
        &data,
    )?;
    assert_eq!(rows(&data)?, "2");

    // Asked for, and only that: the log is the backup's and nothing else moves.
    Connection::open(data.join("irori.db"))?.execute_batch("UPDATE t SET n = 9")?;
    restore_now(
        &backup,
        None,
        Parts {
            settings: false,
            extensions: false,
            state: false,
            history: false,
            activity: true,
        },
        &config,
        &data,
    )?;
    assert_eq!(rows(&data)?, "1", "the log as the backup had it");
    assert_eq!(
        sql(&data, "SELECT n FROM t")?,
        "9",
        "and the state is not touched"
    );

    // Everything, on a machine that has nothing: the log comes with it.
    let fresh = tempfile::tempdir()?;
    let (c2, d2) = (fresh.path().join("c"), fresh.path().join("d"));
    restore_now(&backup, None, Parts::ALL, &c2, &d2)?;
    assert_eq!(rows(&d2)?, "1");
    Ok(())
}

/// After the restart that applies a restore, the log says so once, as Irori itself, and says
/// whether the log was brought back or left.
#[tokio::test]
async fn a_restore_is_written_down_once_after_it_is_applied() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    restore_now(&backup, None, Parts::USUAL, &config, &data)?;
    let audit = crate::audit::Audit::open(&data.join("irori.db"), crate::audit::Keep::default())?;
    record_restore(&audit, &data);
    record_restore(&audit, &data);
    let mut rows = Vec::new();
    for _ in 0..200 {
        rows = audit
            .list(&crate::audit::Query {
                action: Some("restore_applied".to_owned()),
                ..Default::default()
            })
            .map_err(anyhow::Error::msg)?
            .entries;
        if !rows.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rows.len(), 1, "once: {rows:?}");
    assert_eq!(rows[0].actor.to_string(), "Irori");
    let said = rows[0].detail.clone().unwrap_or_default();
    assert!(
        said.contains("settings") && said.contains("left as it was"),
        "{said}"
    );
    assert!(last_restore(&data).is_some_and(|r| r.audited));
    Ok(())
}

/// A backup from before the log existed has no table for it. Restoring the state from one must
/// not take the home's own log with it.
#[test]
fn a_backup_from_before_the_log_leaves_the_log_there_is() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    // The backup has the table (this version makes it in every backup), so take it out of the
    // archive's database the way an older version would have left it.
    let staged = tempfile::tempdir()?;
    let tree = staged.path().join("tree");
    fs::create_dir_all(&tree)?;
    unpack(&backup, None, &tree, room_at(&data))?;
    let old = tree.join("data/irori.db");
    Connection::open(&old)?.execute_batch("DROP TABLE IF EXISTS audit")?;
    drop(crate::audit::Audit::open(
        &data.join("irori.db"),
        crate::audit::Keep::default(),
    )?);
    Connection::open(data.join("irori.db"))?.execute(
        "INSERT INTO audit (at_ns, actor_kind, action, outcome) VALUES (?1, 'irori', 'kept', 'ok')",
        [now()],
    )?;
    compose_database(&data.join("irori.db"), &old, Parts::USUAL)?;
    assert_eq!(sql(&data, "SELECT count(*) FROM audit")?, "1");
    assert_eq!(sql(&data, "SELECT n FROM t")?, "42");
    Ok(())
}

#[test]
fn a_home_with_a_config_problem_is_still_backed_up_and_restored() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    fs::write(config.join("floorplan.toml"), "this is = not [valid")?;
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    let done = restore_now(&backup, None, Parts::ALL, &config, &data)?;
    assert!(
        done.ok && done.warnings.iter().any(|w| w.contains("floorplan.toml")),
        "{done:?}"
    );
    Ok(())
}

/// A member to put in a forged archive: name, kind, content, and a link target.
type Member<'a> = (&'a str, tar::EntryType, &'a [u8], Option<&'a str>);

/// A tarball by hand: the member kinds and names `tar` itself refuses to make.
fn forged(
    entries: &[(&str, tar::EntryType, &[u8], Option<&str>)],
    manifest_version: &str,
) -> anyhow::Result<PathBuf> {
    let dir = tempfile::tempdir()?.keep();
    let path = dir.join("irori-backup-forged.tar.gz");
    let mut tar = tar::Builder::new(GzEncoder::new(
        fs::File::create(&path)?,
        Compression::fast(),
    ));
    let manifest = serde_json::to_vec(&Manifest {
        format: FORMAT,
        irori_version: manifest_version.to_owned(),
        created_at: "2026-01-01T00:00:00Z".into(),
        extensions: vec![],
        encrypted: false,
        history: true,
        activity: true,
    })?;
    let mut all: Vec<Member> = vec![("manifest.json", tar::EntryType::Regular, &manifest, None)];
    all.extend_from_slice(entries);
    for (name, kind, data, link) in all {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        // `set_path` would refuse `..`; the point is to write it anyway.
        let raw = header.as_old_mut();
        raw.name[..name.len()].copy_from_slice(name.as_bytes());
        if let Some(link) = link {
            raw.linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_cksum();
        tar.append(&header, data)?;
    }
    tar.into_inner()?.finish()?;
    Ok(path)
}

#[test]
fn an_archive_that_could_escape_or_isnt_ours_is_refused_and_touches_nothing() -> anyhow::Result<()>
{
    use tar::EntryType::{Link, Regular, Symlink};
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let cases: [(&str, Vec<Member>); 6] = [
        ("outside the roots", vec![("evil", Regular, b"x", None)]),
        ("going up", vec![("config/../../evil", Regular, b"x", None)]),
        ("absolute", vec![("/etc/evil", Regular, b"x", None)]),
        (
            "a symlink",
            vec![("config/link", Symlink, b"", Some("/etc/passwd"))],
        ),
        (
            "a hardlink",
            vec![("config/hard", Link, b"", Some("/etc/passwd"))],
        ),
        (
            "twice",
            vec![
                ("config/a", Regular, b"1", None),
                ("config/a", Regular, b"2", None),
            ],
        ),
    ];
    for (what, entries) in cases {
        let bad = forged(&entries, "0.0.0")?;
        let err = stage(&bad, None, Parts::ALL, &data).expect_err(what);
        assert!(!pending(&data), "{what}: {err:#}");
    }
    assert!(config.join("areas.toml").exists());
    assert!(!tmp.path().join("evil").exists());
    Ok(())
}

#[test]
fn an_archive_cannot_unpack_to_more_than_there_is_room_for() -> anyhow::Result<()> {
    let bomb = forged(
        &[(
            "data/big",
            tar::EntryType::Regular,
            &vec![0_u8; 1 << 20],
            None,
        )],
        "0.0.0",
    )?;
    let into = tempfile::tempdir()?;
    let err = unpack(&bomb, None, into.path(), 64 * 1024).expect_err("refused");
    assert!(format!("{err:#}").contains("room"), "{err:#}");
    // And with room it is fine.
    let into = tempfile::tempdir()?;
    unpack(&bomb, None, into.path(), 4 << 20)?;
    Ok(())
}

#[test]
fn a_backup_from_a_newer_irori_is_refused() -> anyhow::Result<()> {
    let tmp = home()?;
    let (_, data) = paths(&tmp);
    let newer = forged(&[], "999.0.0")?;
    let result = stage(&newer, None, Parts::ALL, &data);
    // A development build (0.0.0) reads anything; a release refuses.
    assert_eq!(result.is_err(), crate::build_info::VERSION != "0.0.0");
    assert!(!newer_than_us("garbage"));
    Ok(())
}

#[test]
fn retention_keeps_the_newest_by_count_and_by_age_but_never_the_last_one() {
    let now: jiff::Timestamp = "2026-03-10T00:00:00Z".parse().expect("time");
    let day = |d: i64| (now.as_second() - d * 86_400).to_string();
    let _ = day;
    let at = |d: i64| jiff::Timestamp::from_second(now.as_second() - d * 86_400).expect("time");
    let entries: Vec<(String, jiff::Timestamp)> = vec![
        ("irori-backup-5.tar.gz".into(), at(1)),
        ("irori-backup-4.tar.gz".into(), at(2)),
        ("pre-restore-1.tar.gz".into(), at(3)),
        ("irori-backup-3.tar.gz".into(), at(40)),
        ("irori-backup-2.tar.gz".into(), at(50)),
    ];
    assert_eq!(
        expired(&entries, PREFIX, 3, None, now),
        ["irori-backup-2.tar.gz"].map(String::from)
    );
    assert_eq!(
        expired(&entries, PREFIX, 10, Some(30), now),
        ["irori-backup-3.tar.gz", "irori-backup-2.tar.gz"].map(String::from)
    );
    // Everything is old, and one is still kept.
    let old = vec![("irori-backup-1.tar.gz".to_owned(), at(400))];
    assert!(expired(&old, PREFIX, 1, Some(30), now).is_empty());
}

#[test]
fn only_our_own_names_may_be_asked_for() {
    assert!(is_backup_name("irori-backup-20260101-000000.tar.gz"));
    assert!(is_backup_name("irori-backup-20260101-000000.tar.gz.enc"));
    assert!(is_backup_name("pre-restore-20260101-000000.tar.gz"));
    for bad in [
        "../irori-backup-x.tar.gz",
        "irori-backup-../../x.tar.gz",
        "secrets.toml",
        "irori-backup-a/b.tar.gz",
        "irori-backup-x.zip",
    ] {
        assert!(!is_backup_name(bad), "{bad}");
    }
}

#[test]
fn a_restore_that_fails_half_way_puts_the_home_back_as_it_was() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    // The home moves on after the backup was taken.
    fs::write(
        config.join("areas.toml"),
        "[areas.hall]\nname = \"Changed\"\n",
    )?;
    let parts = Parts {
        settings: true,
        extensions: false,
        state: true,
        history: false,
        activity: false,
    };
    stage(&backup, None, parts, &data)?;
    // Damaged after it was checked: the settings go in, then the database can't be read.
    fs::write(
        data.join(PENDING).join("tree/data/irori.db"),
        b"not a database",
    )?;

    let done = apply_pending(&config, &data).context("nothing was staged")?;
    assert!(!done.ok);
    let error = done.error.expect("says why");
    assert!(error.contains("put back as it was"), "{error}");
    assert!(
        fs::read_to_string(config.join("areas.toml"))?.contains("Changed"),
        "the settings are the ones the home had, not the backup's"
    );
    assert_eq!(sql(&data, "SELECT n FROM t")?, "42");
    assert!(!data.join("restore-rollback").exists());
    Ok(())
}

#[test]
fn restoring_settings_that_open_the_network_says_so_first() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    fs::write(
        config.join("irori.toml"),
        "[server]\nallow_unauthenticated_lan = true\n",
    )?;
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    let verified = stage(&backup, None, Parts::ALL, &data)?;
    assert!(
        verified
            .warnings
            .iter()
            .any(|w| w.contains("allow_unauthenticated_lan")),
        "{:?}",
        verified.warnings
    );
    Ok(())
}

#[test]
fn the_last_three_safety_copies_are_kept() -> anyhow::Result<()> {
    let tmp = home()?;
    let (config, data) = paths(&tmp);
    let backup = create(&config, &data, &dir(&data), Plan::PLAIN)?;
    for _ in 0..5 {
        restore_now(&backup, None, Parts::ALL, &config, &data)?;
    }
    let kept = fs::read_dir(dir(&data))?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(SAFETY_PREFIX))
        .count();
    assert_eq!(kept, 3);
    Ok(())
}
