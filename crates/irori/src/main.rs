//! `irori`: the single binary. Parses the CLI and wires the pieces together.

mod banner;
mod build_info;
mod config;
mod db;
mod extensions;
mod history;
mod host_info;
mod packages;
mod server;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use irori_core::{Core, ExtensionHost, SystemClock, Timing};
use tokio::sync::Notify;

#[derive(Debug, Parser)]
#[command(name = "irori", version = build_info::VERSION, about = "A fast, modular smart home core")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
        /// --allow-unauthenticated-lan until authentication exists. Also `[server] bind`;
        /// default 127.0.0.1:8480.
        #[arg(long, env = "IRORI_BIND")]
        bind: Option<SocketAddr>,
        /// Address to fall back to when --bind is already taken. Also `[server] bind_fallback`.
        /// Setting it equal to --bind locks the port: a taken one fails instead of stepping.
        /// Without one, Irori steps up past the taken address (8480 -> 8481 -> ...) and tells
        /// you where it ended up instead of failing.
        #[arg(long, env = "IRORI_BIND_FALLBACK")]
        bind_fallback: Option<SocketAddr>,
        /// Allow a non-loopback --bind even though this build has no authentication yet.
        /// Temporary: removed when login and access tokens land (ROADMAP D12, M1.5).
        #[arg(long, env = "IRORI_ALLOW_UNAUTHENTICATED_LAN", num_args = 0..=1, default_missing_value = "true")]
        allow_unauthenticated_lan: Option<bool>,
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            data,
            config,
            bind,
            bind_fallback,
            allow_unauthenticated_lan,
            log_level,
        } => {
            let flags = Flags {
                data,
                bind,
                bind_fallback,
                allow_unauthenticated_lan,
                log_level,
            };
            serve(config, flags)
        }
        Command::Version { json } => {
            let info = build_info::BuildInfo::current();
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("{info}");
            }
            Ok(())
        }
    }
}

/// What was given on the command line or in the environment, which wins over `irori.toml`.
#[derive(Debug, Default)]
struct Flags {
    data: Option<PathBuf>,
    bind: Option<SocketAddr>,
    bind_fallback: Option<SocketAddr>,
    allow_unauthenticated_lan: Option<bool>,
    log_level: Option<tracing::Level>,
}

/// Where Irori's own settings end up: a flag, else `irori.toml`, else the default.
#[derive(Debug, PartialEq, Eq)]
struct Resolved {
    data: PathBuf,
    bind: SocketAddr,
    bind_fallback: Option<SocketAddr>,
    allow_unauthenticated_lan: bool,
    log_level: tracing::Level,
}

fn resolve(
    flags: Flags,
    file: &irori_config::ServerSettings,
    config_dir: &std::path::Path,
) -> Resolved {
    use irori_config::LogLevel;
    let level = |level: LogLevel| match level {
        LogLevel::Error => tracing::Level::ERROR,
        LogLevel::Warn => tracing::Level::WARN,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Debug => tracing::Level::DEBUG,
        LogLevel::Trace => tracing::Level::TRACE,
    };
    Resolved {
        // A path in irori.toml is about that file's directory, not wherever Irori was started.
        data: flags
            .data
            .or_else(|| file.data.as_ref().map(|data| config_dir.join(data)))
            .unwrap_or_else(|| PathBuf::from("./data")),
        bind: flags
            .bind
            .or(file.bind)
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8480))),
        bind_fallback: flags.bind_fallback.or(file.bind_fallback),
        allow_unauthenticated_lan: flags
            .allow_unauthenticated_lan
            .or(file.allow_unauthenticated_lan)
            .unwrap_or(false),
        log_level: flags
            .log_level
            .or(file.log_level.map(level))
            .unwrap_or(tracing::Level::INFO),
    }
}

