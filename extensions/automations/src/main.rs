//! External process for the Automations extension.

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_engine_automations::run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
