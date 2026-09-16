//! Repository automation, run as `cargo xtask <command>`.

mod deps;
mod schemas;
mod ui;

use anyhow::bail;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check-deps        enforce the crate dependency rules (ROADMAP §2.1)
  schemas [--check] write JSON Schemas from irori-types to schemas/, or check they're fresh
  ui                build the web UI (crates/irori-ui) into the folder the binary embeds";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-deps") => deps::run(),
        Some("ui") => ui::run(),
        Some("schemas") => match &args[1..] {
            [] => schemas::run(false),
            [flag] if flag == "--check" => schemas::run(true),
            other => bail!("unexpected arguments to `schemas`: {other:?}\n\n{USAGE}"),
        },
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}
