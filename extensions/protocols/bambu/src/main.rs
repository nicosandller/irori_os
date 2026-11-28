//! External process for the Bambu Lab extension.

use irori_protocol_bambu::Bambu;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Bambu>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
