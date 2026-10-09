//! Where the home is and its time zone: the Settings row, and the same picker in the welcome.
//!
//! The time zone is the part that is needed: it is what "07:00" in an automation is read in,
//! so it comes first and starts from this browser's own. Where the home is on the map is
//! optional, and only the sun needs it; putting a pin down also offers the zone it stands in.
//!
//! The map and the two lookups (an address, a point's zone) need the internet and are extras.
//! The coordinates and the zone list don't, and are the whole of it offline.

use irori_types::{HomeSettings, Location, TimeZoneName};
use irori_ui_kit::combo::{Choice, Combo};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use web_sys::js_sys;
use web_sys::wasm_bindgen::{JsCast as _, JsValue};

use crate::api;
use crate::map::{self, Map, View};

/// How long the pin has to rest before its time zone is looked up: a drag passes over a lot
/// of places nobody lives in.
const SETTLE_MS: u32 = 700;

/// Where the home is, as last saved. `None` until the first answer.
#[derive(Debug, Clone, Copy)]
pub struct Place(pub RwSignal<Option<HomeSettings>>);

/// The zone this browser's clock is set to, which is nearly always the home's.
pub fn browser_zone() -> Option<String> {
    let format = js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
    js_sys::Reflect::get(&format.resolved_options(), &JsValue::from_str("timeZone"))
        .ok()?
        .as_string()
}

/// Every zone the browser knows. Empty in a browser too old to say, where the field still
/// takes a zone typed out.
pub fn zones() -> Vec<String> {
    let listed = || -> Option<Vec<String>> {
        let intl = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("Intl")).ok()?;
        let supported: js_sys::Function =
            js_sys::Reflect::get(&intl, &JsValue::from_str("supportedValuesOf"))
                .ok()?
                .dyn_into()
                .ok()?;
        let zones: js_sys::Array = supported
            .call1(&intl, &JsValue::from_str("timeZone"))
            .ok()?
            .dyn_into()
            .ok()?;
        Some(zones.iter().filter_map(|zone| zone.as_string()).collect())
    };
    listed().unwrap_or_default()
}

/// A zone as a person reads it: "Europe/Brussels" is "Brussels", in "Europe".
pub fn zone_words(zone: &str) -> (String, String) {
    match zone.rsplit_once('/') {
        Some((region, city)) => (city.replace('_', " "), region.replace('_', " ")),
        None => (zone.replace('_', " "), String::new()),
    }
}

/// How the home's place stands, for its row while it's folded.
pub fn summary(home: &HomeSettings) -> String {
    let zone = home.time_zone.as_ref().map(TimeZoneName::as_str);
    let at = home.location.as_ref().map(|at| match &at.label {
        // "Brussels, Brussels-Capital, Belgium" is a lot for a row: the first part names it.
        Some(label) => label.split(',').next().unwrap_or(label).trim().to_owned(),
        None => format!("{:.2}, {:.2}", at.latitude, at.longitude),
    });
    match (at, zone) {
        (Some(at), Some(zone)) => format!("{at} · {zone}"),
        (Some(at), None) => format!("{at} · no time zone"),
        // The zone alone is a whole answer: the map is optional.
        (None, Some(zone)) => zone.to_owned(),
        (None, None) => "not set".to_owned(),
    }
}

/// What was typed as a coordinate, if it is one.
pub fn coordinate(typed: &str, limit: f64) -> Option<f64> {
    let value: f64 = typed.trim().replace(',', ".").parse().ok()?;
    (value.is_finite() && value.abs() <= limit).then_some(value)
}

