//! What can be put on the plan: the devices of the home and its helpers, chosen from a list
//! before clicking where they go.
//!
//! A home has dozens of devices, so the list starts short. It shows what belongs on the floor
//! being drawn — devices in one of its rooms, and the ones in no room yet, which could be
//! anywhere — and leaves out what is already on the plan. Both can be widened: a device placed
//! on the wrong floor is moved by finding it under **Placed** and putting it down again here.

use irori_types::{Device, Entity, FloorId, Floorplan, Marked};
use leptos::prelude::*;

use crate::api::Home;

/// One thing the list offers.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Offer {
    pub marked: Marked,
    pub name: String,
    /// Make and model, or what kind of helper it is.
    pub about: String,
    /// The room it's in, if it's in one.
    pub room: Option<String>,
    /// Whether it is in a room on another floor than the one being drawn.
    pub elsewhere: bool,
    /// Whether it is already on the plan, on any floor.
    pub placed: bool,
}

/// Everything that could go on the plan, as the picker says it, sorted by name.
pub(super) fn offers(home: &Home, plan: &Floorplan, floor: Option<&FloorId>) -> Vec<Offer> {
    let placed = |marked: &Marked| {
        plan.floors
            .values()
            .flat_map(|level| &level.devices)
            .any(|placed| !placed.copy && placed.marked().as_ref() == Some(marked))
    };
    let floor_of = |area: Option<&irori_types::AreaId>| {
        let area = home.areas.iter().find(|room| Some(&room.id) == area)?;
        Some((area.name.to_string(), area.floor_id.clone()))
    };
    let devices = home
        .devices
        .iter()
        .filter(|device| device.protocol.as_str() != HELPERS)
        .map(|device| {
            let marked = Marked::Device(device.id.clone());
            let room = floor_of(device.area_id.as_ref());
            Offer {
                placed: placed(&marked),
                marked,
                name: device.name.to_string(),
                about: made_by(device),
                elsewhere: room
                    .as_ref()
                    .and_then(|(_, on)| on.as_ref())
                    .is_some_and(|on| Some(on) != floor),
                room: room.map(|(name, _)| name),
            }
        });
    let helpers = home
        .entities
        .iter()
        .filter(|entity| is_helper(entity))
        .map(|entity| {
            let marked = Marked::Entity(entity.id.clone());
            let room = floor_of(entity.area_id.as_ref());
            Offer {
                placed: placed(&marked),
                marked,
                name: entity.name.to_string(),
                about: crate::devices::helper_kind_of(entity)
                    .map(|(_, _, word, _)| format!("Helper · {word}"))
                    .unwrap_or_else(|| "Helper".into()),
                elsewhere: room
                    .as_ref()
                    .and_then(|(_, on)| on.as_ref())
                    .is_some_and(|on| Some(on) != floor),
                room: room.map(|(name, _)| name),
            }
        });
    let mut offers: Vec<Offer> = devices.chain(helpers).collect();
    offers.sort_by_key(|offer| offer.name.to_lowercase());
    offers
}

/// The protocol that keeps helpers. Its devices, if it ever has any, are its own business.
const HELPERS: &str = "helpers";

/// Whether an entity is a helper itself — not the event a button helper has beside it.
pub(super) fn is_helper(entity: &Entity) -> bool {
    entity.protocol.as_str() == HELPERS && crate::devices::helper_kind_of(entity).is_some()
}

/// Who made it and what it is, as far as it says.
fn made_by(device: &Device) -> String {
    match (device.manufacturer.as_deref(), device.model.as_deref()) {
        (Some(make), Some(model)) if model.starts_with(make) => model.to_owned(),
        (Some(make), Some(model)) => format!("{make} {model}"),
        (Some(one), None) | (None, Some(one)) => one.to_owned(),
        (None, None) => String::new(),
    }
}

/// Whether an offer is shown, given what has been typed and how wide the list is.
pub(super) fn wanted(offer: &Offer, needle: &str, every_floor: bool, with_placed: bool) -> bool {
    if offer.placed && !with_placed {
        return false;
    }
    if offer.elsewhere && !every_floor {
        return false;
    }
    let needle = needle.trim().to_lowercase();
    needle.is_empty()
        || [
            offer.name.as_str(),
            offer.about.as_str(),
            offer.room.as_deref().unwrap_or_default(),
        ]
        .iter()
        .any(|text| text.to_lowercase().contains(&needle))
}

