//! `irori`: the single binary. Parses the CLI and wires the pieces together.

#[cfg(feature = "assist")]
mod assistant;
mod banner;
mod build_info;
mod cli;
mod config;
mod db;
mod extensions;
mod history;
mod host_info;
#[cfg(feature = "assist")]
mod ollama;
mod packages;
mod serial;
mod server;
mod syslog;
mod system_device;
mod tls;
mod usage;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context as _;
use clap::Parser;
use irori_core::{Core, ExtensionHost, SystemClock, Timing};
use tokio::sync::Notify;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Serve {
            data,
            config,
            bind,
            bind_fallback,
            allow_unauthenticated_lan,
            tls,
            log_level,
        } => {
            let flags = Flags {
                data,
                bind,
                bind_fallback,
                allow_unauthenticated_lan,
                tls,
                log_level,
            };
            serve(config, flags)
        }
        other => cli::execute(other),
    }
}

/// What was given on the command line or in the environment, which wins over `irori.toml`.
#[derive(Debug, Default)]
struct Flags {
    data: Option<PathBuf>,
    bind: Option<SocketAddr>,
    bind_fallback: Option<SocketAddr>,
    allow_unauthenticated_lan: Option<bool>,
    tls: Option<bool>,
    log_level: Option<tracing::Level>,
}

/// Where Irori's own settings end up: a flag, else `irori.toml`, else the default.
#[derive(Debug, PartialEq, Eq)]
struct Resolved {
    data: PathBuf,
    bind: SocketAddr,
    bind_fallback: Option<SocketAddr>,
    allow_unauthenticated_lan: bool,
    tls: bool,
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
        tls: flags.tls.or(file.tls).unwrap_or(false),
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
        tls,
        log_level,
    } = resolve(flags, &store.irori().server, &config);

    // Logs go to stdout, with no color codes when that's journald, Docker, or a file. The same
    // lines are kept a second time in memory for the Settings page's log window (`syslog`),
    // because stdout belongs to whoever started this process: on a box Irori starts itself there
    // is nothing else to read.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let log = Arc::new(syslog::Log::default());
    tracing_subscriber::fmt()
        .with_writer(syslog::Tee::new(Arc::clone(&log)))
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
    let recorder_settings = store.irori().recorder.clone();
    let retention = irori_recorder::Retention::new(
        recorder_settings.retain_days,
        recorder_settings.summary_days,
    )
    .context("invalid [recorder] settings")?;
    tracing::info!(path = %db.path.display(), journal_mode = %db.journal_mode, "database ready");
    // Irori's own device reports how full this volume is.
    let _ = system_device::DATA_DIR.set(db.path.clone());
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
            // The CLI on this machine reads this. A wildcard bind is written as loopback:
            // 0.0.0.0 is not an address a client can connect to.
            write_cli_url(&data, address, tls);
            // Read, or made, before anything is served: a certificate that can't be used is a
            // reason not to start, not a reason to quietly serve in the clear.
            let tls_config = if tls {
                Some(tls::config(&data, address)?)
            } else {
                None
            };
            server::serve_cookies_over_tls(tls);
            banner::print(address, tls);
            if !address.ip().is_loopback() {
                tracing::warn!(
                    %address,
                    "listening beyond this machine (--allow-unauthenticated-lan): until the \
                     home's owner is set up with a password, anyone on the network can reach \
                     this server and switch its devices"
                );
                if !tls {
                    tracing::warn!(
                        "serving plain http: passwords and sign-ins can be read by anything on \
                         the network. Start with --tls to encrypt them"
                    );
                }
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
            // The diary the page's "last 24 hours" reads, and that engines read when they ask
            // for history. It is the same database as everything else; the recorder keeps its
            // own connection so a state change does not wait on a session or a chat.
            let history = history::History::open(&db.path, retention)
                .context("failed to open entity history")?;
            tokio::spawn(history::record(
                core.clone(),
                history.clone(),
                core.subscribe(),
            ));
            // Engines that ask for history (`history:read`) read the same shelf, and engines
            // whose permissions name the config directory are told where it is.
            core.use_history(Arc::new(history.clone()));
            core.use_config_dir(config.clone());
            // Before the extensions, so a device that arrives in the first second already has
            // the name and the room its owner gave it, rather than appearing under its old name
            // and moving a moment later.
            let settings = config::Config::open(store, &problems, &core);
            tokio::spawn(settings.clone().watch(core.clone(), history.clone()));
            // The Ollama Irori installed for a local model, if there is one, comes up with the
            // server, and the model in use is loaded again. Nothing waits on it: the assistant
            // says "not ready" until it is.
            #[cfg(feature = "assist")]
            {
                let settings = settings.clone();
                let data = data.clone();
                tokio::spawn(async move { assistant::wake(&settings, &data).await });
            }
            // Helpers and Irori's own device are core to Irori, not installable extensions: they
            // run every time, in-process, and never appear on the Extensions page.
            let builtins = vec![
                irori_protocol::builtin::<irori_helpers::Helpers>().map_err(anyhow::Error::msg)?,
                irori_protocol::builtin::<system_device::IroriDevice>()
                    .map_err(anyhow::Error::msg)?,
            ];
            let host = ExtensionHost::start_with_packages(
                &core,
                builtins,
                Timing::default(),
                packages_dir,
            )
            .map_err(anyhow::Error::msg)?;

            // With where each request came from: wrong passwords are counted per machine,
            // and one guessing can't make another wait.
            let app = server::router(server::AppState::new(
                db,
                core,
                settings,
                host.clone(),
                history,
                log,
                restart.clone(),
                restarting.clone(),
            ))
            .into_make_service_with_connect_info::<tls::ClientAddr>();
            let served = match tls_config {
                Some(config) => {
                    axum::serve(tls::TlsListener::new(listener, config)?, app)
                        .with_graceful_shutdown(shutdown_signal(restart))
                        .await
                }
                None => {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(shutdown_signal(restart))
                        .await
                }
            }
            .context("server error");
            // Give every extension its chance to stop cleanly, even if the server failed.
            host.shutdown().await;
            if restarting.load(Ordering::SeqCst) {
                // Never returns: the current image is replaced by a fresh `irori serve`. Only a
                // failed exec comes back here. Leave cli.url for the new process to replace.
                let failed = restart_process(address, carry_bind);
                remove_cli_url(&data);
                return failed;
            }
            // A stop, not a restart: the next `irori` must not keep talking to a server
            // that is gone. The new process writes the file again when it binds.
            remove_cli_url(&data);
            served
        })
}