/// The map, the address search, the coordinates and the time zone, over one draft. Nothing is
/// saved here: whoever holds `draft` decides when.
#[component]
pub fn PlacePicker(
    draft: RwSignal<HomeSettings>,
    /// Whether this is somebody who may change it.
    #[prop(default = true)]
    editable: bool,
) -> impl IntoView {
    let start = draft.get_untracked();
    let pin = RwSignal::new(
        start
            .location
            .as_ref()
            .map(|at| (at.latitude, at.longitude)),
    );
    let fly = RwSignal::new(None::<View>);
    // Whether the zone was picked from the list, which a pin's suggestion mustn't undo.
    let by_hand = RwSignal::new(start.time_zone.is_some());
    // Where the zone came from, said under the field.
    let zone_note = RwSignal::new(None::<&'static str>);
    let query = RwSignal::new(String::new());
    let found = RwSignal::new(Vec::<api::Found>::new());
    let searching = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    // A label belongs to the place that was searched for; moving the pin by hand leaves it.
    let label = RwSignal::new(start.location.as_ref().and_then(|at| at.label.clone()));

    // A home with no zone yet starts from this browser's, which is right far more often than
    // it's wrong, and is one press of Save away from being the home's.
    if editable
        && start.time_zone.is_none()
        && let Some(zone) = browser_zone().and_then(|zone| TimeZoneName::try_from(zone).ok())
    {
        draft.update(|draft| draft.time_zone = Some(zone));
        zone_note.set(Some("from this browser's clock"));
    }

    // The pin is the location. Each move of it also counts down to a zone lookup.
    let moves = RwSignal::new(0u32);
    Effect::new(move |before: Option<Option<(f64, f64)>>| {
        let now = pin.get();
        // The first look is what was already saved: nothing has moved.
        let Some(before) = before else {
            return now;
        };
        if before == now {
            return now;
        }
        draft.update(|draft| {
            draft.location = now.and_then(|(latitude, longitude)| {
                // What a search calls a place can run long; a label is a few words.
                let label = label
                    .get_untracked()
                    .map(|label| label.chars().take(120).collect::<String>());
                Location::new(latitude, longitude, label).ok()
            });
        });
        let Some((latitude, longitude)) = now else {
            return now;
        };
        moves.update(|moves| *moves += 1);
        let this = moves.get_untracked();
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(SETTLE_MS).await;
            // Moved again since, or picked by hand: this answer is for somewhere else.
            if moves.try_get_untracked() != Some(this) || by_hand.get_untracked() {
                return;
            }
            let Some(zone) = api::zone_at(latitude, longitude).await else {
                return;
            };
            if moves.try_get_untracked() != Some(this) || by_hand.get_untracked() {
                return;
            }
            if let Ok(zone) = TimeZoneName::try_from(zone) {
                draft.update(|draft| draft.time_zone = Some(zone));
                zone_note.set(Some("from where the pin is"));
            }
        });
        now
    });

    let search = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let asked = query.get_untracked().trim().to_owned();
        if asked.is_empty() || searching.get_untracked() {
            return;
        }
        searching.set(true);
        trouble.set(None);
        spawn_local(async move {
            match api::find_address(&asked).await {
                Ok(places) => {
                    if places.is_empty() {
                        trouble.set(Some(format!(
                            "Nothing was found for \"{asked}\". Try a town, or drop the pin \
                             by hand."
                        )));
                    }
                    found.set(places);
                }
                Err(why) => trouble.set(Some(why)),
            }
            searching.set(false);
        });
    };
    let go = move |place: api::Found| {
        label.set(Some(place.label.clone()));
        // A place that was searched for is somewhere new: its zone is asked for again.
        by_hand.set(false);
        pin.set(Some((place.latitude, place.longitude)));
        fly.set(Some(View::over(
            place.latitude,
            place.longitude,
            map::FOUND_ZOOM,
        )));
        found.set(Vec::new());
    };

    // The coordinates, typed. Both have to read as numbers before the pin moves.
    let typed = move |latitude: Option<String>, longitude: Option<String>| {
        let here = pin.get_untracked();
        let latitude = match latitude {
            Some(typed) => coordinate(&typed, 90.0),
            None => here.map(|(latitude, _)| latitude),
        };
        let longitude = match longitude {
            Some(typed) => coordinate(&typed, 180.0),
            None => here.map(|(_, longitude)| longitude),
        };
        match (latitude, longitude) {
            (Some(latitude), Some(longitude)) => {
                trouble.set(None);
                label.set(None);
                by_hand.set(false);
                pin.set(Some((latitude, longitude)));
                fly.set(Some(View::over(latitude, longitude, 13.0)));
            }
            _ => trouble.set(Some(
                "A latitude goes from -90 to 90 and a longitude from -180 to 180.".to_owned(),
            )),
        }
    };

    let every_zone = StoredValue::new(zones());
    let zone_choices = Signal::derive(move || {
        let current = draft.with(|draft| draft.time_zone.as_ref().map(ToString::to_string));
        let mut choices: Vec<Choice> = every_zone.with_value(|zones| {
            zones
                .iter()
                .map(|zone| {
                    let (city, region) = zone_words(zone);
                    Choice::new(zone.clone(), format!("{city}, {region}")).detail(zone.clone())
                })
                .collect()
        });
        // The saved zone is always in the list, whatever this browser knows.
        if let Some(current) = current
            && !choices.iter().any(|choice| choice.value == current)
        {
            let (city, region) = zone_words(&current);
            choices.insert(0, Choice::new(current, format!("{city}, {region}")));
        }
        choices
    });
    let zone_value = Signal::derive(move || {
        draft
            .with(|draft| draft.time_zone.as_ref().map(ToString::to_string))
            .unwrap_or_default()
    });
    let pick_zone = Callback::new(move |zone: String| match TimeZoneName::try_from(zone) {
        Ok(zone) => {
            by_hand.set(true);
            zone_note.set(None);
            draft.update(|draft| draft.time_zone = Some(zone));
        }
        Err(why) => trouble.set(Some(why.to_string())),
    });

    let shown = move |pick: fn((f64, f64)) -> f64| {
        move || {
            pin.get()
                .map(|at| format!("{:.5}", pick(at)))
                .unwrap_or_default()
        }
    };

    view! {
        <div class="place">
            // The part that is needed: what a time of day is read in.
                <div class="settings-field place-zone">
                    <span>"Time zone"</span>
                    {if editable {
                        view! {
                            <Combo
                                choices=zone_choices
                                value=zone_value
                                pick=pick_zone
                                placeholder="Search for a city".to_owned()
                            />
                        }
                        .into_any()
                    } else {
                        view! { <span class="place-zone-read">{move || zone_value.get()}</span> }
                            .into_any()
                    }}
                    <span class="muted small place-zone-note">
                        {move || zone_note.get().unwrap_or("what a time of day is read in")}
                    </span>
                </div>

            <div class="place-where">
                <span class="place-where-title">"Where the home is"</span>
                <span class="muted small">
                    "Optional. Only sunrise and sunset need it."
                </span>
            </div>
            {editable.then(|| view! {
                <form class="place-search" on:submit=search>
                    <input
                        type="search"
                        placeholder="Search for an address or a town"
                        aria-label="Search for an address"
                        prop:value=move || query.get()
                        on:input=move |event| query.set(event_target_value(&event))
                    />
                    <button type="submit" class="press" disabled=move || searching.get()>
                        {move || if searching.get() { "Searching…" } else { "Search" }}
                    </button>
                </form>
            })}
            // Always drawn, so it can fold away as well as open: what a search found.
            <div class="drawer" class:open=move || !found.with(Vec::is_empty)>
                <div class="drawer-inner">
                    <ul class="place-found">
                        <For each=move || found.get() key=|place| place.label.clone() let:place>
                            {
                                let picked = place.clone();
                                view! {
                                    <li>
                                        <button type="button" on:click=move |_| go(picked.clone())>
                                            {place.label.clone()}
                                        </button>
                                    </li>
                                }
                            }
                        </For>
                    </ul>
                </div>
            </div>

            <Map pin=pin fly=fly editable=editable />

            <p class="muted small place-hint">
                {if editable {
                    "Click the map to put the pin down, or drag the pin to the exact spot. \
                     Only the coordinates are kept, and only in this home's own files."
                } else {
                    "Where this home is. Only an owner can move it."
                }}
            </p>


            <div class="place-fields">
                <label class="settings-field">
                    "Latitude"
                    <input
                        type="text"
                        inputmode="decimal"
                        placeholder="50.84670"
                        disabled=!editable
                        prop:value=shown(|at| at.0)
                        on:change=move |event| typed(Some(event_target_value(&event)), None)
                    />
                </label>
                <label class="settings-field">
                    "Longitude"
                    <input
                        type="text"
                        inputmode="decimal"
                        placeholder="4.35250"
                        disabled=!editable
                        prop:value=shown(|at| at.1)
                        on:change=move |event| typed(None, Some(event_target_value(&event)))
                    />
                </label>
                {editable.then(|| view! {
                    // Always there, so it can fade: taking the pin off the map again.
                    <button
                        type="button"
                        class="press place-clear"
                        disabled=move || pin.with(Option::is_none)
                        on:click=move |_| {
                            label.set(None);
                            pin.set(None);
                        }
                    >
                        "Remove the location"
                    </button>
                })}
            </div>

            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
}

