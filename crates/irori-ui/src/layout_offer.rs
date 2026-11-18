//! The floors and rooms an extension proposes (`docs/specs/protocols.md` §6.9): a bridge to
//! another home system knows how that home is divided, and the owner can bring it over in one
//! step. Shown on the extension's "+ Add device" step, above what it found, so the rooms exist
//! before its devices are added and each lands in its own. Draws nothing while there's nothing
//! to bring, which is every extension that doesn't propose a layout.

use irori_types::{ExtensionId, SuggestedLayout};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// "3 rooms and 2 floors", "1 room", "2 floors".
pub fn counted(layout: &SuggestedLayout) -> String {
    let some = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    [
        some(layout.areas.len(), "room", "rooms"),
        some(layout.floors.len(), "floor", "floors"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" and ")
}

/// What it would make, named: rooms first, each with its floor.
fn named(layout: &SuggestedLayout) -> String {
    let rooms = layout.areas.iter().map(|area| match &area.floor {
        Some(floor) => format!("{} ({floor})", area.name),
        None => area.name.to_string(),
    });
    let floors = layout
        .floors
        .iter()
        .filter(|floor| {
            !layout
                .areas
                .iter()
                .any(|area| area.floor.as_ref() == Some(&floor.name))
        })
        .map(|floor| floor.name.to_string());
    rooms.chain(floors).collect::<Vec<_>>().join(", ")
}

#[component]
pub fn LayoutOffer(extension: ExtensionId) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let proposal = {
        let extension = extension.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.extensions.get(&extension).and_then(|e| {
                    let layout = e.suggested_layout.clone()?;
                    (!layout.is_empty()).then(|| (e.name.clone(), layout))
                })
            })
        })
    };
    let busy = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    let accept = {
        let extension = extension.clone();
        move |_| {
            let extension = extension.clone();
            busy.set(true);
            trouble.set(None);
            spawn_local(async move {
                match crate::api::accept_layout(&extension).await {
                    Ok(()) => crate::refresh(live),
                    Err(why) => trouble.set(Some(format!("Couldn't bring them over: {why}"))),
                }
                busy.set(false);
            });
        }
    };

    move || {
        let Some((name, layout)) = proposal.get() else {
            return ().into_any();
        };
        let accept = accept.clone();
        view! {
            <div class="layout-offer">
                <div class="layout-offer-text">
                    <p>
                        <strong>{format!("Bring over {name}'s {}", counted(&layout))}</strong>
                    </p>
                    <p class="muted small">{named(&layout)}</p>
                    <p class="muted small">
                        "Rooms you already have are left as they are. Devices it says are in \
                         one of these rooms go into it."
                    </p>
                    {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                </div>
                <button
                    type="button"
                    class="primary"
                    disabled=move || busy.get()
                    on:click=accept
                >
                    "Bring them over"
                </button>
            </div>
        }
        .into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(floors: &[&str], areas: &[(&str, Option<&str>)]) -> SuggestedLayout {
        SuggestedLayout {
            floors: floors
                .iter()
                .map(|name| irori_types::SuggestedFloor {
                    name: name.parse().expect("a name"),
                    level: None,
                })
                .collect(),
            areas: areas
                .iter()
                .map(|(name, floor)| irori_types::SuggestedArea {
                    name: name.parse().expect("a name"),
                    floor: floor.map(|f| f.parse().expect("a name")),
                })
                .collect(),
        }
    }

    #[test]
    fn it_says_how_many_of_each() {
        let both = layout(
            &["Ground", "Upstairs"],
            &[
                ("Kitchen", Some("Ground")),
                ("Hall", None),
                ("Office", None),
            ],
        );
        assert_eq!(counted(&both), "3 rooms and 2 floors");
        assert_eq!(counted(&layout(&[], &[("Hall", None)])), "1 room");
        assert_eq!(counted(&layout(&["Attic", "Cellar"], &[])), "2 floors");
        assert_eq!(named(&both), "Kitchen (Ground), Hall, Office, Upstairs");
    }
}
