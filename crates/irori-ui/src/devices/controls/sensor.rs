//! A reading: a sensor's value, or a binary sensor's state in words.

use irori_types::{BinarySensorClass, SensorCapabilities, SensorValue, State};
use leptos::prelude::*;

use super::{UNKNOWN, number};

pub(crate) fn sensor(capabilities: &SensorCapabilities, value: Option<&State>) -> AnyView {
    let (reading, unit, counts) = match value {
        Some(State::Sensor(sensor)) => {
            let unit = capabilities.unit.clone().unwrap_or_default();
            match &sensor.value {
                SensorValue::Number(n) => (number(*n), unit, Some(n.to_string())),
                SensorValue::Text(text) => (text.clone(), unit, None),
            }
        }
        _ => (UNKNOWN.to_owned(), String::new(), None),
    };
    // A number counts to its next value as it changes (count.rs); words just change.
    view! {
        <span class="reading">
            <span class="n" data-n=counts>{reading}</span>
            <span class="unit">{(!unit.is_empty()).then(|| format!(" {unit}"))}</span>
        </span>
    }
    .into_any()
}

pub(crate) fn binary(class: Option<BinarySensorClass>, value: Option<&State>) -> AnyView {
    let on = match value {
        Some(State::BinarySensor(sensor)) => Some(sensor.on),
        _ => None,
    };
    let reading = on.map_or(UNKNOWN, |on| wording(class, on));
    view! { <span class="reading" class:on=on.unwrap_or(false)>{reading}</span> }.into_any()
}

/// What a binary sensor's `true` and `false` mean in words. Without a device class there's
/// nothing better to say than on and off.
pub(crate) fn wording(class: Option<BinarySensorClass>, on: bool) -> &'static str {
    use BinarySensorClass::*;
    // What `on` means follows Home Assistant's classes: a `lock` that's on is unlocked, a
    // `battery` that's on is low.
    let (yes, no) = match class {
        Some(Motion | Vibration) => ("Motion", "Still"),
        Some(Occupancy | Presence) => ("Detected", "Clear"),
        Some(Door | GarageDoor | Window | Opening) => ("Open", "Closed"),
        Some(Moisture) => ("Wet", "Dry"),
        Some(Smoke) => ("Smoke", "Clear"),
        Some(Gas) => ("Gas", "Clear"),
        Some(CarbonMonoxide) => ("Carbon monoxide", "Clear"),
        Some(GlassBreak) => ("Glass broken", "Clear"),
        Some(Sound) => ("Sound", "Quiet"),
        Some(Tamper) => ("Tampered", "Clear"),
        Some(Plug) => ("Plugged in", "Unplugged"),
        Some(Power) => ("Powered", "No power"),
        Some(Connectivity) => ("Connected", "Disconnected"),
        Some(Battery) => ("Low", "OK"),
        Some(BatteryCharging) => ("Charging", "Not charging"),
        Some(Cold) => ("Cold", "Normal"),
        Some(Heat) => ("Hot", "Normal"),
        Some(Light) => ("Light", "Dark"),
        Some(Lock) => ("Unlocked", "Locked"),
        Some(Moving) => ("Moving", "Stopped"),
        Some(Running) => ("Running", "Stopped"),
        Some(Problem) => ("Problem", "OK"),
        Some(Safety) => ("Unsafe", "Safe"),
        Some(Update) => ("Update available", "Up to date"),
        None => ("On", "Off"),
    };
    if on { yes } else { no }
}
