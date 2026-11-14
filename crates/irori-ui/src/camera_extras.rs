//! A camera's own siren and infrared lights, as buttons in its video's bar, so they're at hand
//! beside the picture and in full screen. They stay in the device's list of entities too.
//!
//! The siren is any siren on the camera's device. The infrared lights are the ONVIF
//! Cameras extension's switch for them (its unique id starts `infrared:`): nothing in a switch says
//! what it switches, so they're found by the name that extension gives them.

use std::time::Duration;

use irori_types::{Capabilities, EntityId, State};
use leptos::prelude::*;

use crate::api::Home;
use crate::devices::Controls;
use crate::icons::{Icon, icon};

/// How long a first press of the siren waits for the second that sounds it.
const ARMED_FOR: Duration = Duration::from_secs(3);

/// The camera's siren and infrared switch, when its device has them.
fn companions(home: &Home, camera: &EntityId) -> (Option<EntityId>, Option<EntityId>) {
    let Some(device) = home
        .entities
        .iter()
        .find(|entity| entity.id == *camera)
        .and_then(|entity| entity.device_id.clone())
    else {
        return (None, None);
    };
    let device = &device;
    let mine = || {
        home.entities
            .iter()
            .filter(move |entity| entity.device_id.as_ref() == Some(device))
    };
    let siren = mine()
        .find(|entity| matches!(entity.capabilities, Capabilities::Siren(_)))
        .map(|entity| entity.id.clone());
    let infrared = mine()
        .find(|entity| {
            matches!(entity.capabilities, Capabilities::Switch(_))
                && entity.protocol.as_str() == "onvif"
                && entity.unique_id.as_str().starts_with("infrared:")
        })
        .map(|entity| entity.id.clone());
    (siren, infrared)
}

/// Whether a siren or switch is on, when it's said.
fn is_on(home: &Home, id: &EntityId) -> Option<bool> {
    home.states
        .iter()
        .find(|state| state.entity_id == *id)
        .and_then(|state| match &state.state {
            Some(State::Siren(siren)) => Some(siren.on),
            Some(State::Switch(switch)) => Some(switch.on),
            _ => None,
        })
}

/// The buttons, for the bar of the camera `camera`'s video. Nothing when it has neither.
#[component]
pub fn CameraExtras(camera: EntityId) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    let found = Memo::new(move |_| live.home.with(|home| companions(home, &camera)));
    move || {
        let (siren, infrared) = found.get();
        view! {
            {infrared.map(|id| infrared_button(id, live, controls))}
            {siren.map(|id| siren_button(id, live, controls))}
        }
    }
}

/// Where it is now, or where it was just asked to go, while the camera hasn't said.
fn showing(live: crate::Live, controls: Controls, id: &EntityId) -> bool {
    controls
        .asked
        .with(|asked| asked.get(id).map(|(to, _)| *to))
        .or_else(|| live.home.with(|home| is_on(home, id)))
        .unwrap_or(false)
}

fn infrared_button(id: EntityId, live: crate::Live, controls: Controls) -> impl IntoView {
    let on = {
        let id = id.clone();
        move || showing(live, controls, &id)
    };
    let busy = {
        let id = id.clone();
        move || controls.busy.with(|busy| busy.contains(&id))
    };
    let click = {
        let on = on.clone();
        move |_| controls.set_on.run((id.clone(), !on()))
    };
    view! {
        <button
            type="button"
            class="live-icon live-extra"
            class:on=on.clone()
            class:waiting=busy
            aria-pressed={
                let on = on.clone();
                move || on().to_string()
            }
            aria-label="Infrared lights at night"
            title={
                let on = on.clone();
                move || if on() {
                    "Infrared lights on when it's dark: press to turn them off"
                } else {
                    "Infrared lights off: press to let them come on when it's dark"
                }
            }
            on:click=click
        >
            {icon(Icon::Night)}
        </button>
    }
}

