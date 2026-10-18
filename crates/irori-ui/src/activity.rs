//! The Activity row of Settings: who did what in the home, newest first. An owner's to read.
//!
//! Each row says it as a person would: who, what, to what, and, for something that was refused or
//! didn't work, a chip and the reason. It is read from `GET /api/audit`, a page at a time, and can
//! be narrowed to one person or to what was refused.

use irori_types::{AuditActor, AuditEntry, Outcome};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

/// How the row stands while it's folded.
pub fn summary() -> &'static str {
    "who did what"
}

/// What a row of the log says, as a sentence without its subject's tense: "added the room hall".
/// The subject goes in front, and a refusal or a failure is a chip beside it, so the sentence
/// is the same whether it worked or not.
pub fn what(entry: &AuditEntry) -> String {
    let target = entry.target.as_deref().unwrap_or("");
    let detail = entry.detail.as_deref().unwrap_or("");
    let ok = entry.outcome == Outcome::Ok;
    let named = |verb: &str| {
        if target.is_empty() {
            verb.to_owned()
        } else {
            format!("{verb} {target}")
        }
    };
    match entry.action.as_str() {
        "sign_in" if ok => "signed in".to_owned(),
        "sign_in" if target.is_empty() => "tried to sign in with a name nobody has".to_owned(),
        "sign_in" => format!("tried to sign in as {target}"),
        "sign_out" => "signed out".to_owned(),
        "owner_set_up" if ok => "set the home up".to_owned(),
        "owner_set_up" => "tried to set the home up again".to_owned(),
        "person_added" => format!("added {detail}"),
        // What was changed is the point of the row: the role, the rooms, the name.
        "person_changed" if detail.is_empty() => named("changed"),
        "person_changed" => format!("{}: {detail}", named("changed")),
        "person_removed" => format!("removed {detail}"),
        "token_made" => format!("made the token {detail}"),
        "token_revoked" => format!("revoked the token {detail}"),
        // A refused command's detail goes on past the command, with the reason (`turn_on: not
        // in their rooms`); the reason is the line under the sentence, not part of it.
        "command" => format!(
            "sent {} to {target}",
            detail.split_once(": ").map_or(detail, |(command, _)| command)
        ),
        "place_changed" => "changed where the home is".to_owned(),
        "history_kept_changed" => "changed how long history is kept".to_owned(),
        "restart" => "restarted Irori".to_owned(),
        "room_added" => "added a room".to_owned(),
        "room_changed" => named("changed the room"),
        "room_removed" => named("removed the room"),
        "floor_added" => "added a floor".to_owned(),
        "floor_changed" => named("changed the floor"),
        "floor_removed" => named("removed the floor"),
        "floorplan_saved" => "saved the floorplan".to_owned(),
        "device_changed" => named("changed the device"),
        "device_removed" => named("removed the device"),
        "entity_changed" => named("changed"),
        "extension_secret_given" => named("gave a secret to"),
        "extension_settings_changed" => named("changed the settings of"),
        "extension_action_started" => named("started an action of"),
        "extension_action_stopped" => named("stopped an action of"),
        "extension_installed" => named("installed the extension"),
        "extension_removed" => named("removed the extension"),
        "helper_added" => "added a helper".to_owned(),
        "helper_changed" => named("changed the helper"),
        "helper_removed" => named("removed the helper"),
        "assistant_changed" => "changed the assistant".to_owned(),
        "conversation_cleared" => "cleared a conversation".to_owned(),
        "backup_made" => "made a backup".to_owned(),
        "backup_uploaded" => "uploaded a backup".to_owned(),
        "backup_settings_changed" => "changed how backups are made".to_owned(),
        "backup_key_shown" => "looked at the backup recovery key".to_owned(),
        "backup_key_changed" => "changed the backup recovery key".to_owned(),
        "backup_downloaded" => named("downloaded the backup"),
        "backup_fetched" => "fetched a backup".to_owned(),
        "backup_removed" => named("removed the backup"),
        "flow_saved" => named("saved the automation"),
        "flow_turned_on" => named("turned on the automation"),
        // Said by a person it is just that; said by Irori, because the home changed under the
        // flow, it comes with the reason.
        "flow_turned_off" if detail.is_empty() => named("turned off the automation"),
        "flow_turned_off" => format!("{}: {detail}", named("turned off the automation")),
        "flow_deleted" => named("deleted the automation"),
        "flow_restored" => named("brought back an older version of the automation"),
        "flow_test_run" => named("ran a test of the automation"),
        "flow_handed_over" => format!("{}: {detail}", named("handed over the automation")),
        "restore_cancelled" => "cancelled a restore".to_owned(),
        "restore_asked" => named("asked for a restore from"),
        "restore_applied" => named("applied the restore of"),
        // Irori's own notes about the log say it in their detail.
        "activity_trimmed" | "activity_lost" | "tries_unwritten" => detail.to_owned(),
        other => other.replace('_', " "),
    }
}

