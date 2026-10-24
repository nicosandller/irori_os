//! What a SwitchBot device says in its Bluetooth adverts, read without connecting.
//!
//! Ported from pySwitchbot's `adv_parsers` (MIT), byte for byte, and tested against the adverts
//! in its own tests. A device advertises under SwitchBot's company id (2409, `0x0969`) with its
//! MAC address in the first six bytes, which is also its id in SwitchBot's cloud: that's what
//! makes one device of the two routes, and what identifies it on macOS, where the radio hides
//! addresses. The service data's first byte says the model; newer models say it with a product
//! id at the end of the service data instead.

use std::collections::BTreeMap;

use crate::model::{Bolt, Reading};

/// SwitchBot's company id (Woan Technology).
pub const COMPANY: u16 = 2409;
/// The Bot's and the oldest Curtain's company id.
const COMPANY_OLD: u16 = 89;
/// The ultrasonic Humidifier's.
const COMPANY_HUMIDIFIER: u16 = 741;
/// SwitchBot's service data, and an older one.
pub const SERVICE: &str = "0000fd3d-0000-1000-8000-00805f9b34fb";
const SERVICE_OLD: &str = "00000d00-0000-1000-8000-00805f9b34fb";

/// One advert as the radio hands it over: manufacturer and service data merged per device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Advert {
    /// The device's address, when the radio gives one (Linux does, macOS doesn't).
    pub address: Option<[u8; 6]>,
    pub manufacturer: BTreeMap<u16, Vec<u8>>,
    /// By UUID, lower case.
    pub service: BTreeMap<String, Vec<u8>>,
    pub rssi: Option<i16>,
}

impl Advert {
    /// Whether it might be a SwitchBot device at all.
    pub fn is_switchbot(&self) -> bool {
        self.manufacturer.contains_key(&COMPANY)
            || self.service.contains_key(SERVICE)
            || self.service.contains_key(SERVICE_OLD)
    }

    fn service_data(&self) -> Option<&[u8]> {
        self.service
            .get(SERVICE)
            .or_else(|| self.service.get(SERVICE_OLD))
            .map(Vec::as_slice)
    }

    fn manufacturer_data(&self) -> Option<(u16, &[u8])> {
        [COMPANY, COMPANY_HUMIDIFIER, COMPANY_OLD]
            .into_iter()
            .find_map(|id| self.manufacturer.get(&id).map(|data| (id, data.as_slice())))
    }

    /// The device's MAC address as SwitchBot's cloud writes it: upper case, no colons. From the
    /// advert itself when it carries it, else from the radio.
    pub fn mac(&self) -> Option<String> {
        let from_advert = self
            .manufacturer
            .get(&COMPANY)
            .filter(|data| data.len() >= 6)
            .map(|data| <[u8; 6]>::try_from(&data[..6]).expect("six bytes"));
        let bytes = from_advert.or(self.address.filter(|a| a.iter().any(|&b| b != 0)))?;
        Some(bytes.iter().map(|b| format!("{b:02X}")).collect())
    }
}

/// A SwitchBot device heard over Bluetooth.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    pub mac: String,
    /// Its type as SwitchBot's cloud names it, which [`crate::model::Family::of`] knows.
    pub device_type: &'static str,
    pub reading: Reading,
    pub rssi: Option<i16>,
}

/// The models whose adverts are read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Model {
    Contact,
    Bot,
    Motion,
    Curtain,
    Curtain3,
    Meter,
    MeterPlus,
    OutdoorMeter,
    MeterPro,
    MeterProCo2,
    Hub2,
    Hub3,
    PlugUs,
    PlugJp,
    PlugEu,
    Humidifier,
    Evaporative,
    Lock,
    LockPro,
    LockLite,
    LockUltra,
    LockVision,
    LockVisionPro,
    LockProWifi,
    BlindTilt,
    Leak,
    Relay1,
    Relay1Pm,
    Relay2Pm,
    Garage,
    RollerShade,
    Presence,
}

use Model as M;

