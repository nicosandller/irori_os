//! External process for the TP-Link extension.

use irori_protocol_tplink::Tplink;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Tplink>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