/// The siren sounds on a second press within a few seconds, so a stray tap in full screen
/// doesn't set it off. One press quiets it.
fn siren_button(id: EntityId, live: crate::Live, controls: Controls) -> impl IntoView {
    let armed = RwSignal::new(false);
    let presses = StoredValue::new(0u64);
    let on = {
        let id = id.clone();
        move || showing(live, controls, &id)
    };
    let busy = {
        let id = id.clone();
        move || controls.busy.with(|busy| busy.contains(&id))
    };
    let click = {
        let on = on.clone();
        move |_| {
            if on() {
                armed.set(false);
                controls.set_on.run((id.clone(), false));
            } else if armed.get_untracked() {
                armed.set(false);
                controls.set_on.run((id.clone(), true));
            } else {
                armed.set(true);
                presses.update_value(|n| *n += 1);
                let press = presses.get_value();
                leptos::task::spawn_local(async move {
                    gloo_timers::future::sleep(ARMED_FOR).await;
                    // Only the press that armed it disarms it.
                    if presses.try_get_value() == Some(press) {
                        let _ = armed.try_set(false);
                    }
                });
            }
        }
    };
    let label = {
        let on = on.clone();
        move || {
            if on() {
                "Quiet the siren"
            } else if armed.get() {
                "Press again to sound the siren"
            } else {
                "Siren"
            }
        }
    };
    view! {
        <button
            type="button"
            class="live-icon live-extra live-siren"
            class:on=on.clone()
            class:armed=move || armed.get()
            class:waiting=busy
            aria-pressed={
                let on = on.clone();
                move || on().to_string()
            }
            aria-label=label.clone()
            title=label
            on:click=click
        >
            {icon(Icon::Siren)}
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{
        CameraCapabilities, DeviceId, Entity, Name, ProtocolId, SirenCapabilities,
        SwitchCapabilities, UniqueId,
    };

    fn entity(id: &str, unique: &str, device: &str, capabilities: Capabilities) -> Entity {
        Entity {
            id: EntityId::try_from(id).expect("an id"),
            protocol: ProtocolId::try_from("onvif").expect("a protocol"),
            unique_id: UniqueId::try_from(unique).expect("a unique id"),
            name: Name::try_from("Something").expect("a name"),
            device_id: Some(DeviceId::try_from(device).expect("a device")),
            area_id: None,
            capabilities,
            entity_category: None,
        }
    }

    #[test]
    fn a_camera_finds_its_own_siren_and_infrared_switch_and_nothing_else() {
        let home = Home {
            entities: vec![
                entity(
                    "camera.duo",
                    "camera:a",
                    "onvif_a",
                    Capabilities::Camera(CameraCapabilities::default()),
                ),
                entity(
                    "switch.duo_status",
                    "status_light:a",
                    "onvif_a",
                    Capabilities::Switch(SwitchCapabilities::default()),
                ),
                entity(
                    "switch.duo_infrared",
                    "infrared:a",
                    "onvif_a",
                    Capabilities::Switch(SwitchCapabilities::default()),
                ),
                entity(
                    "siren.duo",
                    "siren:a",
                    "onvif_a",
                    Capabilities::Siren(SirenCapabilities::default()),
                ),
                entity(
                    "siren.other",
                    "siren:b",
                    "onvif_b",
                    Capabilities::Siren(SirenCapabilities::default()),
                ),
                entity(
                    "camera.tapo",
                    "camera:b",
                    "onvif_c",
                    Capabilities::Camera(CameraCapabilities::default()),
                ),
            ],
            ..Home::default()
        };
        let id = |id: &str| EntityId::try_from(id).expect("an id");
        assert_eq!(
            companions(&home, &id("camera.duo")),
            (Some(id("siren.duo")), Some(id("switch.duo_infrared")))
        );
        assert_eq!(companions(&home, &id("camera.tapo")), (None, None));
    }
}
