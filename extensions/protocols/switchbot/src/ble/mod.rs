//! SwitchBot devices over Bluetooth: their adverts, read without connecting. A device heard
//! nearby is read from its adverts instead of SwitchBot's cloud, which saves the account's calls
//! for what's out of range.

pub mod advert;
mod radio;

pub use advert::{Advert, parse};
pub use radio::listen;

/// What the radio says.
#[derive(Debug)]
pub enum Radio {
    Advert(Advert),
    /// Bluetooth can't be used, in words for a person. It's tried again a minute later.
    Failed(String),
}

/// Listens with adapter number `adapter` until `out` closes, trying again a minute after
/// Bluetooth fails (an adapter plugged in later, `bluetoothd` started late).
pub async fn keep_listening(adapter: usize, out: tokio::sync::mpsc::Sender<Radio>) {
    loop {
        match listen(adapter, out.clone()).await {
            Ok(()) => return,
            Err(why) => {
                if out.send(Radio::Failed(why)).await.is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
