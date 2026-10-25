//! The machine's Bluetooth adapter, through btleplug: BlueZ over D-Bus on Linux, CoreBluetooth
//! on macOS. It scans for as long as the extension runs and hands every SwitchBot advert over.

use std::collections::{BTreeMap, HashMap};

use btleplug::api::{Central as _, CentralEvent, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use futures_util::StreamExt as _;
use tokio::sync::mpsc;

use super::{Advert, Radio};

/// Scans with adapter number `adapter` (Linux's `hciN`; the Mac has one) and sends each
/// SwitchBot advert to `out`, until `out` closes. An `Err` says why Bluetooth can't be used,
/// in words for a person.
pub async fn listen(adapter: usize, out: mpsc::Sender<Radio>) -> Result<(), String> {
    let central = open(adapter).await?;
    let mut events = central
        .events()
        .await
        .map_err(|error| format!("the Bluetooth adapter can't be watched: {error}"))?;
    central
        .start_scan(ScanFilter::default())
        .await
        .map_err(|error| format!("the Bluetooth adapter won't scan: {error}"))?;
    if out.send(Radio::Listening).await.is_err() {
        return Ok(());
    }
    // Whether each device nearby is a SwitchBot, once known: most of what's heard is phones and
    // watches, and asking the radio about each of their updates is wasted work.
    let mut switchbot: HashMap<_, bool> = HashMap::new();
    while let Some(event) = events.next().await {
        let id = match event {
            CentralEvent::ManufacturerDataAdvertisement {
                id,
                manufacturer_data,
            } => {
                let says = Advert {
                    manufacturer: manufacturer_data.into_iter().collect(),
                    ..Advert::default()
                }
                .is_switchbot();
                if !remember(&mut switchbot, id.clone(), says) {
                    continue;
                }
                id
            }
            CentralEvent::ServiceDataAdvertisement { id, service_data } => {
                let says = Advert {
                    service: service_data
                        .into_iter()
                        .map(|(uuid, data)| (uuid.to_string(), data))
                        .collect(),
                    ..Advert::default()
                }
                .is_switchbot();
                if !remember(&mut switchbot, id.clone(), says) {
                    continue;
                }
                id
            }
            CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => {
                if switchbot.get(&id) == Some(&false) {
                    continue;
                }
                id
            }
            _ => continue,
        };
        let Ok(peripheral) = central.peripheral(&id).await else {
            continue;
        };
        let Ok(Some(properties)) = peripheral.properties().await else {
            continue;
        };
        let advert = Advert {
            address: Some(properties.address.into_inner()),
            manufacturer: properties.manufacturer_data.into_iter().collect(),
            service: properties
                .service_data
                .into_iter()
                .map(|(uuid, data)| (uuid.to_string(), data))
                .collect::<BTreeMap<_, _>>(),
            rssi: properties.rssi,
        };
        let is = advert.is_switchbot();
        remember(&mut switchbot, id, is);
        if !is {
            continue;
        }
        match out.try_send(Radio::Advert(advert)) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
        }
    }
    Err("the Bluetooth adapter stopped scanning".into())
}

/// Notes whether device `id` is a SwitchBot, and answers whether it is. Once one is, it stays
/// one: a single advert without SwitchBot's data doesn't make it something else. Forgets
/// everything past a few thousand devices, since phones change their address often.
fn remember<K: std::hash::Hash + Eq>(known: &mut HashMap<K, bool>, id: K, is: bool) -> bool {
    if known.len() > 4096 {
        known.clear();
    }
    let entry = known.entry(id).or_insert(is);
    *entry |= is;
    *entry
}

async fn open(adapter: usize) -> Result<Adapter, String> {
    let manager = Manager::new()
        .await
        .map_err(|error| format!("Bluetooth isn't available: {error}"))?;
    let adapters = manager
        .adapters()
        .await
        .map_err(|error| format!("Bluetooth isn't available: {error}"))?;
    let count = adapters.len();
    adapters
        .into_iter()
        .nth(adapter)
        .ok_or_else(|| match count {
            0 => "no Bluetooth adapter found".to_owned(),
            _ => format!("there's no Bluetooth adapter number {adapter} (found {count})"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switchbot_stays_one_and_the_rest_are_skipped() {
        let mut known = HashMap::new();
        assert!(!remember(&mut known, "phone", false));
        assert!(remember(&mut known, "meter", true));
        // Its next advert carries only the other half of its data.
        assert!(remember(&mut known, "meter", false));
        // A device first heard without SwitchBot's data, then with it.
        assert!(remember(&mut known, "phone", true));
    }
}
