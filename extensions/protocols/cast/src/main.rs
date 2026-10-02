//! External process for the Google Cast extension.

use irori_protocol_cast::Cast;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Cast>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