#[component]
pub(super) fn DevicePicker(
    plan: Memo<Floorplan>,
    floor: RwSignal<Option<FloorId>>,
    arming: RwSignal<Option<Marked>>,
    live: crate::Live,
) -> impl IntoView {
    let needle = RwSignal::new(String::new());
    let every_floor = RwSignal::new(false);
    let with_placed = RwSignal::new(false);
    let offered = Memo::new(move |_| {
        let plan = plan.get();
        let floor = floor.get();
        live.home.with(|home| offers(home, &plan, floor.as_ref()))
    });
    view! {
        <div class="device-picker">
            <h2>"Devices and helpers"</h2>
            <input
                type="search"
                class="picker-search"
                placeholder="Find by name, make or room"
                aria-label="Find a device"
                prop:value=move || needle.get()
                on:input=move |event| needle.set(event_target_value(&event))
            />
            <div class="picker-filters">
                {crate::segmented::segmented(
                    "Which floors",
                    vec![(false, "This floor"), (true, "Every floor")],
                    every_floor.into(),
                    move |to| every_floor.set(to),
                )}
                {crate::segmented::segmented(
                    "Which devices",
                    vec![(false, "To place"), (true, "All")],
                    with_placed.into(),
                    move |to| with_placed.set(to),
                )}
            </div>
            <ul>
                {move || {
                    let all = offered.get();
                    if all.is_empty() {
                        return view! { <li class="muted">"No devices yet."</li> }.into_any();
                    }
                    let (needle, every, placed) =
                        (needle.get(), every_floor.get(), with_placed.get());
                    let shown: Vec<Offer> = all
                        .into_iter()
                        .filter(|offer| wanted(offer, &needle, every, placed))
                        .collect();
                    if shown.is_empty() {
                        return view! {
                            <li class="muted small">
                                "Nothing left to place here. Every floor or All shows more."
                            </li>
                        }.into_any();
                    }
                    shown
                        .into_iter()
                        .map(|offer| {
                            let (id, armed) = (offer.marked.clone(), offer.marked.clone());
                            let under = [Some(offer.about.clone()), offer.room.clone()]
                                .into_iter()
                                .flatten()
                                .filter(|part| !part.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · ");
                            view! {
                                <li>
                                    <button
                                        type="button"
                                        class:chosen=move || arming.get().as_ref() == Some(&armed)
                                        on:click=move |_| arming.update(|current| {
                                            *current = if current.as_ref() == Some(&id) {
                                                None
                                            } else {
                                                Some(id.clone())
                                            };
                                        })
                                    >
                                        <span class="offer">
                                            <span class="name">{offer.name}</span>
                                            {(!under.is_empty()).then(|| view! {
                                                <span class="about">{under}</span>
                                            })}
                                        </span>
                                        {offer.placed.then(|| view! {
                                            <span class="badge">"on the plan"</span>
                                        })}
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </ul>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(name: &str, elsewhere: bool, placed: bool) -> Offer {
        Offer {
            marked: Marked::Device(name.parse().expect("a valid device id")),
            name: name.into(),
            about: "Acme Bulb 2".into(),
            room: Some("Kitchen".into()),
            elsewhere,
            placed,
        }
    }

    /// The list starts with what is left to place on this floor, and widens when asked.
    #[test]
    fn the_list_is_this_floor_and_what_is_not_placed_yet() {
        let here = offer("lamp", false, false);
        let upstairs = offer("fan", true, false);
        let done = offer("plug", false, true);
        assert!(wanted(&here, "", false, false));
        assert!(!wanted(&upstairs, "", false, false));
        assert!(wanted(&upstairs, "", true, false));
        assert!(!wanted(&done, "", false, false));
        assert!(wanted(&done, "", false, true));
    }

    /// Typing finds a device by its name, its make or its room.
    #[test]
    fn typing_finds_by_name_make_or_room() {
        let lamp = offer("lamp", false, false);
        for needle in ["LAM", "acme", "kitch", ""] {
            assert!(wanted(&lamp, needle, false, false), "{needle}");
        }
        assert!(!wanted(&lamp, "garage", false, false));
    }

    #[test]
    fn make_and_model_are_said_once() {
        let device = |make: Option<&str>, model: Option<&str>| {
            let mut device: Device = serde_json::from_value(serde_json::json!({
                "id": "lamp",
                "unique_id": "lamp",
                "protocol": "demo",
                "name": "Lamp",
            }))
            .expect("a device");
            device.manufacturer = make.map(Into::into);
            device.model = model.map(Into::into);
            device
        };
        assert_eq!(
            made_by(&device(Some("IKEA"), Some("TRADFRI bulb"))),
            "IKEA TRADFRI bulb"
        );
        assert_eq!(
            made_by(&device(Some("Aqara"), Some("Aqara P1"))),
            "Aqara P1"
        );
        assert_eq!(made_by(&device(None, Some("P1"))), "P1");
        assert_eq!(made_by(&device(None, None)), "");
    }
}
