//! A light: on and off, and its brightness and colour when it has them.

use irori_types::{Entity, LightCapabilities, LightState, LightTurnOn, State};
use leptos::prelude::*;

use super::{fill, knob};
use crate::devices::Controls;

/// A light: the current brightness beside the switch, and — while it's on — the sliders that
/// set the level it comes back to, its color temperature or its color. The switch is what the
/// entity is doing and the sliders are what a person is asking for, which is why the position a
/// slider shows can sit between sends: the row follows the device once it reports back (M1.5's
/// push will make that instant).
pub(crate) fn light(
    entity: &Entity,
    capabilities: &LightCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let on = match value {
        Some(State::Light(light)) => Some(light.on),
        _ => None,
    };
    // Brightness and color are shown and changeable only while the light is on. A change here is
    // `light.turn_on` with a level, which comes on (the demo's `apply_light` turns a lit light
    // on at the level it asked for); a slider that switched the light on from a standing start
    // would be a light that seemed off before it was touched.
    let current = match value {
        Some(State::Light(light)) if light.on => Some(light),
        _ => None,
    };
    view! {
        <>
            {current.and_then(|l| l.brightness).map(reading)}
            {knob(entity, on, offline, controls)}
            {light_controls(entity, capabilities, current, offline, controls)}
        </>
    }
    .into_any()
}

/// The level a light is at, as a percentage the page shows and its sliders work in. `div_ceil`
/// keeps whole levels honest: 180 of 255 is 71%, not 70.
pub(crate) fn brightness_pct(level: u8) -> u16 {
    (u16::from(level) * 100).div_ceil(255)
}

/// The slider's percentage back to the 1-255 the `light.turn_on` data takes. Flooring keeps the
/// round trip stable: a level the page shows as 71% sends back a level the page will also read
/// as 71%, so the slider doesn't creep one notch per change.
pub(crate) fn pct_to_brightness(pct: u16) -> u8 {
    ((u32::from(pct.clamp(1, 100)) * 255 / 100) as u8).clamp(1, 255)
}

/// The color an `<input type="color">` gives, "#rrggbb", as the `[r, g, b]` the data takes.
pub(crate) fn rgb_from_hex(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let read = |from: usize| u8::from_str_radix(&hex[from..from + 2], 16).ok();
    Some([read(0)?, read(2)?, read(4)?])
}

