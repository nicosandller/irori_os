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
use std::collections::BTreeSet;

use irori_types::{
    DeviceId, ExtensionId, ExtensionSettings, HomeSettings, ProtocolId, Settings, SettingsKey,
    UniqueId, User, UserId,
};
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

/// Whether the core may already have lost a device being forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gone {
    Yes,
    No,
}

impl Config {
    /// Reads the directory and tells the core what it says, before any protocol starts — so a
    /// device that arrives in the first second already has the name its owner gave it.
    ///
    /// A directory that isn't there, or files that don't parse, are logged and survived: Irori
    /// starting is not conditional on its config being perfect.
    pub fn open(store: Store, problems: &[Problem], core: &Core) -> Self {
        report(problems);
        // Absolute even when the directory doesn't exist yet: canonicalize needs it on disk,
        // and the log is how someone tells which relative `--config` they actually started.
        let path = std::path::absolute(store.dir()).unwrap_or_else(|_| store.dir().to_path_buf());
        tracing::info!(
            path = %path.display(),
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
        core.apply_home(store.home());
        if let Some(new) = store.irori().devices.new {
            tracing::warn!(
                new = %new,
                "irori.toml's [devices] new is no longer used: a device joins the home when \
                 you add it from + Add device"
            );
        }
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

    /// Removes a device from the home (`docs/specs/config.md` §3.2): everything Irori keeps of it
    /// leaves the config files — its row in `devices.toml`, its entities' rows in `entities.toml`,
    /// and its spot on the floorplan — and the core takes it out of the registry. The device
    /// itself isn't told anything: it goes back among what its protocol has found, so
    /// "+ Add device" lists it straight away and it can be added again like any new device.
    ///
    /// `secrets.toml` is left alone: which of an extension's secrets belongs to which device is
    /// the extension's business, and a key is what an encrypted device needs to be added back.
    ///
    /// Written down first, then forgotten, like every change here: the files and the core never
    /// disagree, so a save that fails leaves the core agreeing with the disk.
    pub async fn forget_device(&self, core: &Core, id: &DeviceId) -> Result<(), EditError> {
        // The rows its entities keep in `entities.toml`, keyed by protocol and unique id. Read
        // from the core, because the files say nothing about which entity belongs to which
        // device.
        let owned = core.device_entity_keys(id).unwrap_or_default();
        self.forget_rows(core, id, owned, Gone::No).await
    }

    /// The same for a device its protocol has just unpaired (`docs/specs/config.md` §3.2). By
    /// now the protocol has taken it out of the core itself, so `owned` — its entities, as
    /// they were — has to have been read before the unpair, and the core having no such device
    /// is how it should be rather than a refusal.
    pub async fn forget_unpaired(
        &self,
        core: &Core,
        id: &DeviceId,
        owned: Vec<(ProtocolId, UniqueId)>,
    ) -> Result<(), EditError> {
        self.forget_rows(core, id, owned, Gone::Yes).await
    }

    async fn forget_rows(
        &self,
        core: &Core,
        id: &DeviceId,
        owned: Vec<(ProtocolId, UniqueId)>,
        gone: Gone,
    ) -> Result<(), EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut settings = store.settings();
        let owned: BTreeSet<SettingsKey> = owned
            .into_iter()
            .map(|(protocol, unique_id)| SettingsKey::new(protocol, unique_id))
            .collect();
        forget(
            &mut settings,
            |device| device == id,
            |key| owned.contains(key),
        );
        let written = store.save(&settings).map_err(EditError::Io)?;
        if !written.is_empty() {
            let files: Vec<&str> = written.iter().map(|file| file.name()).collect();
            tracing::info!(files = ?files, "config written");
        }
        match (core.forget_device(id), gone) {
            (Ok(()), _) | (Err(_), Gone::Yes) => {}
            (Err(why), Gone::No) => return Err(EditError::Refused(Refused(why.to_string()))),
        }
        core.apply_settings(settings);
        Ok(())
    }

    /// Removes every device an extension brought in, for when the extension itself goes: the
    /// same as removing each of them, including the ones that aren't around right now. Without
    /// this, installing the extension again would put every device it ever had straight back in
    /// the home, because their `devices.toml` rows would still say they'd been added.
    pub async fn forget_protocol(
        &self,
        core: &Core,
        protocol: &ProtocolId,
    ) -> Result<(), EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut settings = store.settings();
        let before = settings.clone();
        // The other extensions still installed: a device of theirs can have an id that starts
        // the same way (`zigbee_mqtt_…` against `zigbee`), and it's theirs, not this one's.
        let others: Vec<ProtocolId> = core
            .extensions()
            .keys()
            .filter_map(|id| ProtocolId::try_from(id.as_str()).ok())
            .filter(|other| other != protocol)
            .collect();
        forget(
            &mut settings,
            |device| brought_in_by(device, protocol, &others),
            |key| &key.protocol == protocol,
        );
        if settings == before {
            return Ok(());
        }
        let written = store.save(&settings).map_err(EditError::Io)?;
        if !written.is_empty() {
            let files: Vec<&str> = written.iter().map(|file| file.name()).collect();
            tracing::info!(files = ?files, extension = %protocol, "config written");
        }
        core.apply_settings(settings);
        Ok(())
    }

