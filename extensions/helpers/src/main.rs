//! External process for the Helpers extension.

use irori_protocol_helpers::Helpers;

#[tokio::main]
async fn main() {
    if let Err(error) = irori_protocol::serve::<Helpers>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
