//! The start screen: what IroriOS is, and what it's looking after at a glance.
//!
//! This is the wordmark the terminal prints when `irori serve` runs (the full version in
//! `assets/irori-cli-art.txt`), beside what the instance has to show for itself. Deliberately
//! three counts rather than a dashboard: a browser lands here first, so it says what this is and
//! gives a reason to go deeper — not every reading in the home at once.

use leptos::prelude::*;
use leptos_router::components::A;

use crate::Live;

#[component]
pub fn Start() -> impl IntoView {
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
        <section class="start">
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
            </div>

            <div class="tiles">
                <A href="/devices" attr:class="tile">
                    <span class="count">{move || counts().0}</span>
                    <span class="label">"devices"</span>
                </A>
                <A href="/devices" attr:class="tile">
                    <span class="count">{move || counts().1}</span>
                    <span class="label">"entities"</span>
                </A>
                <A href="/extensions" attr:class="tile">
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

            <p class="muted small">
                "IroriOS keeps what your devices are doing and does what you ask of them — the "
                "thing in the middle of the home. No automations yet, and no way to sign in; "
                "see ROADMAP.md for what comes next."
            </p>
        </section>
    }
}