/// Every model and its cloud name, for a hint from the cloud's device list.
const TYPES: [(Model, &str); 32] = [
    (M::Contact, "Contact Sensor"),
    (M::Bot, "Bot"),
    (M::Motion, "Motion Sensor"),
    (M::Curtain, "Curtain"),
    (M::Curtain3, "Curtain3"),
    (M::Meter, "Meter"),
    (M::MeterPlus, "MeterPlus"),
    (M::OutdoorMeter, "WoIOSensor"),
    (M::MeterPro, "MeterPro"),
    (M::MeterProCo2, "MeterPro(CO2)"),
    (M::Hub2, "Hub 2"),
    (M::Hub3, "Hub 3"),
    (M::PlugUs, "Plug Mini (US)"),
    (M::PlugJp, "Plug Mini (JP)"),
    (M::PlugEu, "Plug Mini (EU)"),
    (M::Humidifier, "Humidifier"),
    (M::Evaporative, "Humidifier2"),
    (M::Lock, "Smart Lock"),
    (M::LockPro, "Smart Lock Pro"),
    (M::LockLite, "Smart Lock Lite"),
    (M::LockUltra, "Smart Lock Ultra"),
    (M::LockVision, "Lock Vision"),
    (M::LockVisionPro, "Lock Vision Pro"),
    (M::LockProWifi, "Smart Lock Pro Wifi"),
    (M::BlindTilt, "Blind Tilt"),
    (M::Leak, "Water Detector"),
    (M::Relay1, "Relay Switch 1"),
    (M::Relay1Pm, "Relay Switch 1PM"),
    (M::Relay2Pm, "Relay Switch 2PM"),
    (M::Garage, "Garage Door Opener"),
    (M::RollerShade, "Roller Shade"),
    (M::Presence, "Presence Sensor"),
];

impl Model {
    fn device_type(self) -> &'static str {
        TYPES
            .iter()
            .find(|(model, _)| *model == self)
            .map(|(_, name)| *name)
            .expect("every model is in TYPES")
    }

    fn of_type(device_type: &str) -> Option<Model> {
        TYPES
            .iter()
            .find(|(_, name)| *name == device_type)
            .map(|(model, _)| *model)
    }

    /// From the model byte (the service data's first, less its top bit). `Err` for a model
    /// SwitchBot has that isn't read here: it's known, so nothing else is tried.
    fn of_byte(byte: u8) -> Result<Option<Model>, ()> {
        Ok(Some(match byte {
            b'd' | b'D' => M::Contact,
            b'H' => M::Bot,
            b's' | b'S' => M::Motion,
            b'{' | b'[' => M::Curtain3,
            b'c' | b'C' => M::Curtain,
            b'w' | b'W' => M::OutdoorMeter,
            b'i' | b'I' => M::MeterPlus,
            b'T' | b't' => M::Meter,
            b'4' | 0x14 => M::MeterPro,
            b'5' | 0x15 => M::MeterProCo2,
            b'v' | b'V' => M::Hub2,
            b'g' | b'G' => M::PlugUs,
            b'j' | b'J' => M::PlugJp,
            b'?' | 0x1f => M::PlugEu,
            b'e' | b'E' => M::Humidifier,
            b'#' | 0x03 => M::Evaporative,
            b'o' | b'O' => M::Lock,
            b'$' | 0x04 => M::LockPro,
            b'-' | 0x0d => M::LockLite,
            b'x' | b'X' => M::BlindTilt,
            b'&' | 0x06 => M::Leak,
            b';' | 0x1b => M::Relay1,
            b'<' | 0x1c => M::Relay1Pm,
            b'=' | 0x1d => M::Relay2Pm,
            b'>' | 0x1e => M::Garage,
            b',' | 0x0c => M::RollerShade,
            // Known to SwitchBot, not read here: lights, remotes, keypads, fans, vacuums,
            // purifiers, the Hub Mini Matter.
            b'r' | b'R' | b'u' | b'U' | b'q' | b'Q' | b'n' | b'N' | b'y' | b'Y' | b'b' | b'B'
            | 0x07 | b'\'' | b'%' | 0x05 | b'~' | b'^' | b'.' | 0x0f | b'z' | b'Z' | b'3'
            | 0x13 | b'}' | b']' | b'(' | 0x08 | b'*' | 0x0a | b'+' | 0x0b | b'7' | 0x17 | b'8'
            | 0x18 => return Err(()),
            _ => return Ok(None),
        }))
    }

    /// From the product id newer models end their service data with.
    fn of_product(id: &[u8]) -> Option<Model> {
        let [variant, rest @ ..] = id else {
            return None;
        };
        if *variant > 1 {
            return None;
        }
        Some(match rest {
            [0x10, 0xb9, 0x40] => M::Hub3,
            [0x10, 0xa5, 0xb8] => M::LockUltra,
            [0x11, 0x69, 0x09] => M::LockVisionPro,
            [0x11, 0x69, 0x08] => M::LockVision,
            [0x10, 0xff, 0x90] => M::LockProWifi,
            [0x10, 0xcc, 0xc8] => M::Presence,
            _ => return None,
        })
    }
}