fn serve(config: PathBuf, flags: Flags) -> anyhow::Result<()> {
    // Read before anything else: the log level and the address come from it.
    let mut store = irori_config::Store::new(&config);
    let problems = store.reload();
    // Whether the address came from the command line (a `--bind` or an `IRORI_BIND` in the
    // environment) rather than irori.toml. Only then does a restart carry the address it
    // actually bound across the exec (restart_process): a config-file bind must not override a
    // `[server].bind` edit made while Irori ran. Read now — `resolve` consumes `flags`.
    let carry_bind = flags.bind.is_some();
    let Resolved {
        data,
        bind,
        bind_fallback,
        allow_unauthenticated_lan,
        log_level,
    } = resolve(flags, &store.irori().server, &config);

    // Logs go to stdout, with no color codes when that's journald, Docker, or a file.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_target(false)
        .with_ansi(ansi)
        .with_max_level(log_level)
        .init();

    // The unauthenticated-network guard must hold for whichever address we end up on: a
    // non-loopback fallback needs the same --allow-unauthenticated-lan as a non-loopback bind.
    for address in std::iter::once(bind).chain(bind_fallback) {
        check_bind(address, allow_unauthenticated_lan)?;
    }

    let db = db::open(&data)?;
    tracing::info!(path = %db.path.display(), journal_mode = %db.journal_mode, "database ready");
    let storage = Arc::new(db::SqliteStorage::open(&db)?);
    let packages_dir = packages::packages_dir(&data);
    let _ = std::fs::create_dir_all(&packages_dir);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?
        .block_on(async move {
            // Waking `restart` asks the server to shut down; `restarting` then tells the copy of
            // `serve` that picks up after that shutdown to re-exec this binary (`serve`), which
            // is how the Settings page's Restart button works (crates/irori/src/server.rs).
            let restart = Arc::new(Notify::new());
            let restarting = Arc::new(AtomicBool::new(false));

            let listener = bind_with_fallback(bind, bind_fallback)
                .await
                .with_context(|| format!("failed to listen on {bind}"))?;
            // After binding, so `--bind ...:0` shows the port the OS actually picked, and the
            // unauthenticated warning names the address we really ended up on (a fallback may
            // differ from `bind`, e.g. a loopback bind falling back to a LAN address).
            let address = listener.local_addr()?;
            banner::print(address);
            if !address.ip().is_loopback() {
                tracing::warn!(
                    %address,
                    "listening beyond this machine WITHOUT authentication \
                     (--allow-unauthenticated-lan); \
                     anyone on the network can reach this server and switch its devices"
                );
            }
            tracing::info!(
                addr = %address,
                version = build_info::VERSION,
                "irori is ready"
            );
            let core = Core::new(Arc::new(SystemClock));
            core.use_storage(storage);
            // Subscribe before any extension starts, so the log sees their first events.
            tokio::spawn(extensions::log_events(core.subscribe()));
            // And the recorder feeding the page's per-entity "last 24 hours" table. In memory,
            // so it starts empty with each server (the SQLite recorder, M1.3, keeps the rest).
            let history = history::History::default();
            tokio::spawn(history::record(history.clone(), core.subscribe()));
            // Before the extensions, so a device that arrives in the first second already has
            // the name and the room its owner gave it, rather than appearing under its old name
            // and moving a moment later.
            let settings = config::Config::open(store, &problems, &core);
            tokio::spawn(settings.clone().watch(core.clone()));
            tokio::spawn(settings.clone().remember_arrivals(core.clone()));
            // Helpers are core to Irori, not an installable extension: they run every time,
            // in-process, and never appear on the Extensions page.
            let builtins = vec![
                irori_protocol::builtin::<irori_helpers::Helpers>().map_err(anyhow::Error::msg)?,
            ];
            let host = ExtensionHost::start_with_packages(
                &core,
                builtins,
                Timing::default(),
                packages_dir,
            )
            .map_err(anyhow::Error::msg)?;

            let served = axum::serve(
                listener,
                server::router(server::AppState::new(
                    db,
                    core,
                    settings,
                    host.clone(),
                    history,
                    restart.clone(),
                    restarting.clone(),
                )),
            )
            .with_graceful_shutdown(shutdown_signal(restart))
            .await
            .context("server error");
            // Give every extension its chance to stop cleanly, even if the server failed.
            host.shutdown().await;
            if restarting.load(Ordering::SeqCst) {
                // Never returns: the current image is replaced by a fresh `irori serve`. Only a
                // failed exec comes back here.
                return restart_process(address, carry_bind);
            }
            served
        })
}

/// How many ports above the requested one Irori steps up before giving up, when no explicit
/// fallback is configured (default 127.0.0.1:8480 -> 8481 -> ... -> 8489).
const FALLBACK_STEPS: u16 = 9;