    /// One extension's `extensions/<id>.toml` as it currently stands.
    ///
    /// Only that file: secrets live in `secrets.toml` and are never read back out (§3.4). This is
    /// what lets the settings form open showing what is already configured, so changing one field
    /// doesn't mean retyping the rest — and so a required field it can't show isn't mistaken for
    /// one nobody has filled in.
    pub async fn extension_settings(
        &self,
        extension: &irori_types::ExtensionId,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut store = self.0.lock().await;
        report(&store.reload());
        store.extension_file(extension)
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

    /// Where the home is and its time zone, as `home.toml` stands.
    pub async fn home(&self) -> HomeSettings {
        let mut store = self.0.lock().await;
        report(&store.reload());
        store.home()
    }

    /// Changes `home.toml`, writes it, and tells the core, which tells the engines that fire
    /// by the clock. Same order and same re-read as [`Config::edit`].
    pub async fn edit_home<T>(
        &self,
        core: &Core,
        change: impl FnOnce(&mut HomeSettings) -> Result<T, Refused>,
    ) -> Result<T, EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut home = store.home();
        let made = change(&mut home).map_err(EditError::Refused)?;
        if store.save_home(&home).map_err(EditError::Io)? {
            tracing::info!(file = "home.toml", "config written");
        }
        core.apply_home(home);
        Ok(made)
    }

    /// The people allowed in, each with the hash of their password if they have one. Read
    /// without touching the disk: this is asked on every request, and the watch loop and every
    /// edit keep it current.
    pub async fn people(&self) -> People {
        let store = self.0.lock().await;
        People::of(store.users(), &store.secrets())
    }

