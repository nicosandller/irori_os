//! External process for the Zigbee extension.

use irori_protocol_zigbee::Zigbee;

#[tokio::main]
async fn main() {
    if let Err(error) = irori_protocol::serve::<Zigbee>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
