//! External process for the MQTT extension.

use irori_int_mqtt::Mqtt;

#[tokio::main]
async fn main() {
    if let Err(error) = irori_integration::serve::<Mqtt>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