    /// Changes who is allowed in and what their passwords are, as one write: `users.toml` and
    /// the `[users]` table of `secrets.toml` together, so nobody is left in one without the
    /// other. What can't stand (nobody left to run the home) is refused with nothing written.
    pub async fn edit_people<T>(
        &self,
        core: &Core,
        change: impl FnOnce(&mut People) -> Result<T, Refused>,
    ) -> Result<T, EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut people = People::of(store.users(), &store.secrets());
        let made = change(&mut people).map_err(EditError::Refused)?;
        irori_types::check_users(&people.users)
            .map_err(|e| EditError::Refused(Refused(e.to_string())))?;
        // A home that asks who is there has to have an owner who can answer. Otherwise the
        // first password, given to somebody who isn't one, would shut every owner out.
        let has = |user: &User| people.hashes.contains_key(&user.id);
        if people.users.iter().any(has)
            && !people
                .users
                .iter()
                .any(|user| user.role.runs_the_home() && has(user))
        {
            return Err(EditError::Refused(Refused(
                "an owner needs a password before anybody else has one: otherwise nobody \
                 could sign in to run the home"
                    .to_owned(),
            )));
        }
        let mut secrets = store.secrets();
        let table = users_table();
        // Rewritten whole, so a person who is gone takes their hash with them.
        secrets.remove(&table, &[PASSWORDS.to_owned()]);
        for user in &people.users {
            if let Some(hash) = people.hashes.get(&user.id) {
                secrets
                    .set(
                        &table,
                        &[PASSWORDS.to_owned(), user.id.to_string()],
                        hash.clone(),
                    )
                    .map_err(|e| EditError::Refused(Refused(e.to_string())))?;
            }
        }
        // A home with no passwords writes nothing secret: `secrets.toml` isn't made for it.
        if secrets == store.secrets() {
            if store.save_users(&people.users).map_err(EditError::Io)? {
                tracing::info!(file = "users.toml", "config written");
            }
        } else {
            store
                .save_users_and_secrets(&people.users, &secrets)
                .map_err(EditError::Io)?;
            // Which files, never what's in them.
            tracing::info!(files = "users.toml, secrets.toml", "config written");
        }
        core.apply_extension_settings(store.extension_settings());
        Ok(made)
    }

    /// The config directory, for files this type doesn't own (`assistant.toml`).
    #[cfg(feature = "assist")]
    pub async fn dir(&self) -> std::path::PathBuf {
        self.0.lock().await.dir().to_path_buf()
    }

    /// The assistant's API key, if one has been saved. Never for a response body.
    #[cfg(feature = "assist")]
    pub async fn assistant_key(&self) -> Option<String> {
        let mut store = self.0.lock().await;
        report(&store.reload());
        let Ok(id) = irori_types::ExtensionId::try_from("assistant") else {
            return None;
        };
        match store.secrets().of(&id).get("api_key") {
            Some(serde_json::Value::String(key)) if !key.is_empty() => Some(key.clone()),
            _ => None,
        }
    }

    /// Writes one extension's settings file and its secrets, then tells the core once.
    ///
    /// [`Self::edit_extension`] and [`Self::edit_secrets`] each tell the core, and the core
    /// restarts an extension whose settings changed. A form that sets both — Zigbee's serial
    /// port and its network key — must not restart between the two writes. The first start
    /// would see the port and no key, and Zigbee2MQTT would generate a network identity of its
    /// own before the key arrived.
    pub async fn edit_extension_with_secrets<T>(
        &self,
        core: &Core,
        extension: &irori_types::ExtensionId,
        change_file: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> Result<T, Refused>,
        secrets_to_set: &[(String, String)],
    ) -> Result<T, EditError> {
        let mut store = self.0.lock().await;
        report(&store.reload());

        let mut file = store.extension_file(extension);
        let made = change_file(&mut file).map_err(EditError::Refused)?;
        let mut secrets = store.secrets();
        for (key, text) in secrets_to_set {
            secrets
                .set(extension, std::slice::from_ref(key), text.clone())
                .map_err(|e| EditError::Refused(Refused(e.to_string())))?;
        }
        store
            .save_extension_and_secrets(extension, &file, &secrets)
            .map_err(EditError::Io)?;
        tracing::info!(
            files = %format!("extensions/{extension}.toml, secrets.toml"),
            "config written"
        );
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
            core.apply_settings(store.settings());
            core.apply_extension_settings(store.extension_settings());
            let irori = store.irori();
            core.apply_disabled_extensions(irori.extensions.disabled);
            core.apply_home(store.home());
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

/// The table of `secrets.toml` that people's password hashes are kept in.
const PASSWORDS: &str = "passwords";

fn users_table() -> ExtensionId {
    ExtensionId::try_from("users").expect("a valid id")
}

/// Who is allowed in, and the hash of each one's password.
#[derive(Clone, Default)]
pub struct People {
    /// Ordered by id.
    pub users: Vec<User>,
    /// By user. A person with no entry has no password.
    pub hashes: std::collections::BTreeMap<UserId, String>,
}

/// Never the hashes: they are secrets, and this type ends up in logs through `AppState`.
impl std::fmt::Debug for People {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("People")
            .field("users", &self.users)
            .field("with_passwords", &self.hashes.len())
            .finish()
    }
}

