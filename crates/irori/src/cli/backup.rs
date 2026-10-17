//! `irori backup` (`docs/specs/backup.md`). Works on the files of a home, not over the API, so
//! it still works on a machine where Irori is not running, which is when a restore is wanted.

use std::path::PathBuf;

use super::args::{BackupArgs, BackupCmd};
use super::{output, prompt};
use crate::backup;

pub fn run(args: BackupArgs) -> Result<(), String> {
    let json = args.json;
    let mut store = irori_config::Store::new(&args.config);
    // Problems with the files are the server's to report; a backup of a home with a typo in it
    // is exactly the backup somebody wants.
    let _ = store.reload();
    let settings = store.irori();
    let data = args
        .data
        .or_else(|| settings.server.data.as_ref().map(|d| args.config.join(d)))
        .unwrap_or_else(|| PathBuf::from("./data"));
    let default_dir = backup::dir(&data, settings.backup.dir.as_deref());

    match args.cmd {
        BackupCmd::Create { to, prune } => {
            let into = to.unwrap_or_else(|| default_dir.clone());
            let file = backup::create(&args.config, &data, &into).map_err(|e| format!("{e:#}"))?;
            let pruned = if prune {
                backup::prune(&into, settings.backup.keep)
            } else {
                0
            };
            output::show(
                json,
                &serde_json::json!({ "file": file, "pruned": pruned }),
                || {
                    println!("Wrote {}", file.display());
                    if pruned > 0 {
                        println!("Deleted {pruned} older backup(s).");
                    }
                },
            )
        }
        BackupCmd::List { dir } => {
            let dir = dir.unwrap_or(default_dir);
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
                        "{:<42} {:>9}  Irori {version}  {}",
                        e.name,
                        human_size(e.bytes),
                        e.modified
                    );
                }
            })
        }
        BackupCmd::Restore { archive, yes } => {
            // A bare name is one in the backup directory.
            let file = if archive.components().count() == 1
                && backup::is_backup_name(&archive.to_string_lossy())
            {
                default_dir.join(&archive)
            } else {
                archive
            };
            if backup::server_running(&data) {
                return Err(format!(
                    "Irori is running (see {}). Stop it first: a restore replaces files it has open.",
                    data.join("cli.url").display()
                ));
            }
            let question = format!(
                "Replace the home in {} and {} with {}? The current one is kept as a pre-restore backup.",
                args.config.display(),
                data.display(),
                file.display()
            );
            if !prompt::confirm(&question, yes, json)? {
                return output::done(json, "Nothing changed.");
            }
            let done = backup::restore(&file, &args.config, &data).map_err(|e| format!("{e:#}"))?;
            output::show(json, &done, || {
                println!("Restored from {}.", file.display());
                if let Some(copy) = &done.safety_copy {
                    println!("The home as it was is kept in {}.", copy.display());
                }
                if !done.manifest.extensions.is_empty() {
                    println!(
                        "Extensions were installed when this was made and are not in the backup: {}. Install them from the Extensions page or `irori extensions install`.",
                        done.manifest.extensions.join(", ")
                    );
                }
            })
        }
    }
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
