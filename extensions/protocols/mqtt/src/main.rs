//! External process for the MQTT extension.

use irori_protocol_mqtt::Mqtt;

#[tokio::main]
async fn main() {
    if let Err(error) = irori_protocol::serve::<Mqtt>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
