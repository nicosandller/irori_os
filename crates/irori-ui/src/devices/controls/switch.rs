//! A switch: on or off.

use irori_types::{Entity, State};
use leptos::prelude::*;

use super::knob;
use crate::devices::Controls;

pub(crate) fn switch(
    entity: &Entity,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let on = match value {
        Some(State::Switch(switch)) => Some(switch.on),
        _ => None,
    };
    view! {
        <>
            {knob(entity, on, offline, controls)}
        </>
    }
    .into_any()
}
