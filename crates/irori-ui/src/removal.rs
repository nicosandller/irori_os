//! The window that asks before a device is removed, and does it.
//!
//! One window for both places a device can go from: its own page, and a found card for a device
//! that paired but was never added. It asks first because what it says is the part worth
//! reading: what goes, and how to get the device back.
//!
//! A device whose protocol keeps a network of its own (Zigbee) is unpaired from that network
//! too. That asks the device itself to leave, and a battery device is asleep most of the time
//! and doesn't answer. So a refused unpair doesn't close the window: it says what happened and
//! offers to remove the device anyway, which drops it from the network's records without its
//! agreement.

use irori_types::DeviceId;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, RemoveFailed};

#[component]
pub fn RemoveDevice(
    id: DeviceId,
    /// The device's name, for the heading.
    name: String,
    /// What its protocol is called, for saying which network it leaves: "Zigbee".
    network: String,
    /// Whether removing it unpairs it (the extension's `unpairs`).
    unpairs: bool,
    /// Whether it's only found, not in the home: there's nothing of Irori's to delete then, and
    /// the job is called unpairing.
    #[prop(optional)]
    found: bool,
    #[prop(into)] on_close: Callback<()>,
    /// Runs once it's gone.
    #[prop(into)]
    on_done: Callback<()>,
) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let removing = RwSignal::new(false);
    // Why a plain unpair was refused. Set, the window offers to remove it anyway.
    let refused = RwSignal::new(None::<String>);
    let trouble = RwSignal::new(None::<String>);

    // A callback rather than a closure: three buttons ask for it, and a callback is `Copy`.
    let remove = Callback::new(move |force: bool| {
        let id = id.clone();
        removing.set(true);
        trouble.set(None);
        spawn_local(async move {
            match api::remove_device(&id, force).await {
                Ok(()) => {
                    crate::refresh(live);
                    on_done.run(());
                }
                Err(RemoveFailed::Unpair(why)) => {
                    removing.set(false);
                    refused.set(Some(why));
                }
                Err(RemoveFailed::Other(why)) => {
                    removing.set(false);
                    trouble.set(Some(why));
                }
            }
        });
    });

    let verb = if found { "Unpair" } else { "Remove" };
    let doing = if found { "Unpairing…" } else { "Removing…" };
    let title = format!("{verb} {name}?");
    let again = network.clone();

    view! {
        <crate::modal::Modal title=title on_close=move || on_close.run(())>
            <div class="confirm">
                {(!found).then(|| view! {
                    <p>
                        "Irori deletes everything it keeps about it — its name, room, "
                        "entities, spot on the floorplan and history — and nothing can use it "
                        "any more."
                    </p>
                })}
                {if unpairs {
                    view! {
                        <p class=if found { "" } else { "muted small" }>
                            {format!("It also leaves your {network} network. To use it again, \
                                      open the network with ")}
                            <strong>"Permit joining"</strong>
                            " and pair it like a new device."
                        </p>
                    }
                        .into_any()
                } else {
                    view! {
                        <p class="muted small">
                            "The device itself isn't touched. It'll be listed under "
                            <strong>"+ Add device"</strong>
                            " if you want it back."
                        </p>
                    }
                        .into_any()
                }}
                {move || refused.get().map(|why| {
                    let again = again.clone();
                    view! {
                        <div class="confirm-refused" role="alert">
                            <p><strong>"It's still paired. "</strong>{why}</p>
                            <p class="muted small">
                                {format!("Or remove it anyway: it leaves your home, and {again} \
                                          forgets it without the device agreeing, if {again} can \
                                          be reached. The device still thinks it's paired, so it \
                                          may need a factory reset before it joins a network \
                                          again.")}
                            </p>
                        </div>
                    }
                })}
                {move || trouble.get().map(|why| view! { <p class="why" role="alert">{why}</p> })}
                <div class="confirm-actions">
                    <button type="button" on:click=move |_| on_close.run(())>"Cancel"</button>
                    {move || refused.get().is_some().then(|| view! {
                        <button
                            type="button"
                            disabled=move || removing.get()
                            on:click=move |_| remove.run(false)
                        >
                            "Try again"
                        </button>
                    })}
                    <button
                        type="button"
                        class="danger-solid"
                        disabled=move || removing.get()
                        on:click=move |_| remove.run(refused.get_untracked().is_some())
                    >
                        {move || match (removing.get(), refused.get().is_some()) {
                            (true, _) => doing,
                            (false, true) => "Remove anyway",
                            (false, false) => verb,
                        }}
                    </button>
                </div>
            </div>
        </crate::modal::Modal>
    }
}
