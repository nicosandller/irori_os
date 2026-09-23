//! External process for the Zigbee extension.

use irori_protocol_zigbee::Zigbee;

#[tokio::main]
async fn main() {
    // Without this, `tracing::info!`/`tracing::warn!` calls anywhere in this process — in
    // particular `supervisor::log_lines`, which is the only place Zigbee2MQTT's own stdout and
    // stderr (its real crash diagnostics included) reach anyone — are silent no-ops: nothing
    // records them without a subscriber, and none was ever installed here.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(error) = irori_protocol::serve::<Zigbee>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