/// `[r, g, b]` back into the "#rrggbb" a color input wants.
pub(crate) fn hex_from_rgb(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// The sliders that set a lit light's level and color: brightness when it can dim, color
/// temperature within its range, and a color well when it has one. Each sends one `turn_on` with
/// that one thing, keeping the level and color it already has.
pub(crate) fn light_controls(
    entity: &Entity,
    capabilities: &LightCapabilities,
    light: Option<&LightState>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let entity_id = entity.id.clone();
    let Some(light) = light else {
        return ().into_any();
    };

    let mut sub = Vec::new();
    if capabilities.brightness {
        let level = brightness_pct(light.brightness.unwrap_or(255));
        let id = entity_id.clone();
        let run = {
            let id = id.clone();
            move |pct| {
                controls.set_light.run((
                    id.clone(),
                    LightTurnOn {
                        brightness: Some(pct_to_brightness(pct)),
                        ..Default::default()
                    },
                ))
            }
        };
        // A command is one thing at a time per entity: while one is in flight the sliders stand
        // still, so dragging can't pile up commands the device will chew through in its own order.
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        // Where the thumb is while it's being dragged: the label and the filled part of the track
        // follow it, and only letting go sends anything. Starts at what the device reported, and
        // does again whenever the device reports.
        let dragged = RwSignal::new(level);
        sub.push(
            view! {
                <label class="dim" title="Brightness">
                    <span class="lv">{move || dragged.get()}%</span>
                    <input
                        type="range"
                        min="1"
                        max="100"
                        step="1"
                        aria-label={format!("Brightness for {}", entity.name)}
                        prop:value=level.to_string()
                        style:--fill=move || format!("{}%", fill(dragged.get(), 1, 100))
                        disabled=disable
                        on:input:target=move |ev| {
                            if let Ok(pct) = ev.target().value().parse::<u16>() {
                                dragged.set(pct);
                            }
                        }
                        on:change:target=move |ev| {
                            let pct = ev.target().value().parse::<u16>().unwrap_or(level);
                            run(pct)
                        }
                    />
                </label>
            }
            .into_any(),
        );
    }
    if let Some(range) = capabilities.color_temp_kelvin {
        let current = light
            .color_temp_kelvin
            .unwrap_or_else(|| range.min + (range.max - range.min) / 2);
        let id = entity_id.clone();
        let run = {
            let id = id.clone();
            move |kelvin| {
                controls.set_light.run((
                    id.clone(),
                    LightTurnOn {
                        color_temp_kelvin: Some(kelvin),
                        ..Default::default()
                    },
                ))
            }
        };
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        let dragged = RwSignal::new(current);
        sub.push(
            view! {
                <label class="dim" title="Color temperature">
                    <span class="lv">{move || dragged.get()}K</span>
                    // No fill here: the track is the colors themselves, warm to cool.
                    <input
                        type="range"
                        class="kelvin"
                        min=range.min.to_string()
                        max=range.max.to_string()
                        step="100"
                        aria-label={format!("Color temperature for {}", entity.name)}
                        prop:value=current.to_string()
                        disabled=disable
                        on:input:target=move |ev| {
                            if let Ok(kelvin) = ev.target().value().parse::<u16>() {
                                dragged.set(kelvin);
                            }
                        }
                        on:change:target=move |ev| {
                            let kelvin = ev.target().value().parse::<u16>().unwrap_or(current);
                            run(kelvin)
                        }
                    />
                </label>
            }
            .into_any(),
        );
    }
    if capabilities.rgb {
        let current = hex_from_rgb(light.rgb.unwrap_or([255, 255, 255]));
        let id = entity_id.clone();
        let run = {
            let id = id.clone();
            move |rgb| {
                controls.set_light.run((
                    id.clone(),
                    LightTurnOn {
                        rgb: Some(rgb),
                        ..Default::default()
                    },
                ))
            }
        };
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        sub.push(
            view! {
                <label class="dim" title="Color">
                    <input
                        type="color"
                        aria-label={format!("Color for {}", entity.name)}
                        prop:value=current.clone()
                        disabled=disable
                        on:change:target=move |ev| {
                            if let Some(rgb) = rgb_from_hex(&ev.target().value()) {
                                run(rgb)
                            }
                        }
                    />
                </label>
            }
            .into_any(),
        );
    }
    if sub.is_empty() {
        return ().into_any();
    }

    view! {
        <span class="light-controls">
            {sub}
        </span>
    }
    .into_any()
}

pub(crate) fn reading(level: u8) -> impl IntoView {
    view! {
        <span class="reading">
            <span class="n" data-n=brightness_pct(level).to_string()>
                {brightness_pct(level).to_string()}
            </span>
            "%"
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row shows a light's level as a percentage, and the slider turns that percentage back
    /// into a level. The two must agree in both directions, or dimming would read one thing and
    /// set another.
    #[test]
    fn a_light_round_trips_between_level_and_percentage() {
        assert_eq!(brightness_pct(0), 0);
        assert_eq!(brightness_pct(1), 1);
        assert_eq!(brightness_pct(180), 71);
        assert_eq!(brightness_pct(255), 100);
        assert_eq!(pct_to_brightness(1), 2);
        assert_eq!(pct_to_brightness(50), 127);
        assert_eq!(pct_to_brightness(71), 181);
        assert_eq!(pct_to_brightness(100), 255);
        assert_eq!(pct_to_brightness(u16::MAX), 255, "a level can't exceed 255");
        for pct in 1..=100 {
            assert_eq!(
                brightness_pct(pct_to_brightness(pct)),
                pct,
                "level for {pct}% must read back as {pct}%"
            );
        }
    }

    #[test]
    fn colors_round_trip_between_the_page_and_the_light() {
        assert_eq!(hex_from_rgb([255, 0, 128]), "#ff0080");
        assert_eq!(hex_from_rgb([0, 0, 0]), "#000000");
        assert_eq!(rgb_from_hex("#ff0080"), Some([255, 0, 128]));
        assert_eq!(rgb_from_hex("ff0080"), Some([255, 0, 128]));
        assert_eq!(
            rgb_from_hex("FF00FF"),
            Some([255, 0, 255]),
            "upper case is a color too"
        );
        assert_eq!(
            rgb_from_hex("#fff"),
            None,
            "short form isn't what the input gives"
        );
        assert_eq!(rgb_from_hex("#ff00"), None);
        assert_eq!(rgb_from_hex("#gg0000"), None);
        assert_eq!(rgb_from_hex(""), None);
        for rgb in [[255, 255, 255], [12, 34, 56]] {
            assert_eq!(rgb_from_hex(&hex_from_rgb(rgb)), Some(rgb));
        }
    }
}
