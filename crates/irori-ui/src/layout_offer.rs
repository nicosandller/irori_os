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

/// How many names the prompt spells out before it says how many more there are.
const MOST_NAMED: usize = 20;

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
            !layout.areas.iter().any(|area| {
                area.floor
                    .as_ref()
                    .is_some_and(|on| irori_types::same_name(on, &floor.name))
            })
        })
        .map(|floor| floor.name.to_string());
    let all: Vec<String> = rooms.chain(floors).collect();
    let rest = all.len().saturating_sub(MOST_NAMED);
    let some = all[..all.len() - rest].join(", ");
    if rest == 0 {
        some
    } else {
        format!("{some} and {rest} more")
    }
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
        move |shown: SuggestedLayout| {
            let extension = extension.clone();
            busy.set(true);
            trouble.set(None);
            spawn_local(async move {
                match crate::api::accept_layout(&extension, &shown).await {
                    // Busy until the page has the new home, so the old prompt can't be clicked
                    // again.
                    Ok(()) => crate::reload(live).await,
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
        let shown = layout.clone();
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
                    on:click=move |_| accept(shown.clone())
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
        // A floor its rooms name in another case is still named once.
        let cased = layout(&["Ground"], &[("Kitchen", Some("ground"))]);
        assert_eq!(named(&cased), "Kitchen (ground)");
        // A long list says how many it left out.
        let names: Vec<String> = (0..25).map(|n| format!("Room{n}")).collect();
        let rooms: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
        let long = named(&layout(&[], &rooms));
        assert!(long.starts_with("Room0, Room1,"), "{long}");
        assert!(long.ends_with("Room19 and 5 more"), "{long}");
    }
}
