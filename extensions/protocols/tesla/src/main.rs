//! External process for the Tesla extension.

use irori_protocol_tesla::Tesla;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Tesla>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
