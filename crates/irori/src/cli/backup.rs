//! `irori backup` (`docs/specs/backup.md`). Works on the files of a home, not over the API, so
//! it still works on a machine where Irori is not running, which is when a restore is wanted.

use std::path::{Path, PathBuf};

use super::args::{BackupArgs, BackupCmd};
use super::{output, prompt};
use crate::backup::{self, NeedsKey, Parts, crypt, schedule, settings};

pub fn run(args: BackupArgs) -> Result<(), String> {
    let json = args.json;
    let config = args.config;
    let mut store = irori_config::Store::new(&config);
    let _ = store.reload();
    let data = args
        .data
        .or_else(|| store.irori().server.data.map(|d| config.join(d)))
        .unwrap_or_else(|| PathBuf::from("./data"));
    let kept = backup::dir(&data);

    match args.cmd {
        BackupCmd::Create { to } => {
            let settings = settings::load(&config);
            let mut key = None;
            let (key_made, plan) = {
                let existed = settings::secret(&config, settings::KEY).is_some();
                let plan = schedule::plan_for(&config, &settings, &mut key)?;
                (settings.encrypt && !existed, plan)
            };
            let into = to.unwrap_or_else(|| kept.clone());
            let file = backup::create(&config, &data, &into, plan).map_err(|e| format!("{e:#}"))?;
            let pruned = if into == kept {
                backup::prune(&into, settings.keep, settings.keep_days)
            } else {
                0
            };
            output::show(
                json,
                &serde_json::json!({ "file": file, "pruned": pruned, "key": key_made.then_some(&key) }),
                || {
                    println!("Wrote {}", file.display());
                    if pruned > 0 {
                        println!("Deleted {pruned} older backup(s).");
                    }
                    if let (true, Some(key)) = (key_made, &key) {
                        println!(
                            "\nThese backups are encrypted with a new recovery key:\n\n    {key}\n\nWrite it down somewhere that isn't this machine. Without it a backup can't be opened."
                        );
                    }
                },
            )
        }
        BackupCmd::List { dir } => {
            let dir = dir.unwrap_or(kept);
            let entries = backup::list(&dir);
            output::show(json, &entries, || {
                if entries.is_empty() {
                    println!("No backups in {}.", dir.display());
                }
                for e in &entries {
                    let version = e
                        .manifest
                        .as_ref()
                        .map_or("?", |m| m.irori_version.as_str());
                    println!(
                        "{:<46} {:>9}  {}  Irori {version}  {}",
                        e.name,
                        human_size(e.bytes),
                        if e.encrypted { "encrypted" } else { "plain    " },
                        e.modified
                    );
                }
            })
        }
        BackupCmd::Verify { archive, key } => {
            let file = resolve(&archive, &kept);
            let key = key_for(&file, key, &config, json)?;
            let verified =
                backup::verify(&file, key.as_deref(), &data.join("backups")).map_err(describe)?;
            output::show(json, &verified, || {
                println!(
                    "{} opens: made by Irori {} on {}, {} files{}.",
                    file.display(),
                    verified.manifest.irori_version,
                    verified.manifest.created_at,
                    verified.files,
                    if verified.manifest.history {
                        ""
                    } else {
                        ", without history"
                    }
                );
                for warning in &verified.warnings {
                    println!("  note: {warning}");
                }
            })
        }
        BackupCmd::Restore {
            archive,
            key,
            keep_history,
            only,
            yes,
        } => {
            let file = resolve(&archive, &kept);
            let parts = parts_from(keep_history, &only)?;
            if backup::server_running(&data) {
                return Err(format!(
                    "Irori is running (see {}). Stop it first: a restore replaces files it has open. \
                     Or restore from the page, which does it for you.",
                    data.join("cli.url").display()
                ));
            }
            let key = key_for(&file, key, &config, json)?;
            // Opened and checked before anyone is asked: a backup that can't be restored
            // shouldn't need a yes first.
            let verified = backup::stage(&file, key.as_deref(), parts, &data).map_err(describe)?;
            let question = format!(
                "Replace this home ({} and {}) with {}? The current one is kept as a pre-restore backup.",
                config.display(),
                data.display(),
                file.display()
            );
            if !prompt::confirm(&question, yes, json)
                .inspect_err(|_| backup::cancel_pending(&data))?
            {
                backup::cancel_pending(&data);
                return output::done(json, "Nothing changed.");
            }
            let done = backup::apply_pending(&config, &data).ok_or("nothing was staged")?;
            if !done.ok {
                return Err(done
                    .error
                    .unwrap_or_else(|| "the restore failed".to_owned()));
            }
            output::show(json, &done, || {
                println!("Restored from {}.", file.display());
                if let Some(copy) = &done.safety_copy {
                    println!(
                        "The home as it was is kept in {}.",
                        kept.join(copy).display()
                    );
                }
                for warning in &verified.warnings {
                    println!("  note: {warning}");
                }
                let missing = backup::missing_extensions(&data, &verified.manifest);
                if !missing.is_empty() {
                    println!(
                        "Extensions that were installed are fetched again the next time Irori starts: {}.",
                        missing.join(", ")
                    );
                }
            })
        }
        BackupCmd::Key { new } => {
            let key = if new {
                let key = crypt::generate_key();
                settings::set_secret(&config, settings::KEY, &key)?;
                key
            } else {
                settings::key_or_make(&config)?.0
            };
            output::show(json, &serde_json::json!({ "key": key }), || {
                println!("{key}")
            })
        }
    }
}

/// A bare backup name is one in the backup directory.
fn resolve(archive: &Path, kept: &Path) -> PathBuf {
    if archive.components().count() == 1 && backup::is_backup_name(&archive.to_string_lossy()) {
        kept.join(archive)
    } else {
        archive.to_path_buf()
    }
}

/// The key to open `file` with: the one given, else (for an encrypted file) the one in this
/// machine's `secrets.toml`, else one typed.
fn key_for(
    file: &Path,
    given: Option<String>,
    config: &Path,
    json: bool,
) -> Result<Option<String>, String> {
    let sealed = backup::inspect(file).map_err(describe)?.0;
    if !sealed || given.is_some() {
        return Ok(given);
    }
    if let Some(local) = settings::secret(config, settings::KEY) {
        return Ok(Some(local));
    }
    if json {
        return Err(NeedsKey.to_string());
    }
    prompt::password("Recovery key: ", false).map(Some)
}

fn parts_from(keep_history: bool, only: &[String]) -> Result<Parts, String> {
    if keep_history {
        return Ok(Parts {
            history: false,
            ..Parts::ALL
        });
    }
    if only.is_empty() {
        return Ok(Parts::ALL);
    }
    let mut parts = Parts {
        settings: false,
        extensions: false,
        state: false,
        history: false,
    };
    for name in only {
        match name.trim() {
            "settings" => parts.settings = true,
            "extensions" => parts.extensions = true,
            "state" => parts.state = true,
            "history" => parts.history = true,
            other => {
                return Err(format!(
                    "`{other}` isn't something to restore: choose from settings, extensions, state, history"
                ));
            }
        }
    }
    Ok(parts)
}

fn describe(error: anyhow::Error) -> String {
    format!("{error:#}")
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
