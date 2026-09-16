//! The config directory, wired to the running core.
//!
//! `irori-config` reads and writes the files; `irori-core` holds what they mean. This joins the
//! two, and is the only place that knows both. Every change goes the same way round — write the
//! file, then tell the core — so a save that fails leaves the core agreeing with the disk rather
//! than showing a change that was never kept.

use std::sync::Arc;
use std::time::Duration;

use irori_config::{Problem, ServerSettings, Store};
use irori_core::Core;
use irori_types::{ExtensionSettings, Settings};
use tokio::sync::Mutex;

/// How often the files are checked for outside edits. Two seconds is fast enough that editing a
/// file feels live, and slow enough that the cost is three `stat` calls.
const POLL: Duration = Duration::from_secs(2);

/// The config directory. Cheap to clone; all clones share one lock, so edits happen one at a
/// time and never interleave with a reload.
#[derive(Debug, Clone)]
pub struct Config(Arc<Mutex<Store>>);

/// What `[server]` said when Irori started, so an edit can be told apart from what's in force.
#[derive(Debug)]
struct Started(ServerSettings);

/// Why an edit couldn't be made. Not an I/O failure — that's an `anyhow::Error`.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An edit either couldn't be made, or couldn't be written down.
#[derive(Debug)]
pub enum EditError {
    Refused(Refused),
    Io(std::io::Error),
}

impl Config {
    /// Reads the directory and tells the core what it says, before any integration starts — so a
    /// device that arrives in the first second already has the name its owner gave it.
    ///
    /// A directory that isn't there, or files that don't parse, are logged and survived: Irori
    /// starting is not conditional on its config being perfect.
    pub fn open(store: Store, problems: &[Problem], core: &Core) -> Self {
        report(problems);
        tracing::info!(
            path = %store.dir().display(),
            areas = store.settings().areas.len(),
            devices = store.settings().devices.len(),
            "config directory read"
        );
        core.apply_settings(store.settings());
        core.apply_extension_settings(store.extension_settings());
        let disabled = store.irori().extensions.disabled;
        if !disabled.is_empty() {
            tracing::info!(extensions = ?disabled, "turned off in irori.toml");
        }
        core.apply_disabled_extensions(disabled);
        Self(Arc::new(Mutex::new(store)))
    }

    /// For tests: a directory read from scratch.
    #[cfg(test)]
    pub fn open_dir(dir: impl AsRef<std::path::Path>, core: &Core) -> Self {
        let mut store = Store::new(dir.as_ref());
        let problems = store.reload();
        Self::open(store, &problems, core)
    }

    /// Changes what the config says, writes it, and tells the core — in that order.
    ///
    /// The files are re-read first, so an edit always builds on what's on disk rather than on a
    /// copy that someone's text editor has since overtaken.
    pub async fn edit<T>(
        &self,
        core: &Core,
        change: impl FnOnce(&mut Settings) -> Result<T, Refused>,
    ) -> Result<T, EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut settings = store.settings();
        let made = change(&mut settings).map_err(EditError::Refused)?;
        let written = store.save(&settings).map_err(EditError::Io)?;
        if !written.is_empty() {
            let files: Vec<&str> = written.iter().map(|file| file.name()).collect();
            tracing::info!(files = ?files, "config written");
        }
        core.apply_settings(settings);
        Ok(made)
    }

    /// Changes `secrets.toml`, writes it, and tells the core — which restarts whichever extension
    /// the change was for. Same order and same re-read as [`Config::edit`].
    pub async fn edit_secrets<T>(
        &self,
        core: &Core,
        change: impl FnOnce(&mut ExtensionSettings) -> Result<T, Refused>,
    ) -> Result<T, EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut secrets = store.extension_settings();
        let made = change(&mut secrets).map_err(EditError::Refused)?;
        if store.save_secrets(&secrets).map_err(EditError::Io)? {
            // Which file, never what's in it.
            tracing::info!(file = "secrets.toml", "config written");
        }
        core.apply_extension_settings(secrets);
        Ok(made)
    }

    /// Picks up edits made outside Irori. Runs until the process ends.
    pub async fn watch(self, core: Core) {
        let started = Started(self.0.lock().await.irori().server);
        let mut warned: Option<ServerSettings> = None;
        loop {
            tokio::time::sleep(POLL).await;
            let mut store = self.0.lock().await;
            let problems = store.reload();
            report(&problems);
            // All of these publish nothing when nothing changed, so this is free on the
            // overwhelming majority of ticks.
            core.apply_settings(store.settings());
            core.apply_extension_settings(store.extension_settings());
            let irori = store.irori();
            core.apply_disabled_extensions(irori.extensions.disabled);
            // Where Irori listens and logs can't change under a running server. Say so, once per
            // edit, rather than leaving someone wondering why their change did nothing.
            if irori.server != started.0 && warned.as_ref() != Some(&irori.server) {
                tracing::warn!(
                    "irori.toml's [server] settings changed; they take effect when Irori restarts"
                );
                warned = Some(irori.server);
            }
        }
    }
}

/// A file Irori couldn't read is worth saying loudly and repeatedly: it means someone's edit
/// isn't taking effect, and the only way they find out is the log.
fn report(problems: &[Problem]) {
    for problem in problems {
        tracing::error!(
            file = %problem.file,
            reason = %problem.reason,
            "config file ignored; the last good version is still in force"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use irori_core::SystemClock;
    use irori_types::{Area, DeviceSettings, Name};

    use super::*;

    fn core() -> Core {
        Core::new(Arc::new(SystemClock))
    }

    fn area(id: &str, called: &str) -> Area {
        Area {
            id: id.parse().expect("a valid area id"),
            name: called.parse().expect("a valid name"),
            floor_id: None,
        }
    }

    #[tokio::test]
    async fn an_edit_is_written_down_and_reaches_the_core() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);

        config
            .edit(&core, |settings| {
                settings.areas.push(area("hall", "Hall"));
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        assert_eq!(core.areas(), vec![area("hall", "Hall")]);
        let written = std::fs::read_to_string(dir.path().join("areas.toml"))?;
        assert!(written.contains("[areas.hall]"), "{written}");
        Ok(())
    }

    /// A refused edit changes neither the files nor the core.
    #[tokio::test]
    async fn a_refused_edit_leaves_everything_alone() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);

        let result: Result<(), _> = config
            .edit(&core, |settings| {
                settings.areas.push(area("hall", "Hall"));
                Err(Refused("no".to_owned()))
            })
            .await;

        assert!(matches!(result, Err(EditError::Refused(_))));
        assert!(core.areas().is_empty());
        assert!(!dir.path().join("areas.toml").exists());
        Ok(())
    }

    /// The reason the files are a supported way in: an edit made in a text editor reaches the
    /// running core, and an edit made through Irori builds on it rather than overwriting it.
    #[tokio::test]
    async fn an_edit_made_in_an_editor_is_picked_up_and_built_on() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);

        std::fs::write(
            dir.path().join("areas.toml"),
            "[areas.kitchen]\nname = \"Kitchen\"\n",
        )?;
        config
            .edit(&core, |settings| {
                settings.devices.insert(
                    "demo_lamp".parse().expect("a valid device id"),
                    DeviceSettings {
                        name: Some("Reading lamp".parse::<Name>().expect("a valid name")),
                        description: None,
                        area: irori_types::Placement::Unsaid,
                    },
                );
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        assert_eq!(
            core.areas(),
            vec![area("kitchen", "Kitchen")],
            "the hand-made room survived an edit that knew nothing about it"
        );
        Ok(())
    }
}
