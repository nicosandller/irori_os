//! The command tree. `serve` stays the server; everything else talks to one, or uninstalls
//! this binary.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Flags for a command that talks to a running Irori.
#[derive(Debug, Args)]
pub struct Remote {
    /// Where Irori is listening. Otherwise `<data>/cli.url`, or `./data/cli.url`, or
    /// `http://127.0.0.1:8480`.
    #[arg(long, global = true, env = "IRORI_URL")]
    pub url: Option<String>,
    /// A program token (`irori_…`). Setup commands need `irori login` instead.
    #[arg(long, global = true, env = "IRORI_TOKEN")]
    pub token: Option<String>,
    /// Data directory of a server on this machine, read for `cli.url` when `--url` is absent.
    /// Put it before the subcommand: `entities call` uses `--data` for the action's JSON.
    #[arg(long)]
    pub data: Option<PathBuf>,
    /// Skip certificate checks, for the certificate Irori made itself.
    #[arg(long, global = true)]
    pub insecure: bool,
    /// PEM certificate to trust besides the system roots.
    #[arg(long, global = true)]
    pub ca: Option<PathBuf>,
    /// Pretty JSON on stdout. Errors are `{ "error": "…" }`.
    #[arg(long, global = true)]
    pub json: bool,
    /// Don't ask before a destructive command. A full-access install still says so.
    #[arg(long, global = true)]
    pub yes: bool,
}

#[derive(Debug, Parser)]
#[command(name = "irori", version = crate::build_info::VERSION, about = "A fast, modular smart home core")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the Irori server. Also spelled `run`.
    #[command(visible_alias = "run")]
    Serve {
        /// Directory for what you've said about your home, and for irori.toml: rooms, names,
        /// keys, and Irori's own settings. Plain TOML you can edit by hand; see
        /// docs/specs/config.md.
        #[arg(long, env = "IRORI_CONFIG", default_value = "./config")]
        config: PathBuf,
        /// Directory for runtime data (SQLite database). Also `[server] data` in irori.toml;
        /// default ./data.
        #[arg(long, env = "IRORI_DATA")]
        data: Option<PathBuf>,
        /// Address to listen on. Anything other than loopback also needs
        /// --allow-unauthenticated-lan. A home with no password is open to whoever can reach
        /// it, and that stays even though the page can sign in. Also `[server] bind`;
        /// default 127.0.0.1:8480.
        #[arg(long, env = "IRORI_BIND")]
        bind: Option<std::net::SocketAddr>,
        /// Address to fall back to when --bind is already taken. Also `[server] bind_fallback`.
        /// Setting it equal to --bind locks the port: a taken one fails instead of stepping.
        /// Without one, Irori steps up past the taken address (8480 -> 8481 -> ...) and tells
        /// you where it ended up instead of failing.
        #[arg(long, env = "IRORI_BIND_FALLBACK")]
        bind_fallback: Option<std::net::SocketAddr>,
        /// Allow a non-loopback --bind. A home that has no password yet is open to whoever
        /// can reach it, so listening beyond this machine has to be asked for.
        #[arg(
            long,
            env = "IRORI_ALLOW_UNAUTHENTICATED_LAN",
            num_args = 0..=1,
            default_missing_value = "true"
        )]
        allow_unauthenticated_lan: Option<bool>,
        /// Serve over https, so passwords and sign-ins travel encrypted. Uses tls/cert.pem and
        /// tls/key.pem in the data directory, and makes its own certificate the first time if
        /// they aren't there (a browser asks about that one once). Also `[server] tls`.
        #[arg(long, env = "IRORI_TLS", num_args = 0..=1, default_missing_value = "true")]
        tls: Option<bool>,
        /// How much to log: error, warn, info, debug, or trace. `debug` shows every device and
        /// state change. Also `[server] log_level`; default info.
        #[arg(long, env = "IRORI_LOG_LEVEL")]
        log_level: Option<tracing::Level>,
    },
    /// Print version and build information.
    Version {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove the binary install.sh put down, its PATH line, and optionally a data directory.
    Uninstall(UninstallArgs),
    /// Back up this home to one file, list backups, or restore one. Works on the files, so it
    /// needs no running server; restore needs it stopped.
    Backup(BackupArgs),
    /// Print a shell completion script.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Sign in and remember the session on this machine. The password is not saved.
    Login(LoginArgs),
    /// Forget the saved session, and sign it out of Irori.
    Logout(OnlyRemote),
    /// Whether Irori is up, and the machine it is on.
    Status(OnlyRemote),
    /// Devices in the home, and devices an extension has found.
    Devices(DevicesArgs),
    /// Entities, their state, and telling one to do something.
    Entities(EntitiesArgs),
    /// Rooms.
    Areas(AreasArgs),
    /// Levels of the home.
    Floors(FloorsArgs),
    /// The drawn plan, as JSON. Drawing stays on the page.
    Floorplan(FloorplanArgs),
    /// Follow state changes until Ctrl-C.
    Watch(OnlyRemote),
    /// Official extensions: list, install, and uninstall.
    Extensions(ExtensionsArgs),
    /// Values Irori keeps itself: toggles, numbers, counters, choices, texts, buttons.
    Helpers(HelpersArgs),
    /// Programs that aren't the page. A token is shown once.
    Token(TokenArgs),
    /// People who can sign in.
    Users(UsersArgs),
    /// Where the home is, which is what arms time and sun.
    Place(PlaceArgs),
    /// How long history is kept.
    Recorder(RecorderArgs),
    /// What this Irori has said since it started.
    Logs(OnlyRemote),
    /// Start this Irori again. It stays where it runs.
    Restart(OnlyRemote),
    /// Flows. The extension has to be installed. Also spelled `rules`.
    #[command(alias = "rules")]
    Automations(AutomationsArgs),
    /// The assistant, when this build includes it.
    #[cfg(feature = "assist")]
    Assistant(AssistantArgs),
}

