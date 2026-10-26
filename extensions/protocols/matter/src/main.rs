//! External process for the Matter extension.

use irori_protocol_matter::Matter;

#[tokio::main]
async fn main() {
    // The Matter server's own log reaches Irori's only through this subscriber.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Matter>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
