//! The command tree besides `serve` (`docs/specs/cli.md`).

mod args;
mod automations;
mod connect;
mod extensions;
mod home;
mod output;
mod prompt;
mod setup;
mod uninstall;
mod watch;

#[cfg(feature = "assist")]
mod assistant;

pub use args::{Cli, Command};

use args::Command as Cmd;

pub fn execute(command: Command) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow::anyhow!("failed to start the async runtime: {error}"))?;
    match command {
        Cmd::Serve { .. } => unreachable!("serve stays in main"),
        Cmd::Version { json } => {
            let info = crate::build_info::BuildInfo::current();
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("{info}");
            }
            Ok(())
        }
        Cmd::Uninstall(args) => output::finish(false, uninstall::run(args)),
        Cmd::Completions { shell } => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut cmd, "irori", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Login(args) => {
            let json = args.remote.json;
            output::finish(json, runtime.block_on(setup::login(args)))
        }
        Cmd::Logout(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::logout(&args.remote)),
        ),
        Cmd::Status(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::status(&args.remote)),
        ),
        Cmd::Devices(args) => output::finish(
            args.remote.json,
            runtime.block_on(home::devices(&args.remote, args.cmd)),
        ),
        Cmd::Entities(args) => output::finish(
            args.remote.json,
            runtime.block_on(home::entities(&args.remote, args.cmd)),
        ),
        Cmd::Areas(args) => output::finish(
            args.remote.json,
            runtime.block_on(home::areas(&args.remote, args.cmd)),
        ),
        Cmd::Floors(args) => output::finish(
            args.remote.json,
            runtime.block_on(home::floors(&args.remote, args.cmd)),
        ),
        Cmd::Floorplan(args) => output::finish(
            args.remote.json,
            runtime.block_on(home::floorplan(&args.remote, args.cmd)),
        ),
        Cmd::Watch(args) => output::finish(
            args.remote.json,
            runtime.block_on(home::watch(&args.remote)),
        ),
        Cmd::Extensions(args) => output::finish(
            args.remote.json,
            runtime.block_on(extensions::extensions(&args.remote, args.cmd)),
        ),
        Cmd::Helpers(args) => output::finish(
            args.remote.json,
            runtime.block_on(extensions::helpers(&args.remote, args.cmd)),
        ),
        Cmd::Token(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::token(&args.remote, args.cmd)),
        ),
        Cmd::Users(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::users(&args.remote, args.cmd)),
        ),
        Cmd::Place(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::place(&args.remote, args.cmd)),
        ),
        Cmd::Recorder(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::recorder(&args.remote, args.cmd)),
        ),
        Cmd::Logs(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::logs(&args.remote)),
        ),
        Cmd::Restart(args) => output::finish(
            args.remote.json,
            runtime.block_on(setup::restart(&args.remote)),
        ),
        Cmd::Automations(args) => output::finish(
            args.remote.json,
            runtime.block_on(automations::run(&args.remote, args.cmd)),
        ),
        #[cfg(feature = "assist")]
        Cmd::Assistant(args) => output::finish(
            args.remote.json,
            runtime.block_on(assistant::run(&args.remote, args.cmd)),
        ),
    }
}