impl People {
    fn of(users: Vec<User>, secrets: &ExtensionSettings) -> Self {
        let table = secrets.of(&users_table());
        let hashes = users
            .iter()
            .filter_map(|user| match &table[PASSWORDS][user.id.as_str()] {
                serde_json::Value::String(hash) if !hash.is_empty() => {
                    Some((user.id.clone(), hash.clone()))
                }
                _ => None,
            })
            .collect();
        Self { users, hashes }
    }

    pub fn user(&self, id: &UserId) -> Option<&User> {
        self.users.iter().find(|user| &user.id == id)
    }

    /// Whether an owner has a password: the home has somebody who can sign in to run it. Until
    /// it does, the welcome sets one up.
    pub fn owned(&self) -> bool {
        self.users
            .iter()
            .any(|user| user.role.runs_the_home() && self.hashes.contains_key(&user.id))
    }

    /// Whether anyone has a password, which is what makes Irori ask who is there
    /// (`docs/specs/config.md` §3.10).
    pub fn locked(&self) -> bool {
        !self.hashes.is_empty()
    }
}

/// Takes the chosen devices and entities out of `settings`, and the devices off the floorplan.
fn forget(
    settings: &mut Settings,
    device: impl Fn(&DeviceId) -> bool,
    entity: impl Fn(&SettingsKey) -> bool,
) {
    settings.devices.retain(|id, _| !device(id));
    settings.entities.retain(|key, _| !entity(key));
    for level in settings.floorplan.floors.values_mut() {
        level.devices.retain(|placed| !device(&placed.device));
    }
}

