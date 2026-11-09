//! External process for the SwitchBot extension.

use irori_protocol_switchbot::SwitchBot;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<SwitchBot>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
