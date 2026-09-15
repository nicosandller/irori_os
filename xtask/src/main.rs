//! Repository automation, run as `cargo xtask <command>`.

mod deps;

use anyhow::bail;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check-deps   enforce the crate dependency rules (ROADMAP §2.1)";

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("check-deps") => deps::run(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}