#[derive(Debug, Args)]
pub struct OnlyRemote {
    #[command(flatten)]
    pub remote: Remote,
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    #[command(flatten)]
    pub remote: Remote,
    /// Their name, as they type it.
    pub user: String,
    /// Read the password from stdin instead of asking.
    #[arg(long)]
    pub password_stdin: bool,
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Remove this binary, when it is the one install.sh put in `~/.irori/bin` or
    /// `/usr/local/bin`, and the `# irori` PATH line.
    #[arg(long)]
    pub binary: bool,
    /// Disable and remove `/etc/systemd/system/irori.service`.
    #[arg(long)]
    pub unit: bool,
    /// Delete this directory. Refused for `/` and your home directory.
    #[arg(long)]
    pub purge: Option<PathBuf>,
    /// Don't ask. Does not choose what to remove: pass `--binary`, `--unit`, or `--purge`.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct BackupArgs {
    /// The config directory, as for `irori serve`.
    #[arg(long, env = "IRORI_CONFIG", default_value = "./config", global = true)]
    pub config: PathBuf,
    /// The data directory. Also `[server] data` in irori.toml; default ./data.
    #[arg(long, env = "IRORI_DATA", global = true)]
    pub data: Option<PathBuf>,
    /// Print JSON.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: BackupCmd,
}