/// The Settings row's contents: the picker, and saving what it holds.
#[component]
pub fn Section() -> impl IntoView {
    let Place(saved) = expect_context::<Place>();
    let crate::Session(session) = expect_context::<crate::Session>();
    let editable = move || session.with(|session| session.as_ref().is_none_or(|s| s.owner));
    let draft = RwSignal::new(saved.get_untracked().unwrap_or_default());
    let ready = Memo::new(move |_| saved.with(Option::is_some));
    let saving = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    // The check that draws itself once a save has landed.
    let done = RwSignal::new(false);

    // What's unsaved: the draft against what the home holds. A zone only offered from the
    // browser counts, which is what makes Save light up for a home with nothing set.
    let changed = move || saved.with(|saved| saved.as_ref() != Some(&draft.get()));

    let save = move |_| {
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        trouble.set(None);
        let home = draft.get_untracked();
        spawn_local(async move {
            match api::save_place(&home).await {
                Ok(home) => {
                    saved.set(Some(home));
                    done.set(true);
                    gloo_timers::future::TimeoutFuture::new(1600).await;
                    let _ = done.try_set(false);
                }
                Err(why) => trouble.set(Some(why)),
            }
            let _ = saving.try_set(false);
        });
    };

    view! {
        // Built once the saved place is known, so the map opens on it and not on the world;
        // and only once, so saving doesn't take the map away and put it back.
        {move || ready.get().then(|| {
            if let Some(home) = saved.get_untracked() {
                draft.set(home);
            }
            view! { <PlacePicker draft=draft editable=editable() /> }
        })}
        {move || editable().then(|| view! {
            <div class="place-actions">
                <span class="muted small">
                    {move || {
                        if done.get() {
                            "Saved. Automations that fire by the clock or the sun follow it."
                        } else if changed() {
                            "Not saved yet."
                        } else {
                            ""
                        }
                    }}
                </span>
                <button
                    type="button"
                    class="add-one"
                    class:busy=move || saving.get()
                    class:done=move || done.get()
                    // The zone is the part that's needed: without one there is nothing to save.
                    disabled=move || {
                        saving.get() || !changed() || draft.with(|draft| draft.time_zone.is_none())
                    }
                    on:click=save
                >
                    <span class="add-one-label">"Save"</span>
                    <svg class="add-one-check" viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M5 12.5 10 17.5 19 7" pathLength="1" fill="none"
                            stroke="currentColor" stroke-width="2.4" stroke-linecap="round"
                            stroke-linejoin="round" />
                    </svg>
                </button>
            </div>
        })}
        {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(zone: Option<&str>, at: Option<(f64, f64, Option<&str>)>) -> HomeSettings {
        HomeSettings {
            time_zone: zone.map(|zone| TimeZoneName::try_from(zone).expect("a zone")),
            location: at.map(|(latitude, longitude, label)| {
                Location::new(latitude, longitude, label.map(str::to_owned)).expect("a place")
            }),
        }
    }

    #[test]
    fn the_row_says_where_the_home_is_in_a_few_words() {
        assert_eq!(summary(&home(None, None)), "not set");
        assert_eq!(
            summary(&home(Some("Europe/Brussels"), None)),
            "Europe/Brussels"
        );
        assert_eq!(
            summary(&home(
                Some("Europe/Brussels"),
                Some((50.8467, 4.3525, Some("Brussels, Brussels-Capital, Belgium")))
            )),
            "Brussels · Europe/Brussels"
        );
        assert_eq!(
            summary(&home(None, Some((50.8467, 4.3525, None)))),
            "50.85, 4.35 · no time zone"
        );
    }

    #[test]
    fn a_zone_reads_as_a_city_in_a_region() {
        assert_eq!(
            zone_words("America/Argentina/Buenos_Aires"),
            ("Buenos Aires".to_owned(), "America/Argentina".to_owned())
        );
        assert_eq!(zone_words("UTC"), ("UTC".to_owned(), String::new()));
    }

    #[test]
    fn a_coordinate_is_a_number_inside_its_range() {
        assert_eq!(coordinate(" 50.8467 ", 90.0), Some(50.8467));
        // A comma for the decimal point, as half the world writes it.
        assert_eq!(coordinate("4,35", 180.0), Some(4.35));
        assert_eq!(coordinate("-91", 90.0), None);
        assert_eq!(coordinate("north", 90.0), None);
        assert_eq!(coordinate("", 90.0), None);
    }
}
