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

    /// Changes one extension's `extensions/<id>.toml`, writes it, and tells the core — which
    /// restarts that extension with it.
    pub async fn edit_extension<T>(
        &self,
        core: &Core,
        extension: &irori_types::ExtensionId,
        change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> Result<T, Refused>,
    ) -> Result<T, EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut file = store.extension_file(extension);
        let made = change(&mut file).map_err(EditError::Refused)?;
        if store
            .save_extension(extension, &file)
            .map_err(EditError::Io)?
        {
            tracing::info!(file = %format!("extensions/{extension}.toml"), "config written");
        }
        core.apply_extension_settings(store.extension_settings());
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

        let mut secrets = store.secrets();
        let made = change(&mut secrets).map_err(EditError::Refused)?;
        if store.save_secrets(&secrets).map_err(EditError::Io)? {
            // Which file, never what's in it.
            tracing::info!(file = "secrets.toml", "config written");
        }
        core.apply_extension_settings(store.extension_settings());
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
            let mut settings = store.settings();
            if settings.ask_before_adding && !core.settings().ask_before_adding {
                keep_what_is_here(&mut store, &mut settings, &core);
            }
            core.apply_settings(settings);
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

/// Asking before adding is about what Irori finds from now on. Turning it on mustn't empty the
/// home of everything already in it, so those devices are written down as added — the one time
/// Irori writes `devices.toml` without being asked for that exact change, because the change a
/// person did ask for would otherwise undo their whole home.
fn keep_what_is_here(store: &mut Store, settings: &mut Settings, core: &Core) {
    let mut kept = 0;
    for device in core.devices() {
        let entry = settings.devices.entry(device.id).or_default();
        if !entry.added && !entry.ignored {
            entry.added = true;
            kept += 1;
        }
    }
    if kept == 0 {
        return;
    }
    match store.save(settings) {
        Ok(_) => tracing::info!(
            devices = kept,
            "asking before adding new devices; the ones already in the home stay"
        ),
        // The devices file didn't change. Asking still comes from irori.toml, so restoring
        // `store.settings()` would enable it without `added` and hold the whole home. Leave
        // asking off in what we apply; the next poll retries the write.
        Err(e) => {
            tracing::error!(%e, "couldn't record the devices already in the home as added");
            *settings = store.settings();
            settings.ask_before_adding = false;
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
                        added: false,
                        name: Some("Reading lamp".parse::<Name>().expect("a valid name")),
                        description: None,
                        area: irori_types::Placement::Unsaid,
                        ignored: false,
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

    /// Turning on "ask before adding" keeps the home as it is: what's already in it is written
    /// down as added, so neither this moment nor the next restart empties it.
    #[cfg(feature = "int-demo")]
    #[tokio::test]
    async fn asking_before_adding_keeps_the_devices_already_here() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);
        let host = irori_core::ExtensionHost::start(
            &core,
            // The demo alone: ESPHome would find whatever is on this network partway through.
            crate::extensions::builtins()?
                .into_iter()
                .filter(|builtin| builtin.manifest.extension.id.as_str() == "demo")
                .collect(),
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if core.devices().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let before = core.devices().len();
        assert!(before >= 3, "the demo devices arrived");
        tokio::spawn(config.clone().watch(core.clone()));

        std::fs::write(dir.path().join("irori.toml"), "[devices]\nnew = \"ask\"\n")?;
        for _ in 0..500 {
            if core.settings().ask_before_adding {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            core.settings().ask_before_adding,
            "the change was picked up"
        );
        assert_eq!(core.devices().len(), before, "nothing left the home");
        assert!(core.held_devices().is_empty());
        let devices = std::fs::read_to_string(dir.path().join("devices.toml"))?;
        assert!(devices.contains("added = true"), "{devices}");

        host.shutdown().await;
        Ok(())
    }

    /// Answering a secret must write only `secrets.toml`. Starting from the joined view would
    /// copy `extensions/<id>.toml` into it, and the secret would then win on every reload.
    #[tokio::test]
    async fn answering_a_secret_does_not_copy_the_extension_file_into_secrets() -> anyhow::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);
        let helpers: irori_types::ExtensionId = "helpers".parse().expect("valid");

        config
            .edit_extension(&core, &helpers, |file| {
                file.insert(
                    "toggles".into(),
                    serde_json::json!({"guests": {"name": "Guests"}}),
                );
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        config
            .edit_secrets(&core, |secrets| {
                secrets
                    .set(&helpers, &["token".into()], "s3cret".into())
                    .map_err(|e| Refused(e.to_string()))
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let secrets = std::fs::read_to_string(dir.path().join("secrets.toml"))?;
        assert!(secrets.contains("token"), "{secrets}");
        assert!(
            !secrets.contains("toggles"),
            "non-secret settings landed in secrets.toml:\n{secrets}"
        );
        let extension = std::fs::read_to_string(dir.path().join("extensions/helpers.toml"))?;
        assert!(extension.contains("toggles"), "{extension}");
        Ok(())
    }

    /// If recording `added` fails when asking is turned on, asking must not take effect: the
    /// devices already in the home would otherwise move to the held list.
    #[cfg(feature = "int-demo")]
    #[tokio::test]
    async fn a_failed_write_when_asking_starts_does_not_hold_the_home() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let _config = Config::open_dir(dir.path(), &core);
        let host = irori_core::ExtensionHost::start(
            &core,
            crate::extensions::builtins()?
                .into_iter()
                .filter(|builtin| builtin.manifest.extension.id.as_str() == "demo")
                .collect(),
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if core.devices().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let before = core.devices().len();
        assert!(before >= 3, "the demo devices arrived");

        std::fs::create_dir(dir.path().join("devices.toml.writing"))?;
        let mut store = Store::new(dir.path());
        store.reload();
        let mut settings = store.settings();
        settings.ask_before_adding = true;
        keep_what_is_here(&mut store, &mut settings, &core);
        assert!(
            !settings.ask_before_adding,
            "asking must wait until the home is recorded as added"
        );
        core.apply_settings(settings);
        assert_eq!(core.devices().len(), before);
        assert!(core.held_devices().is_empty());

        host.shutdown().await;
        Ok(())
    }
}
