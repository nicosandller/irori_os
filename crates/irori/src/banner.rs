//! The terminal banner printed by `irori serve`.
//!
//! Hand-drawn here from the design in `assets/irori-cli-art.txt` (compact version), with the
//! version and URL filled in. The text file is a design reference, not read at runtime: if the
//! art changes, update this file to match.

use std::io::{IsTerminal as _, Write as _};
use std::net::{IpAddr, SocketAddr};

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
    let url = display_url(bind);
    let _ = write!(
        out,
        "\n  ┌───────┐\n  \
         │  {EMBER}███{RESET}  │  IroriOS {MUTED}{version}{RESET}\n  \
         │  {EMBER}███{RESET}  │  {MUTED}smart home core · {url}{RESET}\n  \
         └───────┘\n\n"
    );
}

/// A URL a person can open. A wildcard bind (`0.0.0.0`, `::`) isn't an address you can browse
/// to, so show the loopback URL and say it's listening on every interface.
fn display_url(bind: SocketAddr) -> String {
    let port = bind.port();
    match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            format!("http://127.0.0.1:{port} (listening on all interfaces)")
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            format!("http://[::1]:{port} (listening on all interfaces)")
        }
        _ => format!("http://{bind}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_binds_show_a_browsable_url() {
        let url = |s: &str| display_url(s.parse().expect("valid address"));
        assert_eq!(url("127.0.0.1:8480"), "http://127.0.0.1:8480");
        assert_eq!(url("192.168.1.20:8480"), "http://192.168.1.20:8480");
        assert_eq!(
            url("0.0.0.0:8480"),
            "http://127.0.0.1:8480 (listening on all interfaces)"
        );
        assert_eq!(
            url("[::]:8480"),
            "http://[::1]:8480 (listening on all interfaces)"
        );
        assert_eq!(url("[::1]:8480"), "http://[::1]:8480");
    }
}
