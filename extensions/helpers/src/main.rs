//! External process for the Helpers extension.

use irori_int_helpers::Helpers;

#[tokio::main]
async fn main() {
    if let Err(error) = irori_integration::serve::<Helpers>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