/// Which model sent `advert`, the way pySwitchbot decides: the model byte, then what the cloud
/// says the device is, then the length of the Humidifier's advert, then a product id.
fn model(advert: &Advert, hint: Option<&str>) -> Option<Model> {
    let service = advert.service_data();
    if let Some(&first) = service.and_then(|s| s.first()) {
        match Model::of_byte(first & 0x7f) {
            Ok(Some(model)) => return Some(model),
            Err(()) => return None,
            Ok(None) => {}
        }
    }
    if let Some(model) = hint.and_then(Model::of_type) {
        return Some(model);
    }
    if advert
        .manufacturer
        .get(&COMPANY_HUMIDIFIER)
        .is_some_and(|data| data.len() == 6)
    {
        return Some(M::Humidifier);
    }
    let service = service.filter(|s| s.len() > 5)?;
    let n = service.len();
    Model::of_product(&service[n - 4..]).or_else(|| Model::of_product(&service[n - 5..n - 1]))
}

/// Reads `advert`. `hint` is the device's type from the cloud's list, when it's listed there:
/// a device scanned passively sends no service data, and its type has to come from elsewhere.
/// `None` for anything that isn't a SwitchBot device read here, or that says nothing yet.
pub fn parse(advert: &Advert, hint: Option<&str>) -> Option<Heard> {
    let mac = advert.mac()?;
    let model = model(advert, hint)?;
    let svc = advert.service_data().unwrap_or_default();
    let mfr = advert
        .manufacturer_data()
        .map(|(_, data)| data)
        .unwrap_or_default();
    let reading = read(model, svc, mfr)?;
    Some(Heard {
        mac,
        device_type: model.device_type(),
        reading,
        rssi: advert.rssi,
    })
}

