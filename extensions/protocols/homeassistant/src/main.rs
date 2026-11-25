//! External process for the Home Assistant extension.

use irori_protocol_homeassistant::HomeAssistant;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<HomeAssistant>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
