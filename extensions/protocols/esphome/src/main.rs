//! External process for the ESPHome extension.

use irori_int_esphome::Esphome;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_integration::serve::<Esphome>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
