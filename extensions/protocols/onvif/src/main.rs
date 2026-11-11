//! External process for the ONVIF Cameras extension.

use irori_protocol_onvif::Cameras;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Cameras>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
