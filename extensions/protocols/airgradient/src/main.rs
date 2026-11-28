//! External process for the AirGradient extension.

use irori_protocol_airgradient::AirGradient;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<AirGradient>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