/// `http://127.0.0.1:8480`, with a wildcard address rewritten to this machine.
fn cli_origin(address: SocketAddr, tls: bool) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    let scheme = if tls { "https" } else { "http" };
    format!("{scheme}://{}", SocketAddr::new(ip, address.port()))
}

fn write_cli_url(data: &std::path::Path, address: SocketAddr, tls: bool) {
    let path = data.join("cli.url");
    let body = format!("{}\n", cli_origin(address, tls));
    if let Err(error) = std::fs::write(&path, body) {
        tracing::warn!(path = %path.display(), %error, "couldn't write cli.url");
    }
}

fn remove_cli_url(data: &std::path::Path) {
    let path = data.join("cli.url");
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(path = %path.display(), %error, "couldn't remove cli.url"),
    }
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
        "refusing to listen on {bind}: until the home's owner is set up with a password it \
         would be open to anyone on the network.\n\
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
    use crate::cli::{Cli, Command};

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("valid socket address")
    }

    #[test]
    fn a_wildcard_bind_is_written_as_loopback() {
        assert_eq!(
            cli_origin(addr("0.0.0.0:8480"), false),
            "http://127.0.0.1:8480"
        );
        assert_eq!(cli_origin(addr("[::]:8480"), true), "https://[::1]:8480");
        assert_eq!(
            cli_origin(addr("192.168.1.10:9000"), false),
            "http://192.168.1.10:9000"
        );
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
            tls: Some(true),
            log_level: Some(irori_config::LogLevel::Debug),
        };
        let config = std::path::Path::new("/etc/irori");

        let from_file = resolve(Flags::default(), &file, config);
        assert_eq!(from_file.bind, addr("0.0.0.0:9000"));
        assert_eq!(from_file.bind_fallback, Some(addr("0.0.0.0:9001")));
        assert_eq!(from_file.data, PathBuf::from("/etc/irori/state"));
        assert!(from_file.allow_unauthenticated_lan);
        assert!(from_file.tls);
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
        assert!(!defaults.tls, "https is asked for, never assumed");
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