/// The addresses to try, in order. An explicit `bind_fallback` means Irori never steps
/// automatically, which is what a fixed-port deployment needs: it either gets `bind`, or
/// `bind_fallback`, or nothing to do. Without one, Irori steps up to `FALLBACK_STEPS` ports
/// above `bind`. A fallback equal to `bind` is how a fixed port says "fail loudly instead of
/// wandering": the addresses to try are just `bind`.
fn fallback_candidates(bind: SocketAddr, bind_fallback: Option<SocketAddr>) -> Vec<SocketAddr> {
    if bind_fallback == Some(bind) {
        return vec![bind];
    }
    let mut addrs = vec![bind];
    if let Some(fallback) = bind_fallback {
        addrs.push(fallback);
        return addrs;
    }
    let mut step = bind;
    let last = bind.port().saturating_add(FALLBACK_STEPS);
    while let Some(port) = step.port().checked_add(1) {
        if port > last {
            break;
        }
        step.set_port(port);
        addrs.push(step);
    }
    addrs
}

/// Binds the listener, falling back when the address is already taken. With an explicit
/// `bind_fallback` that address is tried next and Irori never steps automatically; without one,
/// Irori steps up past the taken port `FALLBACK_STEPS` times (a fallback equal to `bind` makes
/// it fail loudly instead). Returns the bound listener, whose actual address (`local_addr`)
/// may differ from `bind`.
async fn bind_with_fallback(
    bind: SocketAddr,
    bind_fallback: Option<SocketAddr>,
) -> anyhow::Result<tokio::net::TcpListener> {
    let candidates = fallback_candidates(bind, bind_fallback);
    for at in &candidates {
        match tokio::net::TcpListener::bind(*at).await {
            Ok(listener) => {
                if *at != bind {
                    tracing::warn!(
                        %bind,
                        actual = %listener.local_addr()?,
                        "the requested address was in use; listening on a fallback",
                    );
                }
                return Ok(listener);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(err) => return Err(err.into()),
        }
    }
    anyhow::bail!(
        "failed to listen on {bind}: the requested and fallback ports are all in use \
         (tried {:?})",
        candidates
    )
}
fn check_bind(bind: SocketAddr, allow_unauthenticated_lan: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        bind.ip().is_loopback() || allow_unauthenticated_lan,
        "refusing to listen on {bind}: this build has no authentication yet, so it would be \
         open to anyone on the network.\n\
         Use a loopback address (the default, 127.0.0.1:8480), or pass \
         --allow-unauthenticated-lan (IRORI_ALLOW_UNAUTHENTICATED_LAN=true) if you accept that."
    );
    Ok(())
}

/// Resolves on Ctrl-C or, on Unix, SIGTERM (what systemd sends).
async fn shutdown_signal(restart: Arc<Notify>) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "failed to listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            tracing::info!("shutting down");
        }
        () = terminate => {
            tracing::info!("shutting down");
        }
        // The Settings page's Restart button. The handler's notify_one retains the notification,
        // so this resolves however the ordering falls out — even a request that lands before
        // this select! was first polled stores a wake for it.
        () = restart.notified() => {
            tracing::info!("shutting down for a restart");
        }
    }
}

/// Starts this binary again, in this process. `exec` replaces the current image with the same
/// executable and the same arguments, so the process — and with it the container when Irori is
/// its PID 1, the systemd unit, and the terminal it was started from — stays what it was. No
/// supervisor has to be asked to bring it back. It only returns if the exec itself failed, and
/// the extensions were already stopped (`host.shutdown`), so nothing is left running twice.
///
/// The address Irori actually ended up on is carried in `IRORI_BIND` — which wins over
/// irori.toml (`resolve`) — but only when the current bind came from the command line (`carry_bind`,
/// i.e. `--bind` or an `IRORI_BIND` in the environment) and not from `irori.toml`. When it was
/// explicit, the address must survive the restart: a `--bind 127.0.0.1:0` has the OS pick the
/// port, and the restarted process must bind the address that worked rather than asking for a
/// fresh random one, or the page's port would move on every restart. (A bind the fallback stepped
/// away from a taken address is the same case.) When the address came from the config file, it is
/// *not* carried: the restarted process resolves `[server].bind` anew, so an edit made while Irori
/// ran — which `irori.toml` documents as taking effect on restart — applies instead of being
/// silently overridden by the old address. Any `--bind` argument is dropped from the argv — its
/// value, when written `--bind <addr>`, follows it — and `--bind-fallback` is left alone.
#[cfg(unix)]
fn restart_process(bind: SocketAddr, carry_bind: bool) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let current = std::env::current_exe().context("can't find what to restart")?;
    let mut command = std::process::Command::new(current);
    command.args(restart_args());
    if carry_bind {
        command.env("IRORI_BIND", bind.to_string());
    }
    let err = command.exec();
    tracing::error!(%err, "restart failed");
    anyhow::bail!("restart failed: {err}")
}

