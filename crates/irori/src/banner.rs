//! The terminal banner printed by `irori serve` (assets/irori-cli-art.txt, compact version).

use std::io::{IsTerminal as _, Write as _};
use std::net::SocketAddr;

const EMBER: &str = "\x1b[38;2;196;85;43m"; // #c4552b
const MUTED: &str = "\x1b[38;2;154;143;134m"; // #9a8f86
const RESET: &str = "\x1b[0m";

/// Prints the banner to stdout, but only for a person at a terminal: journald, Docker logs,
/// and files get plain log lines only.
pub fn print(bind: SocketAddr) {
    let mut out = std::io::stdout().lock();
    if !out.is_terminal() {
        return;
    }
    let version = crate::build_info::VERSION;
    let _ = write!(
        out,
        "\n  ┌───────┐\n  \
         │  {EMBER}███{RESET}  │  IroriOS {MUTED}{version}{RESET}\n  \
         │  {EMBER}███{RESET}  │  {MUTED}smart home core · http://{bind}{RESET}\n  \
         └───────┘\n\n"
    );
}