#[derive(Debug, Subcommand)]
pub enum BackupCmd {
    /// Write a backup now, the way `backup.toml` says (encrypted unless turned off). Safe while
    /// Irori is running.
    Create {
        /// Write it into this directory instead of `<data>/backups`.
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// List the backups on this machine, newest first.
    List {
        /// Look in this directory instead.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Open a backup and check it, as a restore would, without changing anything.
    Verify {
        /// A backup file, or the name of one in `<data>/backups`.
        archive: PathBuf,
        /// The recovery key of an encrypted backup. Also `IRORI_BACKUP_KEY`; asked for on a terminal.
        #[arg(long, env = "IRORI_BACKUP_KEY", hide_env_values = true)]
        key: Option<String>,
    },
    /// Replace this home with a backup. Irori must be stopped. The home as it is now is kept
    /// first as `pre-restore-*` in `<data>/backups`.
    Restore {
        /// A backup file, or the name of one in `<data>/backups`.
        archive: PathBuf,
        /// The recovery key of an encrypted backup. Also `IRORI_BACKUP_KEY`; asked for on a terminal.
        #[arg(long, env = "IRORI_BACKUP_KEY", hide_env_values = true)]
        key: Option<String>,
        /// Put back everything except entity history, which stays as it is now.
        #[arg(long)]
        keep_history: bool,
        /// Put back only these, comma-separated: settings, extensions, state, history.
        #[arg(long, value_delimiter = ',', conflicts_with = "keep_history")]
        only: Vec<String>,
        /// Don't ask.
        #[arg(long)]
        yes: bool,
    },
    /// Print the recovery key that encrypts backups on this machine. It is the only way into them
    /// from somewhere else: keep a copy that isn't on this machine.
    Key {
        /// Make a new key. Backups already made keep the key they were made with.
        #[arg(long)]
        new: bool,
    },
}

#[derive(Debug, Args)]
pub struct DevicesArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: DevicesCmd,
}

#[derive(Debug, Subcommand)]
pub enum DevicesCmd {
    /// Home devices, then devices that are found and not added yet.
    List,
    /// One device.
    Show { id: String },
    /// Add a device an extension has found.
    Add { id: String },
    /// Give a device a name.
    Rename { id: String, name: String },
    /// Put a device in a room, or `--none` for deliberately no room.
    Area {
        id: String,
        /// A room's id. Absent with `--none`.
        area: Option<String>,
        #[arg(long)]
        none: bool,
    },
    /// Say what a device is for. `--clear` takes that back.
    Describe {
        id: String,
        text: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Take a device out of the home. A Zigbee device is unpaired first; `--force` removes
    /// it even when the network doesn't answer.
    Remove {
        id: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Args)]
pub struct EntitiesArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: EntitiesCmd,
}

#[derive(Debug, Subcommand)]
pub enum EntitiesCmd {
    /// Every entity, or those of one device or room.
    List {
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        area: Option<String>,
    },
    Show {
        id: String,
    },
    /// Give an entity a name.
    Rename {
        id: String,
        name: String,
    },
    /// The last day of changes. `--since` keeps only later rows of that day; older readings
    /// are `entities summary`.
    History {
        id: String,
        #[arg(long)]
        since: Option<String>,
    },
    /// Hourly readings from `--since`.
    Summary {
        id: String,
        #[arg(long)]
        since: String,
    },
    /// Ask an entity to do something. `on`, `off`, and `toggle` are the usual three.
    Call {
        id: String,
        command: String,
        /// JSON object, the action's data.
        #[arg(long)]
        data: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct AreasArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: AreasCmd,
}

#[derive(Debug, Subcommand)]
pub enum AreasCmd {
    List,
    Add {
        name: String,
        #[arg(long)]
        floor: Option<String>,
    },
    Rename {
        id: String,
        name: String,
    },
    /// Move a room onto a floor, or off every floor with `--none`.
    Move {
        id: String,
        floor: Option<String>,
        #[arg(long)]
        none: bool,
    },
    Remove {
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct FloorsArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: FloorsCmd,
}

#[derive(Debug, Subcommand)]
pub enum FloorsCmd {
    List,
    /// `level` is 0 at the entrance and negative below ground.
    Add {
        name: String,
        level: i8,
    },
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        level: Option<i8>,
    },
    Remove {
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct FloorplanArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: FloorplanCmd,
}

#[derive(Debug, Subcommand)]
pub enum FloorplanCmd {
    Get,
    /// Replace the plan with a JSON file.
    Put {
        file: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct ExtensionsArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: ExtensionsCmd,
}

#[derive(Debug, Subcommand)]
pub enum ExtensionsCmd {
    /// The catalog. `--installed` keeps the ones in this home.
    List {
        #[arg(long)]
        installed: bool,
    },
    Show {
        id: String,
    },
    /// Install an official extension. `--update` replaces one that's already there and keeps
    /// its devices, settings, and data.
    Install {
        id: String,
        #[arg(long)]
        update: bool,
        /// Say yes to full access to this machine. The warning is still printed.
        #[arg(long)]
        approve_full_access: bool,
    },
    /// Install a package from an http(s) URL.
    InstallUrl {
        url: String,
        #[arg(long)]
        approve_full_access: bool,
    },
    /// Delete an extension and the devices it added. With no id, on a terminal, a menu.
    Uninstall {
        id: Option<String>,
    },
    /// Replace an extension's settings with a JSON object. Secrets stay out of this file.
    Settings {
        id: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Give an extension a secret it is waiting on. The value is read from the terminal,
    /// or from stdin with `--stdin`.
    Secret {
        id: String,
        /// The field it asked for.
        key: String,
        #[arg(long)]
        stdin: bool,
    },
    /// Run one of an extension's actions, or `--stop` a timed one.
    Action {
        id: String,
        action: String,
        #[arg(long)]
        stop: bool,
    },
    /// What an extension has said. `--follow` keeps printing until Ctrl-C.
    Log {
        id: String,
        #[arg(long)]
        follow: bool,
    },
}

#[derive(Debug, Args)]
pub struct HelpersArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: HelpersCmd,
}

#[derive(Debug, Subcommand)]
pub enum HelpersCmd {
    /// Every helper: its kind, its id, its entity, and its name.
    List,
    /// Make a helper. Its id comes from its name, and its entity is `<kind>.<id>`.
    Add {
        kind: HelperKind,
        name: String,
        #[command(flatten)]
        settings: HelperSettings,
    },
    /// Change a helper's settings. What isn't given stays as it is.
    Set {
        kind: HelperKind,
        /// The id, not the entity.
        id: String,
        /// A new name.
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        settings: HelperSettings,
    },
    Remove {
        kind: HelperKind,
        /// The id, not the entity.
        id: String,
    },
    /// A toggle's id comes from its name. The entity is `switch.<id>`.
    Toggle {
        #[command(subcommand)]
        cmd: ToggleCmd,
    },
}

/// A kind of helper, as a person says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HelperKind {
    /// On or off: `switch.<id>`.
    Toggle,
    /// A value within `--min` and `--max`: `number.<id>`.
    Number,
    /// A number counted up and down: `number.<id>`.
    Counter,
    /// One of the `--option`s: `select.<id>`.
    Choice,
    /// A few words: `text.<id>`.
    Text,
    /// Something to press: `button.<id>`, and `event.<id>` for when it was.
    Button,
    /// A calendar day: `date.<id>`.
    Date,
    /// A time of day: `time.<id>`.
    Time,
    /// A day and a time: `datetime.<id>`.
    Datetime,
    /// Counts down: `timer.<id>`, and `event.<id>` for when it starts and ends. `--initial`
    /// isn't for a timer; its length is `--step`, in seconds.
    Timer,
    /// On during a week's times, given with `--set mon='[["07:00","09:00"]]'`: `schedule.<id>`.
    Schedule,
    /// Several entities as one: `--set kind=light --set members='["light.a","light.b"]'`.
    Group,
    /// Several sensors' readings as one: `--set sources='["sensor.a","sensor.b"]' --set of=mean`.
    Combined,
    /// A switch shown as what it powers: `--set switch=switch.lamp_plug --set as=light`.
    SwitchAs,
    /// On while a reading is past a line: `--set source=sensor.x --set above=26`.
    Threshold,
    /// On while a reading rises or falls: `--set source=… --set direction=rising --set by=… --set over=…`.
    Trend,
    /// What a counting reading went up by: `--set source=… --set every=day`.
    Meter,
    /// The area under a reading, power into energy: `--set source=…`.
    Integral,
    /// How fast a reading changes: `--set source=… --set over=…`.
    Derivative,
    /// A reading's mean, min or max: `--set source=… --set of=mean --set over=…`.
    Statistics,
    /// How long, how often or how much of the time something was a value:
    /// `--set source=… --set is=true --set measure=time`.
    History,
}

impl HelperKind {
    /// Its table in `extensions/helpers.toml`, which is also its part of the API's path.
    pub fn table(self) -> &'static str {
        match self {
            Self::Toggle => "toggles",
            Self::Number => "numbers",
            Self::Counter => "counters",
            Self::Choice => "selects",
            Self::Text => "texts",
            Self::Button => "buttons",
            Self::Date => "dates",
            Self::Time => "times",
            Self::Datetime => "datetimes",
            Self::Timer => "timers",
            Self::Schedule => "schedules",
            Self::Group => "groups",
            Self::Combined => "combined",
            Self::SwitchAs => "switch_as",
            Self::Threshold => "thresholds",
            Self::Trend => "trends",
            Self::Meter => "meters",
            Self::Integral => "integrals",
            Self::Derivative => "derivatives",
            Self::Statistics => "statistics",
            Self::History => "history",
        }
    }
}

/// What a helper is set up with. Each kind takes the ones that mean something to it, and
/// Irori says so when it's given one that doesn't.
#[derive(Debug, Default, Args)]
pub struct HelperSettings {
    /// The least a number or a counter can be.
    #[arg(long, allow_negative_numbers = true)]
    pub min: Option<f64>,
    /// The most a number or a counter can be.
    #[arg(long, allow_negative_numbers = true)]
    pub max: Option<f64>,
    /// How finely a number is set, how much one count is, or how long a timer runs (seconds).
    #[arg(long)]
    pub step: Option<f64>,
    /// What a number counts in: `%`, `°C`, `min`.
    #[arg(long)]
    pub unit: Option<String>,
    /// How a number is set on a page: `slider` or `box`.
    #[arg(long)]
    pub mode: Option<String>,
    /// One thing a choice can be. Give it once for each.
    #[arg(long = "option")]
    pub options: Vec<String>,
    /// The fewest characters a text takes.
    #[arg(long)]
    pub min_length: Option<u32>,
    /// The most characters a text takes.
    #[arg(long)]
    pub max_length: Option<u32>,
    /// What it is until someone sets it: `on` or `off` for a toggle, a number, an option, a
    /// text, a date (`2026-10-14`), a time (`06:45`) or a moment (`2026-10-14T06:45:00Z`).
    #[arg(long, allow_hyphen_values = true)]
    pub initial: Option<String>,
    /// Any setting, as `key=value`, the way it's written in `extensions/helpers.toml`: a value
    /// that reads as JSON (a number, `true`, a list) is that, and anything else is text. Give
    /// it once for each.
    #[arg(long = "set", value_name = "KEY=VALUE", allow_hyphen_values = true)]
    pub set: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum ToggleCmd {
    Add {
        name: String,
    },
    /// The id, not the `switch.` entity.
    Remove {
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: TokenCmd,
}

#[derive(Debug, Subcommand)]
pub enum TokenCmd {
    List,
    /// Print the secret once. Either repeat `--scope`, or pass `--extension` and no scopes.
    Create {
        #[arg(long)]
        name: String,
        /// `registry:read`, `states:read`, `services:call`, `history:read`, or `events:read`.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// A token that can only open `/api/extension` for this extension.
        #[arg(long)]
        extension: Option<String>,
    },
    Revoke {
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct UsersArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: UsersCmd,
}

#[derive(Debug, Subcommand)]
pub enum UsersCmd {
    List,
    /// A password of at least 8 characters. Asked, unless `--password-stdin`.
    Add {
        name: String,
        /// `owner` or `user`.
        #[arg(long, default_value = "user")]
        role: String,
        #[arg(long)]
        password_stdin: bool,
    },
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        role: Option<String>,
        /// Read a new password from stdin.
        #[arg(long)]
        password_stdin: bool,
        /// Read the current password from stdin. Needed to change your own.
        #[arg(long)]
        current_password_stdin: bool,
    },
    Remove {
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct PlaceArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: PlaceCmd,
}

#[derive(Debug, Subcommand)]
pub enum PlaceCmd {
    Show,
    /// Coordinates and a time zone name, for example `Europe/Stockholm`.
    Set {
        /// A label for the coordinates, like a city. The time zone is separate.
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        latitude: f64,
        #[arg(long)]
        longitude: f64,
        #[arg(long)]
        timezone: String,
    },
}

#[derive(Debug, Args)]
pub struct RecorderArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: RecorderCmd,
}

#[derive(Debug, Subcommand)]
pub enum RecorderCmd {
    Show,
    /// Detailed history is kept this many days. Hourly summaries follow, unless
    /// `--summary-days` stops them.
    Set {
        #[arg(long)]
        retain_days: u32,
        #[arg(long)]
        summary_days: Option<u32>,
    },
}

#[derive(Debug, Args)]
pub struct AutomationsArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: AutomationsCmd,
}

#[derive(Debug, Subcommand)]
pub enum AutomationsCmd {
    List,
    Show {
        id: String,
    },
    /// Check a flow JSON file, or the saved flow when no file is given.
    Validate {
        id: Option<String>,
        file: Option<PathBuf>,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Delete {
        id: String,
    },
    /// Runs of one flow, or every run that's going when no id is given.
    Runs {
        id: Option<String>,
    },
    /// One kept run.
    Run {
        id: String,
        run_id: String,
    },
    Cancel {
        run_id: String,
    },
    /// Fire a trigger. `--dry` doesn't touch the home.
    Test {
        id: String,
        #[arg(long)]
        trigger: String,
        #[arg(long)]
        dry: bool,
    },
    /// Replay the last day.
    Backtest {
        id: String,
    },
    Versions {
        id: String,
    },
    Restore {
        id: String,
        version: String,
    },
}

#[cfg(feature = "assist")]
#[derive(Debug, Args)]
pub struct AssistantArgs {
    #[command(flatten)]
    pub remote: Remote,
    #[command(subcommand)]
    pub cmd: AssistantCmd,
}

#[cfg(feature = "assist")]
#[derive(Debug, Subcommand)]
pub enum AssistantCmd {
    Status,
    /// Replace the assistant settings with a JSON object.
    Settings {
        file: PathBuf,
    },
    Install,
    Uninstall,
    Pull {
        tag: String,
    },
    Load {
        tag: String,
    },
    Unload {
        tag: String,
    },
    Forget {
        tag: String,
    },
    /// Ask one question and print the answer.
    Ask {
        #[arg(long, default_value = "general")]
        scope: String,
        message: String,
    },
    Log,
}
