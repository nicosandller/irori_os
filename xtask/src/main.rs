//! Repository automation, run as `cargo xtask <command>`.

mod deps;
mod schemas;

use anyhow::bail;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check-deps        enforce the crate dependency rules (ROADMAP §2.1)
  schemas [--check] write JSON Schemas from irori-types to schemas/, or check they're fresh";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-deps") => deps::run(),
        Some("schemas") => schemas::run(args.iter().any(|a| a == "--check")),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}