/// Whether `device` came in through `protocol`: its id starts with the protocol's, the way
/// [`irori_core::device_id_for`] makes every id — which is what finds the rows of devices that
/// aren't around right now, and that the core has never heard of this time round.
///
/// Where another installed protocol's prefix also fits, and is longer, the device is that one's.
fn brought_in_by(device: &DeviceId, protocol: &ProtocolId, others: &[ProtocolId]) -> bool {
    let prefix = |protocol: &ProtocolId| {
        let probe = UniqueId::try_from("x").expect("a valid unique id");
        let made = irori_core::device_id_for(protocol, &probe);
        made.as_str().strip_suffix('x').unwrap_or("").to_owned()
    };
    let own = prefix(protocol);
    !own.is_empty()
        && device.as_str().starts_with(&own)
        && !others
            .iter()
            .map(prefix)
            .any(|other| other.len() > own.len() && device.as_str().starts_with(&other))
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
    use irori_types::{Area, DeviceSettings, EntitySettings, Name};

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

    /// The demo extension, running, with every device it brings in added — the way a person
    /// would from "+ Add device".
    async fn demo_home(config: &Config, core: &Core) -> anyhow::Result<irori_core::ExtensionHost> {
        let host = irori_core::ExtensionHost::start(
            core,
            vec![
                irori_protocol::builtin::<irori_protocol_demo::Demo>()
                    .map_err(anyhow::Error::msg)?,
            ],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if core.held_devices().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            core.devices().is_empty(),
            "nothing joins the home on its own"
        );
        let found: Vec<DeviceId> = core.held_devices().into_iter().map(|d| d.id).collect();
        assert!(found.len() >= 3, "the demo devices were found");
        config
            .edit(core, |settings| {
                for id in &found {
                    settings.devices.entry(id.clone()).or_default().added = true;
                }
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        for _ in 0..500 {
            if core.devices().len() == found.len() && !core.entities().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(host)
    }

    /// `[devices] new` is from when a found device could join on its own. An `irori.toml` that
    /// still says it loads, and asking before adding stays on whatever it says.
    #[tokio::test]
    async fn an_old_new_devices_setting_no_longer_adds_anything() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        std::fs::write(dir.path().join("irori.toml"), "[devices]\nnew = \"add\"\n")?;
        let _config = Config::open_dir(dir.path(), &core);
        assert!(core.settings().ask_before_adding);
        Ok(())
    }

    /// Removing a device writes it out of the config files and out of the home — the device, its
    /// entities, everything said about either, and its spot on the floorplan — and it's listed
    /// as found again at once, to be added back if wanted.
    #[tokio::test]
    async fn removing_a_device_reaches_the_files_and_the_core_and_finds_it_again()
    -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);
        let host = demo_home(&config, &core).await?;
        let Some(device) = core.devices().first().cloned() else {
            anyhow::bail!("no demo device in the home");
        };
        let id = device.id.clone();
        let neighbours = core.devices().len() - 1;
        let first = core
            .entities()
            .into_iter()
            .find(|entity| entity.device_id.as_ref() == Some(&id))
            .expect("the device has entities");
        let key = SettingsKey::new(first.protocol, first.unique_id);

        config
            .edit(&core, |settings| {
                let said = settings.devices.entry(id.clone()).or_default();
                said.name = Some("Shared".parse::<Name>().expect("a valid name"));
                settings.entities.insert(
                    key.clone(),
                    EntitySettings {
                        name: Some("Reading light".parse::<Name>().expect("a valid name")),
                    },
                );
                settings.floorplan = irori_types::Floorplan {
                    floors: [(
                        "ground".parse().expect("a valid floor id"),
                        irori_types::Level {
                            devices: vec![irori_types::PlacedDevice::new(
                                id.clone(),
                                irori_types::Point { x: 10, y: 20 },
                            )],
                            ..Default::default()
                        },
                    )]
                    .into(),
                };
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        config
            .forget_device(&core, &id)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        assert!(
            !core.devices().iter().any(|d| d.id == id),
            "out of the home"
        );
        assert!(
            !core
                .entities()
                .iter()
                .any(|e| e.device_id.as_ref() == Some(&id)),
            "its entities went with it"
        );
        assert_eq!(core.devices().len(), neighbours, "its neighbours stayed");
        let found = core.held_devices();
        assert!(
            found
                .iter()
                .any(|d| d.id == id && d.name.as_str() != "Shared"),
            "found again straight away, under its own name: {found:?}"
        );
        let devices = std::fs::read_to_string(dir.path().join("devices.toml"))?;
        assert!(!devices.contains(&id.to_string()), "{devices}");
        let entities = std::fs::read_to_string(dir.path().join("entities.toml"))?;
        assert!(!entities.contains(&key.to_string()), "{entities}");
        assert!(
            core.floorplan()
                .floors
                .values()
                .all(|level| level.devices.is_empty()),
            "off the floorplan"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Uninstalling an extension removes every device it brought in — including one that isn't
    /// around right now — so installing it again starts with nothing in the home. Other
    /// extensions' devices are left alone.
    /// A device of another installed protocol whose id starts like this one's stays.
    #[test]
    fn a_longer_protocol_keeps_its_own_devices() {
        let demo: ProtocolId = "demo".parse().expect("valid");
        let extra: ProtocolId = "demo_extra".parse().expect("valid");
        let lamp: DeviceId = "demo_extra_lamp".parse().expect("valid");
        assert!(brought_in_by(&lamp, &demo, &[]), "nobody else claims it");
        assert!(!brought_in_by(&lamp, &demo, std::slice::from_ref(&extra)));
        assert!(brought_in_by(&lamp, &extra, &[demo]));
    }

    #[tokio::test]
    async fn forgetting_a_protocol_forgets_every_device_it_brought_in() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("devices.toml"),
            "[devices.demo_lamp]\nadded = true\n\n\
             [devices.demo_unplugged]\nname = \"Away\"\nadded = true\n\n\
             [devices.demonic_1]\nadded = true\n\n\
             [devices.esphome_00_11]\nadded = true\n",
        )?;
        std::fs::write(
            dir.path().join("entities.toml"),
            "[entities.\"demo/lamp-light\"]\nname = \"Lamp\"\n\n\
             [entities.\"esphome/00:11-light\"]\nname = \"Other\"\n",
        )?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);

        config
            .forget_protocol(&core, &"demo".parse().expect("a valid protocol id"))
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let devices = std::fs::read_to_string(dir.path().join("devices.toml"))?;
        assert!(!devices.contains("demo_lamp"), "{devices}");
        assert!(!devices.contains("demo_unplugged"), "{devices}");
        assert!(
            devices.contains("demonic_1"),
            "another protocol's: {devices}"
        );
        assert!(devices.contains("esphome_00_11"), "{devices}");
        let entities = std::fs::read_to_string(dir.path().join("entities.toml"))?;
        assert!(!entities.contains("demo/lamp-light"), "{entities}");
        assert!(entities.contains("esphome/00:11-light"), "{entities}");
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

    /// A form that sets a plain field and a secret is one update. Two edits would restart the
    /// extension after the plain field and before the secret, which is how Zigbee2MQTT would
    /// invent a network key of its own.
    #[tokio::test]
    async fn a_setting_and_a_secret_are_written_together() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);
        let helpers: irori_types::ExtensionId = "helpers".parse().expect("valid");

        config
            .edit_extension_with_secrets(
                &core,
                &helpers,
                |file| {
                    file.insert("serial_port".into(), serde_json::json!("/dev/ttyUSB0"));
                    Ok(())
                },
                &[(
                    "network_key".into(),
                    "00112233445566778899aabbccddeeff".into(),
                )],
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let extension = std::fs::read_to_string(dir.path().join("extensions/helpers.toml"))?;
        assert!(extension.contains("serial_port"), "{extension}");
        assert!(
            !extension.contains("network_key"),
            "the secret must stay out of the settings file:\n{extension}"
        );
        let secrets = std::fs::read_to_string(dir.path().join("secrets.toml"))?;
        assert!(secrets.contains("network_key"), "{secrets}");
        assert!(
            !secrets.contains("serial_port"),
            "the plain setting must stay out of secrets.toml:\n{secrets}"
        );
        Ok(())
    }

    /// The secrets write can fail after the settings file is already in place (a rename onto a
    /// `secrets.toml` that is somehow not a file). The settings file has to be the old one
    /// again, or the next reload restarts Zigbee with the new port and no key.
    #[tokio::test]
    async fn a_failed_secret_write_puts_the_settings_file_back() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let core = core();
        let config = Config::open_dir(dir.path(), &core);
        let helpers: irori_types::ExtensionId = "helpers".parse().expect("valid");

        config
            .edit_extension(&core, &helpers, |file| {
                file.insert("serial_port".into(), serde_json::json!("/dev/ttyUSB0"));
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        // A directory where the file should be: the rename of the new secrets file fails.
        std::fs::create_dir(dir.path().join("secrets.toml"))?;

        let failed = config
            .edit_extension_with_secrets(
                &core,
                &helpers,
                |file| {
                    file.insert("serial_port".into(), serde_json::json!("/dev/ttyACM0"));
                    Ok(())
                },
                &[(
                    "network_key".into(),
                    "00112233445566778899aabbccddeeff".into(),
                )],
            )
            .await;
        assert!(failed.is_err(), "the secrets write has to fail");

        let extension = std::fs::read_to_string(dir.path().join("extensions/helpers.toml"))?;
        assert!(
            extension.contains("/dev/ttyUSB0"),
            "the previous settings must still be the file:\n{extension}"
        );
        assert!(
            !extension.contains("/dev/ttyACM0"),
            "the new settings must not be left behind without the secret:\n{extension}"
        );
        Ok(())
    }
}
