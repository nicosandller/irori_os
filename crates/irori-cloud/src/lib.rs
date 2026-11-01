//! What an extension that talks to a vendor's cloud needs, kept out of the protocol SDK so local
//! extensions don't carry an HTTPS client they never use:
//!
//! - [`client`]: one HTTPS client, rustls, with timeouts and Irori's user agent.
//! - [`Secret`]: a credential that never prints and that the settings form never sends back.
//! - [`Budget`]: how many calls (or how much money) a day or a month allows, so polling slows
//!   down before a vendor's limit is hit rather than after.
//! - [`Backoff`] and [`retry_after`]: how long to wait after a failure, or as long as the vendor
//!   asked.

mod budget;
mod pace;
mod secret;

pub use budget::{Budget, Period};
pub use pace::{Backoff, LONGEST_WAIT, retry_after};
pub use reqwest;
pub use secret::Secret;

use std::time::Duration;

/// How long a request may take altogether. Vendor clouds answer in well under a second; one that
/// hangs shouldn't hold a protocol's loop.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The HTTPS client an extension uses for every call to its vendor. `product` names the extension
/// in the user agent ("irori-switchbot/0.1.0"), so a vendor reading its logs can tell who called.
pub fn client(product: &str) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(product)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
}
