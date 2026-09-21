//! External process for the Demo extension.

use irori_protocol_demo::Demo;

#[tokio::main]
async fn main() {
    if let Err(error) = irori_protocol::serve::<Demo>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