fn read(model: Model, svc: &[u8], mfr: &[u8]) -> Option<Reading> {
    let battery = || svc.get(2).map(|b| f64::from(b & 0x7f));
    let mut reading = Reading::default();
    match model {
        M::Meter | M::MeterPlus | M::OutdoorMeter | M::MeterPro | M::MeterProCo2 => {
            let th = mfr
                .get(8..11)
                .filter(|_| mfr.len() >= 11)
                .or_else(|| svc.get(3..6))?;
            let (temperature, humidity) = climate(th)?;
            reading.temperature = Some(temperature);
            reading.humidity = Some(humidity);
            reading.battery = battery();
            if model == M::MeterProCo2
                && let Some(co2) = mfr.get(13..15)
            {
                let co2 = u16::from_be_bytes([co2[0], co2[1]]);
                // The sensor reads 400-9999 ppm; more is a garbled advert.
                if co2 <= 9999 {
                    reading.co2 = Some(f64::from(co2));
                }
            }
        }
        M::Hub2 => {
            let status = *mfr.get(12)?;
            let (temperature, humidity) = climate(mfr.get(13..16)?)?;
            reading.temperature = Some(temperature);
            reading.humidity = Some(humidity);
            reading.light_level = Some(f64::from(status & 0x1f));
        }
        M::Hub3 => {
            if mfr.len() < 17 {
                return None;
            }
            let d = &mfr[6..];
            let th = &d[7..10];
            reading.temperature = Some(temperature(th));
            reading.humidity = Some(f64::from(th[2] & 0x7f));
            reading.light_level = Some(f64::from(d[6] & 0x0f));
            reading.motion = Some(d[10] & 0x80 != 0);
        }
        M::Contact => {
            let full_mfr = mfr.len() >= 13;
            if !full_mfr && svc.len() < 9 {
                return None;
            }
            reading.battery = battery();
            let (motion, open, timeout, light) = if full_mfr {
                let b = mfr[7];
                (b & 0x80 != 0, b & 0x10 != 0, b & 0x20 != 0, b & 0x40 != 0)
            } else {
                let b = svc[3];
                (
                    svc[1] & 0x40 != 0,
                    b & 0x02 != 0,
                    b & 0x04 != 0,
                    b & 0x01 != 0,
                )
            };
            reading.motion = Some(motion);
            // Left open past its timeout is still open.
            reading.contact_open = Some(open || timeout);
            reading.bright = Some(light);
        }
        M::Motion => {
            let mut said = false;
            if svc.len() >= 6 {
                reading.motion = Some(svc[1] & 0x40 != 0);
                reading.battery = battery();
                reading.bright = Some(svc[5] & 0x02 != 0);
                said = true;
            }
            if mfr.len() >= 8 {
                reading.motion = Some(mfr[7] & 0x40 != 0);
                reading.bright = Some(mfr[7] & 0x20 != 0);
                said = true;
            }
            if !said {
                return None;
            }
        }
        M::Leak => {
            if mfr.len() < 9 {
                return None;
            }
            reading.leak = Some(mfr[8] & 0x01 != 0);
            reading.battery = Some(f64::from(mfr[7] & 0x7f));
        }
        M::Presence => {
            if mfr.len() < 12 {
                return None;
            }
            reading.occupied = Some(mfr[7] & 0x40 != 0);
            reading.light_level = Some(f64::from(mfr[11] & 0x1f));
            reading.battery = battery();
        }
        M::Lock | M::LockLite | M::LockVision => {
            if mfr.len() < 9 {
                return None;
            }
            reading.bolt = bolt((mfr[7] & 0x70) >> 4);
            reading.calibrated = Some(mfr[7] & 0x80 != 0);
            reading.battery = battery();
            if model == M::Lock {
                reading.door_open = Some(mfr[7] & 0x04 != 0);
            }
        }
        M::LockPro | M::LockProWifi | M::LockUltra | M::LockVisionPro => {
            if mfr.len() < 12 {
                return None;
            }
            reading.bolt = bolt((mfr[7] & 0x78) >> 3);
            reading.calibrated = Some(mfr[7] & 0x80 != 0);
            reading.door_open = Some(mfr[8] & 0x10 != 0);
            reading.battery = Some(f64::from(mfr[9] & 0x7f));
        }
        M::Bot => {
            if svc.len() < 3 {
                return None;
            }
            let switching = svc[1] & 0x80 != 0;
            reading.bot_mode = Some(if switching { "switchMode" } else { "pressMode" }.into());
            // Only a Bot in switch mode is on or off; in press mode the bit means nothing.
            reading.on = switching.then_some(svc[1] & 0x40 == 0);
            reading.battery = battery();
        }
        M::PlugUs | M::PlugJp => {
            if mfr.len() < 12 {
                return None;
            }
            reading.on = Some(mfr[7] == 0x80);
            let watts = u16::from_be_bytes([mfr[10], mfr[11]]) & 0x7fff;
            reading.power = Some(f64::from(watts) / 10.0);
        }
        // Their advert's power isn't in a unit anyone has pinned down; the cloud gives watts.
        M::Relay1 | M::Relay1Pm | M::PlugEu => {
            reading.on = Some(mfr.get(7)? & 0x80 != 0);
        }
        M::Relay2Pm => {
            let b = *mfr.get(7)?;
            reading.on = Some(b & 0x80 != 0);
            reading.on_2 = Some(b & 0x40 != 0);
        }
        M::Garage => {
            reading.door_open = Some(mfr.get(7)? & 0x20 == 0);
        }
        M::Curtain | M::Curtain3 => {
            let (d, battery) = if mfr.len() >= 13 {
                (&mfr[8..11], Some(mfr[12]))
            } else if mfr.len() >= 11 {
                (&mfr[8..11], svc.get(2).copied())
            } else if svc.len() >= 6 {
                (&svc[3..6], Some(svc[2]))
            } else {
                return None;
            };
            // Counted from open, like the cloud's: 0 is open.
            reading.position = Some(100.0 - f64::from((d[0] & 0x7f).min(100)));
            reading.moving = Some(d[0] & 0x80 != 0);
            reading.battery = battery.map(|b| f64::from(b & 0x7f));
            reading.calibrated = svc.get(1).map(|b| b & 0x40 != 0);
        }
        M::RollerShade => {
            if mfr.len() < 10 {
                return None;
            }
            let d = &mfr[6..];
            reading.position = Some(100.0 - f64::from((d[2] & 0x7f).min(100)));
            reading.calibrated = Some(d[2] & 0x80 != 0);
            reading.moving = Some(d[1] & 0x06 != 0);
            reading.battery = battery();
        }
        M::BlindTilt => {
            if mfr.len() < 10 {
                return None;
            }
            let d = &mfr[6..];
            reading.position = Some(f64::from((d[2] & 0x7f).min(100)));
            reading.moving = Some(d[2] & 0x80 != 0);
            reading.calibrated = Some(d[1] & 0x01 != 0);
            reading.battery = battery();
        }
        M::Humidifier => {
            reading.on = Some(*svc.get(1).filter(|_| svc.len() >= 5)? != 0);
        }
        M::Evaporative => {
            if mfr.len() < 17 {
                return None;
            }
            reading.on = Some(mfr[7] & 0x80 != 0);
            let mode = mfr[7] & 0x0f;
            reading.humidifier_mode = (1..=8).contains(&mode).then_some(mode);
            // The room's humidity, when a meter is paired with it.
            let d = &mfr[9..12];
            let humidity = d[0] & 0x7f;
            if d[0] & 0x80 != 0 && humidity <= 100 {
                reading.humidity = Some(f64::from(humidity));
                let sign = if d[1] & 0x80 != 0 { 1.0 } else { -1.0 };
                reading.temperature = Some(round(
                    sign * (f64::from(d[1] & 0x7f) + f64::from(d[2] >> 4) / 10.0),
                ));
            }
        }
    }
    Some(reading)
}

/// A meter's temperature and humidity from its three bytes: tenths in the low nibble of the
/// first, the sign and whole degrees in the second, humidity in the third. All zero means the
/// sensor hasn't read yet.
fn climate(th: &[u8]) -> Option<(f64, f64)> {
    let temperature = temperature(th);
    let humidity = f64::from(th[2] & 0x7f);
    (temperature != 0.0 || humidity != 0.0).then_some((temperature, humidity))
}

fn temperature(th: &[u8]) -> f64 {
    let sign = if th[1] & 0x80 != 0 { 1.0 } else { -1.0 };
    round(sign * (f64::from(th[1] & 0x7f) + f64::from(th[0] & 0x0f) / 10.0))
}

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// A lock's state, the way Home Assistant reads it: half locked is locked, a stop on the way is
/// jammed, and the latch states of the EU locks are unlocked.
fn bolt(status: u8) -> Option<Bolt> {
    Some(match status {
        0 | 7 => Bolt::Locked,
        2 => Bolt::Locking,
        3 => Bolt::Unlocking,
        4 | 5 => Bolt::Jammed,
        _ => Bolt::Unlocked,
    })
}

#[cfg(test)]
mod tests;