/// A row as one line: who, then what.
pub fn sentence(entry: &AuditEntry) -> String {
    match entry.action.as_str() {
        // These are Irori's remarks, not something somebody did.
        "activity_trimmed" | "activity_lost" | "tries_unwritten" => what(entry),
        _ => format!("{} {}", subject(&entry.actor), what(entry)),
    }
}

fn subject(actor: &AuditActor) -> String {
    match actor {
        AuditActor::Nobody => "Someone".to_owned(),
        other => other.to_string(),
    }
}

/// Why a refusal or a failure happened, when the row says: what goes under the sentence.
fn why(entry: &AuditEntry) -> Option<String> {
    if entry.outcome == Outcome::Ok {
        return None;
    }
    entry.detail.clone().filter(|detail| !detail.is_empty())
}

fn chip(outcome: Outcome) -> Option<&'static str> {
    match outcome {
        Outcome::Ok => None,
        Outcome::Denied => Some("Refused"),
        Outcome::Failed => Some("Didn't work"),
    }
}

#[component]
pub fn Section(
    /// Whether the row is open: the log is read when it opens, and again when it is narrowed.
    open: Signal<bool>,
) -> impl IntoView {
    let crate::users::People(people) = expect_context::<crate::users::People>();
    let rows = RwSignal::new(Vec::<AuditEntry>::new());
    let older = RwSignal::new(None::<u64>);
    let trouble = RwSignal::new(None::<String>);
    let loading = RwSignal::new(false);
    // Who to look at: everybody, or one person by id.
    let person = RwSignal::new(String::new());
    // What to look at: everything, or only what was refused.
    let refused = RwSignal::new(false);

    // Which look at the log is the latest. Narrowing it while an earlier look is still coming
    // asks again, and the answer to the earlier one is dropped when it lands.
    let latest = StoredValue::new(0u64);
    let load = move |more: bool| {
        // Older rows are asked for once at a time; a new look at the log is never refused.
        if more && loading.get_untracked() {
            return;
        }
        if !more {
            latest.update_value(|latest| *latest += 1);
        }
        let mine = latest.get_value();
        loading.set(true);
        let before = if more { older.get_untracked() } else { None };
        let who = person.get_untracked();
        let outcome = refused.get_untracked().then_some(Outcome::Denied);
        spawn_local(async move {
            let asked =
                api::fetch_audit((!who.is_empty()).then_some(who.as_str()), outcome, before).await;
            if latest.get_value() != mine {
                return;
            }
            match asked {
                Ok(page) => {
                    trouble.set(None);
                    older.set(page.older);
                    if more {
                        rows.update(|rows| rows.extend(page.entries));
                    } else {
                        rows.set(page.entries);
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
            loading.set(false);
        });
    };
    // Read when the row opens and whenever it is narrowed; not while it is folded.
    Effect::new(move |_| {
        let _ = (person.get(), refused.get());
        if open.get() {
            load(false);
        }
    });

    view! {
        <p class="muted small">
            "Everything that changed the home, and everything that was refused, with who did it. \
             Looking is not written down. It keeps three months, or the last hundred thousand rows."
        </p>
        <div class="activity-bar">
            <label class="activity-who">
                <span class="muted small">"Who"</span>
                <select on:change=move |event| person.set(event_target_value(&event))
                    prop:value=move || person.get()>
                    <option value="">"Everybody"</option>
                    <For each=move || people.get() key=|user| user.id.clone() let:user>
                        <option value=user.id.to_string()>{user.name.as_str().to_owned()}</option>
                    </For>
                </select>
            </label>
            <div class="choices" role="group" aria-label="What to show">
                <button type="button" aria-pressed=move || (!refused.get()).to_string()
                    on:click=move |_| refused.set(false)>"Everything"</button>
                <button type="button" aria-pressed=move || refused.get().to_string()
                    on:click=move |_| refused.set(true)>"Refused"</button>
            </div>
        </div>
        {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        <ul class="activity">
            {move || {
                let all = rows.get();
                if all.is_empty() {
                    let words = if loading.get() { "Reading…" } else { "Nothing yet." };
                    return view! { <li class="activity-none muted">{words}</li> }.into_any();
                }
                all.into_iter().map(|entry| {
                    let said = sentence(&entry);
                    let reason = why(&entry);
                    let outcome = entry.outcome;
                    let when = crate::backups::when(&entry.at.to_string());
                    view! {
                        <li class="act" class:bad=outcome != Outcome::Ok>
                            <span class="act-when muted small">{when}</span>
                            <span class="act-words">
                                <span class="act-said">{said}</span>
                                {chip(outcome).map(|word| view! {
                                    <span class="scope-chip refusal">{word}</span>
                                })}
                                {reason.map(|why| view! { <span class="act-why muted small">{why}</span> })}
                            </span>
                        </li>
                    }
                }).collect_view().into_any()
            }}
        </ul>
        {move || older.get().map(|_| view! {
            <button type="button" class="press" disabled=move || loading.get()
                on:click=move |_| load(true)>
                "Show older"
            </button>
        })}
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{TokenId, UserId};

    use super::*;

    fn entry(actor: AuditActor, action: &str, outcome: Outcome) -> AuditEntry {
        AuditEntry {
            id: 1,
            at: "2026-10-19T12:00:00Z".parse().expect("a time"),
            actor,
            action: action.to_owned(),
            target: None,
            outcome,
            detail: None,
            context_id: None,
            client: None,
        }
    }

    fn leo() -> AuditActor {
        AuditActor::Person {
            id: UserId::try_from("leo").expect("valid"),
            name: "Leo".to_owned(),
        }
    }

    #[test]
    fn a_command_reads_as_who_sent_what_to_what() {
        let mut row = entry(leo(), "command", Outcome::Ok);
        row.target = Some("light.hall".to_owned());
        row.detail = Some("turn_on".to_owned());
        assert_eq!(sentence(&row), "Leo sent turn_on to light.hall");
        assert_eq!(chip(row.outcome), None);
        // Refused, it is the same sentence with a chip, and the reason under it.
        row.outcome = Outcome::Denied;
        row.detail = Some("turn_on: not in their rooms".to_owned());
        assert_eq!(chip(row.outcome), Some("Refused"));
        assert_eq!(sentence(&row), "Leo sent turn_on to light.hall");
        assert_eq!(why(&row).as_deref(), Some("turn_on: not in their rooms"));
    }

    #[test]
    fn a_change_to_a_person_says_what_was_changed() {
        let mut row = entry(leo(), "person_changed", Outcome::Ok);
        row.target = Some("mia".to_owned());
        assert_eq!(sentence(&row), "Leo changed mia");
        row.detail = Some("role to owner; rooms to the whole home".to_owned());
        assert_eq!(
            sentence(&row),
            "Leo changed mia: role to owner; rooms to the whole home"
        );
    }

    #[test]
    fn nobody_who_got_the_password_wrong_is_someone() {
        let mut row = entry(AuditActor::Nobody, "sign_in", Outcome::Denied);
        assert_eq!(
            sentence(&row),
            "Someone tried to sign in with a name nobody has"
        );
        row.target = Some("leo".to_owned());
        assert_eq!(sentence(&row), "Someone tried to sign in as leo");
    }

    #[test]
    fn a_program_is_named_with_the_person_it_is_for() {
        let tablet = AuditActor::Token {
            id: TokenId::try_from("tablet").expect("valid"),
            name: "Tablet".to_owned(),
            person: None,
        };
        let mut row = entry(tablet, "command", Outcome::Ok);
        row.target = Some("light.hall".to_owned());
        row.detail = Some("toggle".to_owned());
        assert_eq!(sentence(&row), "token “Tablet” sent toggle to light.hall");
    }

    #[test]
    fn a_flow_turned_off_by_irori_says_why() {
        let mut row = entry(AuditActor::Irori, "flow_turned_off", Outcome::Ok);
        row.target = Some("hall_light".to_owned());
        row.detail = Some("`binary_sensor.hall` = no such entity in your rooms".to_owned());
        assert_eq!(
            sentence(&row),
            "Irori turned off the automation hall_light: `binary_sensor.hall` = no such entity in your rooms"
        );
        // A person who did it needs no reason.
        let mut row = entry(leo(), "flow_turned_off", Outcome::Ok);
        row.target = Some("hall_light".to_owned());
        assert_eq!(sentence(&row), "Leo turned off the automation hall_light");
    }

    #[test]
    fn irori_speaks_for_itself_when_the_log_was_cut() {
        let mut row = entry(AuditActor::Irori, "activity_trimmed", Outcome::Ok);
        row.detail = Some("5000 of the oldest rows were removed".to_owned());
        assert_eq!(sentence(&row), "5000 of the oldest rows were removed");
    }

    #[test]
    fn a_name_the_page_has_not_heard_of_is_still_readable() {
        let row = entry(leo(), "some_new_thing", Outcome::Ok);
        assert_eq!(sentence(&row), "Leo some new thing");
    }
}