/// The arguments to run `irori serve` with again, minus any `--bind` (and, for `--bind <addr>`,
/// its value that follows), since the actual bound address goes in `IRORI_BIND` instead.
/// Everything else — `--bind-fallback` included — keeps its place.
fn restart_args() -> Vec<std::ffi::OsString> {
    let mut rest = std::env::args_os().skip(1);
    let mut args = Vec::new();
    while let Some(arg) = rest.next() {
        let shown = arg.to_string_lossy();
        if shown == "--bind" {
            let _ = rest.next(); // its value, also dropped
            continue;
        }
        if shown.starts_with("--bind=") {
            continue;
        }
        args.push(arg);
    }
    args
}

#[cfg(not(unix))]
fn restart_process(_bind: SocketAddr, _carry_bind: bool) -> anyhow::Result<()> {
    anyhow::bail!("Irori can only restart itself on Unix")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("valid socket address")
    }

    #[test]
    fn a_non_loopback_fallback_still_needs_the_lan_flag() {
        // A loopback bind is fine, but its fallback must satisfy the same guard: binding
        // 0.0.0.0 without --allow-unauthenticated-lan must be refused whether it's the bind
        // or the fallback that says so.
        let file = irori_config::ServerSettings {
            bind: Some(addr("127.0.0.1:8480")),
            bind_fallback: Some(addr("0.0.0.0:8481")),
            ..Default::default()
        };
        assert!(check_bind(file.bind.expect("the bind"), false).is_ok());
        if let Some(fallback) = file.bind_fallback {
            assert!(check_bind(fallback, false).is_err(), "{fallback}");
        }
        assert!(check_bind(addr("0.0.0.0:8481"), true).is_ok());
    }

    #[test]
    fn loopback_binds_need_no_flag() {
        assert!(check_bind(addr("127.0.0.1:8480"), false).is_ok());
        assert!(check_bind(addr("[::1]:8480"), false).is_ok());
    }

    #[test]
    fn network_binds_are_refused_without_the_flag() {
        for a in ["0.0.0.0:8480", "[::]:8480", "192.168.1.10:8480"] {
            let err = check_bind(addr(a), false).expect_err(a);
            assert!(err.to_string().contains("--allow-unauthenticated-lan"));
        }
    }

    #[test]
    fn network_binds_are_allowed_with_the_flag() {
        assert!(check_bind(addr("0.0.0.0:8480"), true).is_ok());
    }

    #[test]
    fn flag_parses_from_the_command_line() {
        let cli = Cli::try_parse_from(["irori", "serve", "--allow-unauthenticated-lan"]);
        assert!(matches!(
            cli.map(|c| c.command),
            Ok(Command::Serve {
                allow_unauthenticated_lan: Some(true),
                ..
            })
        ));
    }

    /// A flag wins over irori.toml, irori.toml wins over the default, and a path in the file is
    /// about the file's directory.
    #[test]
    fn a_flag_beats_the_file_and_the_file_beats_the_default() {
        let file = irori_config::ServerSettings {
            bind: Some(addr("0.0.0.0:9000")),
            bind_fallback: Some(addr("0.0.0.0:9001")),
            data: Some(PathBuf::from("state")),
            allow_unauthenticated_lan: Some(true),
            log_level: Some(irori_config::LogLevel::Debug),
        };
        let config = std::path::Path::new("/etc/irori");

        let from_file = resolve(Flags::default(), &file, config);
        assert_eq!(from_file.bind, addr("0.0.0.0:9000"));
        assert_eq!(from_file.bind_fallback, Some(addr("0.0.0.0:9001")));
        assert_eq!(from_file.data, PathBuf::from("/etc/irori/state"));
        assert!(from_file.allow_unauthenticated_lan);
        assert_eq!(from_file.log_level, tracing::Level::DEBUG);

        let flags = Flags {
            bind: Some(addr("127.0.0.1:8481")),
            bind_fallback: Some(addr("127.0.0.1:8482")),
            allow_unauthenticated_lan: Some(false),
            ..Flags::default()
        };
        let flagged = resolve(flags, &file, config);
        assert_eq!(flagged.bind, addr("127.0.0.1:8481"));
        assert_eq!(flagged.bind_fallback, Some(addr("127.0.0.1:8482")));
        assert!(!flagged.allow_unauthenticated_lan);

        let defaults = resolve(
            Flags::default(),
            &irori_config::ServerSettings::default(),
            config,
        );
        assert_eq!(defaults.bind, addr("127.0.0.1:8480"));
        assert_eq!(defaults.bind_fallback, None);
        assert_eq!(defaults.data, PathBuf::from("./data"));
        assert!(!defaults.allow_unauthenticated_lan);
        assert_eq!(defaults.log_level, tracing::Level::INFO);
    }

    #[test]
    fn an_explicit_fallback_is_tried_after_the_bind() {
        let bind = addr("127.0.0.1:8480");
        let candidates = fallback_candidates(bind, Some(addr("127.0.0.1:8481")));
        assert_eq!(
            candidates,
            vec![addr("127.0.0.1:8480"), addr("127.0.0.1:8481")]
        );
    }

    #[test]
    fn without_a_fallback_the_bind_steps_up_nine_ports() {
        let bind = addr("127.0.0.1:8480");
        let candidates = fallback_candidates(bind, None);
        assert_eq!(candidates.len(), 10);
        assert_eq!(candidates[0], addr("127.0.0.1:8480"));
        assert_eq!(candidates[9], addr("127.0.0.1:8489"));
    }

    #[test]
    fn a_fallback_same_as_the_bind_locks_the_port() {
        // `bind_fallback` equal to `bind` is how a fixed-port deployment says "don't step":
        // the only address to try is the bind itself, so a taken port fails loudly.
        let bind = addr("127.0.0.1:8480");
        let candidates = fallback_candidates(bind, Some(addr("127.0.0.1:8480")));
        assert_eq!(candidates, vec![addr("127.0.0.1:8480")]);
    }

    #[test]
    fn stepping_up_stops_at_the_port_range_end() {
        let bind = addr("127.0.0.1:65534");
        let candidates = fallback_candidates(bind, None);
        assert_eq!(candidates[0], addr("127.0.0.1:65534"));
        assert_eq!(
            *candidates.last().expect("a candidate"),
            addr("127.0.0.1:65535")
        );
    }

    #[tokio::test]
    async fn an_in_use_bind_falls_back_to_the_next_address() {
        // Occupy a port (0 -> the OS picks one), then ask to serve that same address with an
        // explicit fallback on port 0, which the OS always hands out a free port for.
        let taken = tokio::net::TcpListener::bind(addr("127.0.0.1:0"))
            .await
            .expect("a port to occupy");
        let occupied = taken.local_addr().expect("the occupied address");
        let any_free = addr("127.0.0.1:0");

        let listener = bind_with_fallback(occupied, Some(any_free))
            .await
            .expect("a fallback binds");
        let actual = listener.local_addr().expect("the actual address");
        assert_ne!(actual.port(), occupied.port());
    }

    #[tokio::test]
    async fn no_explicit_fallback_steps_up_past_the_taken_port() {
        // Waiting for a port we can't control (the OS hands out ephemeral ports anywhere)
        // turns a bind into a game of chance: any of the stepped ports may already be busy.
        // So reserve a run of consecutive ports, hold every one open, and ask to serve the
        // first: stepping up then has nowhere free to go, deterministically.
        let mut base: u16 = 33_000;
        let held: Vec<tokio::net::TcpListener> = loop {
            let mut held = Vec::new();
            for offset in 0..=FALLBACK_STEPS {
                let port = base.checked_add(offset).expect("a port in range");
                match tokio::net::TcpListener::bind(addr(&format!("127.0.0.1:{port}"))).await {
                    Ok(listener) => held.push(listener),
                    Err(_) => break,
                }
            }
            if held.len() == (FALLBACK_STEPS + 1) as usize {
                break held;
            }
            base += FALLBACK_STEPS + 1;
            assert!(base <= 65_500, "could not reserve a full run of free ports");
        };

        // Every candidate is taken (we hold them), so binding must report it and stop.
        let first = held[0].local_addr().expect("the held address");
        let err = bind_with_fallback(first, None)
            .await
            .expect_err("every candidate port is in use");
        assert!(
            err.to_string().contains("all in use"),
            "expected the all-in-use error, got: {err}"
        );
    }
}
