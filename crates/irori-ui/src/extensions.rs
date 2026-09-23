//! The Extensions page: every official extension, install and uninstall.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, CatalogEntry};
use crate::devices::icon;
use crate::settings_form::SettingsForm;

#[component]
pub fn Extensions() -> impl IntoView {
    let catalog = RwSignal::new(Vec::<CatalogEntry>::new());
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(None::<String>);
    let search = RwSignal::new(String::new());
    let filter = RwSignal::new("all".to_owned());
    // Which extension's settings form is open, if any — at most one at a time.
    let settings_open = RwSignal::new(None::<String>);

    let reload = move || {
        spawn_local(async move {
            match api::fetch_catalog().await {
                Ok(entries) => {
                    catalog.set(entries);
                    trouble.set(None);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    reload();

    // The categories actually present, in the order the catalog lists them, so a quick filter
    // never offers a choice that would show nothing.
    let categories = Memo::new(move |_| {
        let mut seen = Vec::new();
        for entry in catalog.get() {
            if !seen.contains(&entry.category) {
                seen.push(entry.category.clone());
            }
        }
        seen
    });

    let visible = Memo::new(move |_| {
        let needle = search.get().to_lowercase();
        let active = filter.get();
        catalog
            .get()
            .into_iter()
            .filter(|entry| {
                (active == "all" || entry.category == active)
                    && (needle.is_empty()
                        || entry.name.to_lowercase().contains(&needle)
                        || entry.description.to_lowercase().contains(&needle))
            })
            .collect::<Vec<_>>()
    });

    view! {
        <div class="page-head">
            <h1>"Extensions"</h1>
        </div>
        <p class="lede">
            "Irori itself doesn't talk to devices or run automations. Those arrive as "
            "extensions: official ones live in this repo and are not compiled into the core. "
            "Install downloads (or, in a checkout, builds) a package; uninstall deletes it "
            "and anything it brought in."
        </p>
        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        <div class="ext-controls">
            <input
                class="ext-search"
                type="search"
                placeholder="Search extensions"
                aria-label="Search extensions"
                prop:value=search
                on:input:target=move |ev| search.set(ev.target().value())
            />
            <div class="ext-chips" role="group" aria-label="Filter by category">
                <button
                    type="button"
                    class:active=move || filter.get() == "all"
                    on:click=move |_| filter.set("all".into())
                >
                    "All"
                </button>
                {move || {
                    categories
                        .get()
                        .into_iter()
                        .map(|category| {
                            let label = category_label(&category);
                            let value = category.clone();
                            view! {
                                <button
                                    type="button"
                                    class:active=move || filter.get() == category
                                    on:click=move |_| filter.set(value.clone())
                                >
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()
                }}
            </div>
        </div>

        {move || {
            let entries = visible.get();
            if entries.is_empty() {
                view! { <p class="muted">"No extensions match."</p> }.into_any()
            } else {
                view! {
                    <div class="ext-grid">
                        {
                            entries
                                .into_iter()
                                .map(|entry| card(entry, busy, catalog, trouble, settings_open))
                                .collect_view()
                        }
                    </div>
                }
                    .into_any()
            }
        }}
    }
}

fn card(
    entry: CatalogEntry,
    busy: RwSignal<Option<String>>,
    catalog: RwSignal<Vec<CatalogEntry>>,
    trouble: RwSignal<Option<String>>,
    settings_open: RwSignal<Option<String>>,
) -> impl IntoView {
    let id = entry.id.clone();
    let id_busy = id.clone();
    let id_click = id.clone();
    let id_gear = id.clone();
    let id_form = id.clone();
    let installed = entry.installed;
    let running = entry.state.as_deref() == Some("running");
    let schema = entry.config_schema.clone();
    // A `Memo` rather than a plain closure: it's `Copy`, so the same check can be read from the
    // button's `disabled`, its progress bar, and its label without cloning the id three times.
    let is_busy = Memo::new(move |_| busy.get().as_deref() == Some(id_busy.as_str()));
    let is_open = {
        let id = id.clone();
        move || settings_open.get().as_deref() == Some(id.as_str())
    };

    view! {
        <section class="ext-card">
            <span class="ext-category">{category_label(&entry.category)}</span>
            <div class="ext-card-head">
                // `entry.icon` already reflects whether the icon endpoint has bytes to serve
                // right now (the server checks the same live state), so nothing to combine here.
                {icon(&entry.id, entry.icon)}
                <div class="ext-card-title">
                    <span class="name">{entry.name.clone()}</span>
                    <span class="muted small">{entry.version.clone()}</span>
                </div>
            </div>
            {entry.state.clone().map(|state| view! {
                <span class="state" class:ok=running>{state}</span>
            })}
            <p class="muted ext-description">{entry.description.clone()}</p>
            {entry.reason.clone().map(|why| view! { <p class="why">{why}</p> })}
            <div class="ext-actions">
                {if installed {
                    view! {
                        <button
                            type="button"
                            class="danger ext-btn"
                            disabled=move || is_busy.get()
                            on:click=move |_| act(id_click.clone(), false, busy, catalog, trouble)
                        >
                            {move || is_busy.get().then(|| view! { <span class="bar" aria-hidden="true"></span> })}
                            <span class="label">
                                {move || if is_busy.get() { "Uninstalling…" } else { "Uninstall" }}
                            </span>
                        </button>
                    }
                        .into_any()
                } else {
                    view! {
                        <button
                            type="button"
                            class="add ext-btn"
                            disabled=move || is_busy.get()
                            on:click=move |_| act(id.clone(), true, busy, catalog, trouble)
                        >
                            {move || is_busy.get().then(|| view! { <span class="bar" aria-hidden="true"></span> })}
                            <span class="label">
                                {move || if is_busy.get() { "Installing…" } else { "Install" }}
                            </span>
                        </button>
                    }
                        .into_any()
                }}
                {(installed && schema.is_some()).then(|| {
                    view! {
                        <button
                            type="button"
                            class="ext-settings-btn"
                            aria-label="Settings"
                            title="Settings"
                            on:click=move |_| {
                                settings_open
                                    .update(|open| {
                                        *open = if open.as_deref() == Some(id_gear.as_str()) {
                                            None
                                        } else {
                                            Some(id_gear.clone())
                                        };
                                    })
                            }
                        >
                            "⚙"
                        </button>
                    }
                })}
            </div>
            {move || {
                is_open()
                    .then(|| {
                        schema
                            .clone()
                            .map(|schema| {
                                let id_form = id_form.clone();
                                view! {
                                    <SettingsForm
                                        id=id_form.clone()
                                        schema=schema
                                        on_close=move || settings_open.set(None)
                                    />
                                }
                            })
                    })
            }}
        </section>
    }
}

fn category_label(category: &str) -> String {
    match category {
        "protocol" => "Protocol".into(),
        "demo" => "Demo".into(),
        "automation" => "Automation".into(),
        other => other.into(),
    }
}

fn act(
    id: String,
    install: bool,
    busy: RwSignal<Option<String>>,
    catalog: RwSignal<Vec<CatalogEntry>>,
    trouble: RwSignal<Option<String>>,
) {
    busy.set(Some(id.clone()));
    spawn_local(async move {
        let result = if install {
            api::install_extension(&id).await
        } else {
            api::uninstall_extension(&id).await
        };
        match result {
            Ok(()) => {
                trouble.set(None);
                match api::fetch_catalog().await {
                    Ok(entries) => catalog.set(entries),
                    Err(why) => trouble.set(Some(why)),
                }
            }
            Err(why) => trouble.set(Some(why)),
        }
        busy.set(None);
    });
}
