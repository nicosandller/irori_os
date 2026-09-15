//! `irori`: the single binary. Parses the CLI and wires the pieces together.

mod build_info;
mod db;
mod server;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "irori", version, about = "A fast, modular smart home core")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Irori server.
    Serve {
        /// Directory for runtime data (SQLite database).
        #[arg(long, env = "IRORI_DATA", default_value = "./data")]
        data: PathBuf,
        /// Address to listen on. Anything other than loopback also needs
        /// --allow-unauthenticated-lan until authentication exists.
        #[arg(long, env = "IRORI_BIND", default_value = "127.0.0.1:8480")]
        bind: SocketAddr,
        /// Allow a non-loopback --bind even though this build has no authentication yet.
        /// Temporary: removed when login and access tokens land (ROADMAP D12, M1.5).
        #[arg(long, env = "IRORI_ALLOW_UNAUTHENTICATED_LAN")]
        allow_unauthenticated_lan: bool,
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
            bind,
            allow_unauthenticated_lan,
        } => serve(data, bind, allow_unauthenticated_lan),
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

fn serve(data: PathBuf, bind: SocketAddr, allow_unauthenticated_lan: bool) -> anyhow::Result<()> {
    // Logs go to stdout, with no color codes when that's journald, Docker, or a file.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_target(false)
        .with_ansi(ansi)
        .init();

    check_bind(bind, allow_unauthenticated_lan)?;
    if !bind.ip().is_loopback() {
        tracing::warn!(
            %bind,
            "listening beyond this machine WITHOUT authentication (--allow-unauthenticated-lan); \
             anyone on the network can reach this server"
        );
    }

    let db = db::open(&data)?;
    tracing::info!(path = %db.path.display(), journal_mode = %db.journal_mode, "database ready");

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?
        .block_on(async move {
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to listen on {bind}"))?;
            tracing::info!(
                addr = %listener.local_addr()?,
                version = build_info::VERSION,
                "irori is ready"
            );
            axum::serve(listener, server::router(server::AppState::new(db)))
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("server error")
        })
}

/// ROADMAP D12: no unauthenticated server on the network. Until auth exists (M1.5), a
/// non-loopback bind must be asked for explicitly.
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
                allow_unauthenticated_lan: true,
                ..
            })
        ));
    }
}
