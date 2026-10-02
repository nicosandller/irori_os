//! Each entity's control on the Devices page and the device page, one module per kind. A kind
//! that's more than a knob has its own; [`control`] hands each entity to its kind's. Covers and
//! valves share [`opening`].

mod button;
mod climate;
mod event;
mod fan;
mod humidifier;
mod light;
mod lock;
mod number;
mod opening;
mod select;
mod sensor;
mod siren;
mod switch;
mod text;
mod water_heater;

pub(crate) use self::climate::climate_words;
pub(crate) use self::fan::fan_words;
pub(crate) use self::humidifier::humidifier_words;
pub(crate) use self::lock::lock_words;
pub(crate) use self::number::unit_of;
pub(crate) use self::opening::opening_words;
pub(crate) use self::sensor::wording;
pub(crate) use self::water_heater::water_heater_words;

use irori_types::{Capabilities, CoverState, Entity, EntityState, State};
use leptos::ev;
use leptos::prelude::*;

use crate::devices::Controls;

pub(super) fn control(
    entity: &Entity,
    state: Option<&EntityState>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let value = state.and_then(|s| s.state.as_ref());
    match &entity.capabilities {
        Capabilities::Light(capabilities) => {
            light::light(entity, capabilities, value, offline, controls)
        }
        Capabilities::Switch(_) => switch::switch(entity, value, offline, controls),
        Capabilities::Sensor(capabilities) => sensor::sensor(capabilities, value),
        Capabilities::BinarySensor(capabilities) => {
            sensor::binary(capabilities.device_class, value)
        }
        Capabilities::Number(capabilities) => {
            number::number_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Select(capabilities) => {
            select::select_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Text(capabilities) => {
            text::text_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Button(capabilities) => {
            button::button_control(entity, capabilities.device_class, offline, controls)
        }
        Capabilities::Event(_) => event::happened(state),
        Capabilities::Cover(capabilities) => {
            let current = match value {
                Some(State::Cover(cover)) => Some(cover),
                _ => None,
            };
            opening::opening_control(
                entity,
                capabilities.opening(),
                current.map(CoverState::opening),
                capabilities
                    .tilt
                    .then(|| current.and_then(|cover| cover.tilt)),
                offline,
                controls,
            )
        }
        Capabilities::Valve(capabilities) => {
            let current = match value {
                Some(State::Valve(valve)) => Some(valve.opening()),
                _ => None,
            };
            opening::opening_control(
                entity,
                capabilities.opening(),
                current,
                None,
                offline,
                controls,
            )
        }
        Capabilities::Lock(capabilities) => {
            lock::lock_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Fan(capabilities) => {
            fan::fan_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Siren(capabilities) => {
            siren::siren_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Climate(capabilities) => {
            climate::climate_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::WaterHeater(capabilities) => {
            water_heater::water_heater_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Humidifier(capabilities) => {
            humidifier::humidifier_control(entity, capabilities, value, offline, controls)
        }
    }
}

/// How far along a slider's track `value` sits, as a whole percentage — where its filled part
/// ends. Any number type a slider holds: a light's level, a wall's thickness, a snap step.
pub(crate) fn fill(value: impl Into<f64>, min: impl Into<f64>, max: impl Into<f64>) -> u16 {
    let (value, min, max) = (value.into(), min.into(), max.into());
    if max <= min {
        return 100;
    }
    ((value.clamp(min, max) - min) * 100.0 / (max - min)).floor() as u16
}

/// A switch showing what the entity is doing, not what was last clicked: it moves when the
/// device reports back. A light that has never reported sits in between, and clicking turns it
/// on.
pub(crate) fn knob(
    entity: &Entity,
    on: Option<bool>,
    offline: bool,
    controls: Controls,
) -> impl IntoView {
    let entity_id = entity.id.clone();
    let busy = {
        let entity_id = entity_id.clone();
        move || controls.busy.get().contains(&entity_id)
    };
    let pending = busy.clone();
    // What the click means is "I want it off", not "flip whatever it is now": the page sends the
    // state the person asked for, so a second click on a stale row can't undo the first.
    let wanted = !on.unwrap_or(false);
    let click = {
        let entity_id = entity_id.clone();
        move |event: ev::MouseEvent| {
            // A swipe already said what it wanted when it let go.
            if !crate::gesture::swallow_click(&event) {
                controls.set_on.run((entity_id.clone(), wanted));
            }
        }
    };
    // Swiped: whichever side the knob was let go on, if that's not where it already is.
    let let_go = {
        let entity_id = entity_id.clone();
        move |event: ev::PointerEvent| {
            if let Some(side) = crate::gesture::knob_up(&event)
                && Some(side) != on
            {
                controls.set_on.run((entity_id.clone(), side));
            }
        }
    };
    let label = format!("Turn {} {}", entity.name, if wanted { "on" } else { "off" });
    // A screen reader is told what the knob shows. `mixed` is how ARIA says "neither", which is
    // what an entity that has never reported is: saying `false` would claim it's off.
    let pressed = match on {
        Some(true) => "true",
        Some(false) => "false",
        None => "mixed",
    };
    view! {
        <button
            type="button"
            class="toggle"
            class:unknown=on.is_none()
            // Waiting on the device, as opposed to unable to reach it: both disable the switch,
            // but only this one is going to change.
            class:pending=pending
            aria-label=label
            aria-pressed=pressed
            disabled=move || offline || busy()
            on:click=click
            on:pointerdown=|event| crate::gesture::knob_down(&event)
            on:pointermove=|event| crate::gesture::knob_move(&event)
            on:pointerup=let_go
            on:pointercancel=|event| {
                crate::gesture::knob_up(&event);
            }
        >
            <span class="knob"></span>
        </button>
    }
}

pub(crate) const UNKNOWN: &str = "unknown";

/// A reading a person can read: whole numbers stay whole, the rest keep up to three decimals
/// without trailing zeros. 22.299999999999997 is 22.3.
pub(crate) fn number(n: f64) -> String {
    if !n.is_finite() {
        return UNKNOWN.to_owned();
    }
    let mut text = format!("{n:.3}");
    if text.contains('.') {
        text.truncate(text.trim_end_matches('0').trim_end_matches('.').len());
    }
    text
}

/// `21.5 °C`, `21 °C`.
pub(crate) fn degrees(celsius: f64) -> String {
    let rounded = (celsius * 10.0).round() / 10.0;
    format!("{rounded} °C")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_readable() {
        assert_eq!(number(22.299999999999997), "22.3");
        assert_eq!(number(21.0), "21");
        assert_eq!(number(-0.5), "-0.5");
        assert_eq!(number(1234.56789), "1234.568");
        assert_eq!(number(f64::NAN), UNKNOWN);
    }

    /// A slider's filled part ends where its thumb is, at either end of any range — and a value
    /// outside the range, or a range with nothing in it, can't push the fill off the track.
    #[test]
    fn a_slider_fills_up_to_its_thumb() {
        assert_eq!(fill(1, 1, 100), 0);
        assert_eq!(fill(100, 1, 100), 100);
        assert_eq!(fill(2700, 2000, 6500), 15);
        assert_eq!(fill(9000, 2000, 6500), 100);
        assert_eq!(fill(0, 2000, 6500), 0);
        assert_eq!(fill(50, 50, 50), 100);
    }
}
