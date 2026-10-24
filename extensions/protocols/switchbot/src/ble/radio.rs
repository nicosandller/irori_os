//! The machine's Bluetooth adapter, through btleplug: BlueZ over D-Bus on Linux, CoreBluetooth
//! on macOS. It scans for as long as the extension runs and hands every SwitchBot advert over.

use std::collections::BTreeMap;

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
    while let Some(event) = events.next().await {
        let id = match event {
            CentralEvent::DeviceDiscovered(id)
            | CentralEvent::DeviceUpdated(id)
            | CentralEvent::ManufacturerDataAdvertisement { id, .. }
            | CentralEvent::ServiceDataAdvertisement { id, .. } => id,
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
        if !advert.is_switchbot() {
            continue;
        }
        match out.try_send(Radio::Advert(advert)) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
        }
    }
    Err("the Bluetooth adapter stopped scanning".into())
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
