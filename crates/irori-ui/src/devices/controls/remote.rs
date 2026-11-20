//! A remote: what it drives is on or off, and the activity going, which can be chosen.

use irori_types::{Entity, RemoteCapabilities, RemoteState, State};
use leptos::prelude::*;

use super::knob;
use crate::choices::choices;
use crate::devices::Controls;

/// On or off, and the activity when there is one: `On · Watch TV`.
pub(crate) fn remote_words(remote: &RemoteState) -> String {
    match (&remote.activity, remote.on) {
        (Some(activity), true) => format!("On · {activity}"),
        (_, on) => if on { "On" } else { "Off" }.to_owned(),
    }
}

pub(crate) fn remote_control(
    entity: &Entity,
    capabilities: &RemoteCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let remote = match value {
        Some(State::Remote(remote)) => Some(remote.clone()),
        _ => None,
    };
    let on = remote.as_ref().map(|remote| remote.on);
    let activities = (!capabilities.activities.is_empty()).then(|| {
        let entity_id = entity.id.clone();
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        let current = remote.as_ref().and_then(|remote| remote.activity.clone());
        let options = capabilities
            .activities
            .iter()
            .map(|activity| (activity.clone(), activity.clone()))
            .collect();
        choices(
            format!("{} activity", entity.name),
            options,
            move || current.clone(),
            disable,
            move |activity| {
                controls.act.run((
                    entity_id.clone(),
                    "turn_on",
                    Some(serde_json::json!({ "activity": activity })),
                ));
            },
        )
    });
    view! {
        <>
            {activities}
            {knob(entity, on, offline, controls)}
        </>
    }
    .into_any()
}
