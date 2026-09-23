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

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use irori_core::{Core, ExtensionHost, SystemClock, Timing};

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

    check_bind(bind, allow_unauthenticated_lan)?;
    if !bind.ip().is_loopback() {
        tracing::warn!(
            %bind,
            "listening beyond this machine WITHOUT authentication (--allow-unauthenticated-lan); \
             anyone on the network can reach this server and switch its devices"
        );
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
            let listener = bind_with_fallback(bind, bind_fallback)
                .await
                .with_context(|| format!("failed to listen on {bind}"))?;
            // After binding, so `--bind ...:0` shows the port the OS actually picked.
            banner::print(listener.local_addr()?);
            tracing::info!(
                addr = %listener.local_addr()?,
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
                )),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("server error");
            // Give every extension its chance to stop cleanly, even if the server failed.
            host.shutdown().await;
            served
        })
}

/// How many ports above the requested one Irori steps up before giving up, when no explicit
/// fallback is configured (default 127.0.0.1:8480 -> 8481 -> ... -> 8489).
const FALLBACK_STEPS: u16 = 9;

/// The addresses to try, in order: the requested `bind`, an explicit `bind_fallback` if given
/// and different, then up to `FALLBACK_STEPS` ports above `bind`.
fn fallback_candidates(bind: SocketAddr, bind_fallback: Option<SocketAddr>) -> Vec<SocketAddr> {
    let mut addrs = vec![bind];
    if let Some(fallback) = bind_fallback.filter(|f| *f != bind) {
        addrs.push(fallback);
    }
    if addrs.len() == 2 {
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
/// `bind_fallback` that address is tried next; otherwise Irori steps up past the taken port
/// `FALLBACK_STEPS` times. Returns the bound listener, whose actual address (`local_addr`)
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
async fn shutdown_signal() {
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
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("valid socket address")
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
    fn a_fallback_same_as_the_bind_steps_up() {
        let bind = addr("127.0.0.1:8480");
        let candidates = fallback_candidates(bind, Some(addr("127.0.0.1:8480")));
        assert_eq!(candidates.len(), 10);
        assert_eq!(candidates[0], addr("127.0.0.1:8480"));
        assert_eq!(candidates[9], addr("127.0.0.1:8489"));
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
        let taken = tokio::net::TcpListener::bind(addr("127.0.0.1:0"))
            .await
            .expect("a port to occupy");
        let occupied = taken.local_addr().expect("the occupied address");

        // Any of the ten stepped addresses is free, so this must succeed somewhere new.
        let listener = bind_with_fallback(occupied, None)
            .await
            .expect("stepping up finds a free port");
        let actual = listener.local_addr().expect("the actual address");
        assert!(actual.port() >= occupied.port());
        assert_ne!(actual.port(), occupied.port());
    }
}
