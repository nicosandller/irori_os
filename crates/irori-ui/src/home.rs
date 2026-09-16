//! The Home page: is Irori well, and what is it looking after?
//!
//! Deliberately a summary rather than a dashboard. Dashboards are a Phase 2 idea and belong to
//! extensions (ROADMAP §2); this is the page that answers "is it running, and what does it
//! know about?" at a glance.

use leptos::prelude::*;
use leptos_router::components::A;

use crate::Live;

#[component]
pub fn Home() -> impl IntoView {
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
        <h1>"Your home"</h1>
        <p class="lede">
            "Irori is the thing in the middle: it keeps what your devices are doing, and does "
            "what you ask of them."
        </p>

        <div class="tiles">
            <A href="/devices" attr:class="tile">
                <span class="count">{move || counts().0}</span>
                <span class="label">"devices"</span>
            </A>
            <A href="/devices" attr:class="tile">
                <span class="count">{move || counts().1}</span>
                <span class="label">"entities"</span>
            </A>
            <span class="tile">
                <span class="count">
                    {move || {
                        let (_, _, running, total) = counts();
                        if running == total { running.to_string() }
                        else { format!("{running}/{total}") }
                    }}
                </span>
                <span class="label">"extensions running"</span>
            </span>
        </div>

        <section class="card">
            <h2>"Irori itself"</h2>
            {move || match live.health.get() {
                None => view! { <p class="muted">"Asking…"</p> }.into_any(),
                Some(health) => view! {
                    <dl>
                        <dt>"Version"</dt>
                        <dd>{health.version}</dd>
                        <dt>"Uptime"</dt>
                        <dd>{uptime(health.uptime_ms)}</dd>
                        <dt>"Database"</dt>
                        <dd>
                            {format!("SQLite {} · {}", health.sqlite.version,
                                health.sqlite.journal_mode.to_uppercase())}
                        </dd>
                        <dt>"Built with"</dt>
                        <dd>
                            {if health.features.is_empty() {
                                "nothing optional (barebones)".to_owned()
                            } else {
                                health.features.join(", ")
                            }}
                        </dd>
                    </dl>
                }
                .into_any(),
            }}
        </section>

        <section class="card">
            <h2>"What brings the devices in"</h2>
            <ul class="extensions">
                {move || {
                    live.home
                        .get()
                        .extensions
                        .into_iter()
                        .map(|(id, extension)| {
                            let devices = live
                                .home
                                .get()
                                .devices
                                .iter()
                                .filter(|device| device.integration.as_str() == id.as_str())
                                .count();
                            let trouble = extension.reason.clone();
                            view! {
                                <li>
                                    <span class="name">{extension.name.clone()}</span>
                                    <span class="state" class:ok=extension.state == "running">
                                        {extension.state.clone()}
                                    </span>
                                    <span class="muted">
                                        {format!(
                                            "{devices} device{}",
                                            if devices == 1 { "" } else { "s" },
                                        )}
                                    </span>
                                    {trouble.map(|why| view! { <p class="why">{why}</p> })}
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <p class="muted">
                "Extensions are how Irori talks to anything. "
                <A href="/devices">"Add a device"</A>
                " to see what each one can bring in."
            </p>
        </section>

        <p class="muted small">
            "No automations yet, and no way to sign in: Irori isn't looking after a home on its "
            "own. See ROADMAP.md for what comes next."
        </p>
    }
}

/// Uptime a person can read, to one unit: seconds, then minutes, then hours, then days.
fn uptime(ms: u128) -> String {
    let seconds = ms / 1000;
    let (value, unit) = match seconds {
        0..60 => (seconds, "second"),
        60..3600 => (seconds / 60, "minute"),
        3600..86400 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_as_a_person_would_say_it() {
        assert_eq!(uptime(1), "0 seconds");
        assert_eq!(uptime(1_000), "1 second");
        assert_eq!(uptime(90_000), "1 minute");
        assert_eq!(uptime(3_600_000), "1 hour");
        assert_eq!(uptime(90_000_000), "1 day");
        assert_eq!(uptime(180_000_000), "2 days");
    }
}
