//! The Extensions page: every official extension, install and uninstall.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, CatalogEntry};

#[component]
pub fn Extensions() -> impl IntoView {
    let catalog = RwSignal::new(Vec::<CatalogEntry>::new());
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(None::<String>);

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
        {move || {
            let entries = catalog.get();
            let mut groups: Vec<(String, Vec<CatalogEntry>)> = Vec::new();
            for entry in entries {
                let label = category_label(&entry.category);
                if let Some((_, list)) = groups.iter_mut().find(|(name, _)| name == &label) {
                    list.push(entry);
                } else {
                    groups.push((label, vec![entry]));
                }
            }
            groups
                .into_iter()
                .map(|(label, list)| view! {
                    <section class="card">
                        <h2>{label}</h2>
                        <ul class="ext-list">
                            {list.into_iter().map(|entry| {
                                let id = entry.id.clone();
                                let id_busy = id.clone();
                                let id_click = id.clone();
                                let installed = entry.installed;
                                let running = entry.state.as_deref() == Some("running");
                                view! {
                                    <li>
                                        <div class="ext-head">
                                            <span class="name">{entry.name.clone()}</span>
                                            {entry.state.clone().map(|state| view! {
                                                <span class="state" class:ok=running>
                                                    {state}
                                                </span>
                                            })}
                                            <span class="muted small">{entry.version.clone()}</span>
                                        </div>
                                        <p class="muted">{entry.description.clone()}</p>
                                        {entry.reason.clone().map(|why| view! { <p class="why">{why}</p> })}
                                        <div class="ext-actions">
                                            {if installed {
                                                view! {
                                                    <button
                                                        type="button"
                                                        class="danger"
                                                        disabled=move || busy.get().as_deref() == Some(id_busy.as_str())
                                                        on:click=move |_| act(id_click.clone(), false, busy, catalog, trouble)
                                                    >
                                                        "Uninstall"
                                                    </button>
                                                }.into_any()
                                            } else {
                                                view! {
                                                    <button
                                                        type="button"
                                                        class="add"
                                                        disabled=move || busy.get().as_deref() == Some(id_busy.as_str())
                                                        on:click=move |_| act(id_click.clone(), true, busy, catalog, trouble)
                                                    >
                                                        {move || if busy.get().as_deref() == Some(id.as_str()) {
                                                            "Installing…"
                                                        } else {
                                                            "Install"
                                                        }}
                                                    </button>
                                                }.into_any()
                                            }}
                                        </div>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    </section>
                })
                .collect_view()
        }}
    }
}

fn category_label(category: &str) -> String {
    match category {
        "protocol" => "Protocols".into(),
        "demo" => "Demo".into(),
        "helpers" => "Helpers".into(),
        "automation" => "Automations".into(),
        other => other.to_owned(),
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
