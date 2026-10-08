//! The head of Settings: what IroriOS is, and what it's looking after at a glance.
//!
//! This is the wordmark the terminal prints when `irori serve` runs (the full version in
//! `assets/irori-cli-art.txt`), beside what the instance has to show for itself. Deliberately
//! three counts rather than a dashboard: it says what this is and gives a reason to go deeper,
//! not every reading in the home at once. It was a page of its own once, the first one a
//! browser landed on; it sits over the settings table now, where the instance is looked after.

use leptos::prelude::*;
use leptos_router::components::A;

use crate::Live;

/// The wordmark and the counts. `children` are the page's own actions, drawn beside the name.
#[component]
pub fn Hero(children: Children) -> impl IntoView {
    let live = expect_context::<Live>();

    let counts = move || {
        let home = live.home.get();
        let running = home
            .extensions
            .values()
            .filter(|extension| extension.state == "running")
            .count();
        (
            home.devices.len(),
            home.entities.len(),
            running,
            home.extensions.len(),
        )
    };

    view! {
        // Named for the page it heads: its heading is the instance's name, not the page's.
        <section class="start" aria-label="Settings">
            <div class="start-wordmark">
                // The mark, drawn big: the frame is everything Irori keeps around the hearth,
                // and the ember is the hearth itself.
                <span class="start-box" aria-hidden="true">
                    <span class="start-ember"></span>
                </span>
                <span class="start-letters">
                    <h1 class="start-title">"IroriOS"</h1>
                    <span class="start-rule" aria-hidden="true"></span>
                    <span class="start-tag">"the hearth at the center of the home"</span>
                </span>
                <div class="page-actions">{children()}</div>
            </div>

            <div class="tiles">
                <A href="/devices" attr:class="tile" on:click=crate::transition::expand attr:style="--i: 0">
                    <span class="count" data-n=move || counts().0.to_string()>
                        {move || counts().0}
                    </span>
                    <span class="label">"devices"</span>
                </A>
                <A href="/devices" attr:class="tile" on:click=crate::transition::expand attr:style="--i: 1">
                    <span class="count" data-n=move || counts().1.to_string()>
                        {move || counts().1}
                    </span>
                    <span class="label">"entities"</span>
                </A>
                <A href="/extensions" attr:class="tile" on:click=crate::transition::expand attr:style="--i: 2">
                    <span class="count">
                        {move || {
                            let (_, _, running, total) = counts();
                            if running == total { running.to_string() }
                            else { format!("{running}/{total}") }
                        }}
                    </span>
                    <span class="label">"extensions running"</span>
                </A>
            </div>
        </section>
    }
}
