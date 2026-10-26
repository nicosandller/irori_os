//! What a request leaves in the activity log (`docs/specs/access.md` §3).
//!
//! The guard (`auth::guard`) gives every request a [`Trail`] and writes one row from it when the
//! request is over, so a change is recorded whether or not the handler that made it remembered
//! to say so. A handler adds what only it knows (the entity a command was for, the context of the
//! call, who a wrong password was typed for) and may leave the rest.
//!
//! [`ACTIONS`] says, for each address that changes something, what the row is called and
//! whether the answer waits until it is written. A test fails when an address is added to the
//! router without a row here.

use std::sync::{Arc, Mutex, PoisonError};

use axum::http::{Method, StatusCode};
use irori_types::{AuditActor, ContextId, Outcome};

use crate::audit::{Audit, Row};

/// What a request wants written about it. Cheap to clone; a clone writes to the same notes.
#[derive(Debug, Clone, Default)]
pub struct Trail(Arc<Mutex<Notes>>);

#[derive(Debug, Default)]
struct Notes {
    actor: Option<AuditActor>,
    target: Option<String>,
    detail: Option<String>,
    outcome: Option<Outcome>,
    context_id: Option<ContextId>,
    client: Option<std::net::IpAddr>,
    /// What the row is called when the handler knows better than the table.
    action: Option<&'static str>,
}

impl Trail {
    fn notes(&self) -> std::sync::MutexGuard<'_, Notes> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Who did it, when the guard could not know: a sign-in is somebody only once it has worked.
    pub fn actor(&self, actor: AuditActor) {
        self.notes().actor = Some(actor);
    }

    /// What it was done to, when that is not the first thing in the address.
    pub fn target(&self, target: impl Into<String>) {
        self.notes().target = Some(target.into());
    }

    /// A few words more. Never a password, a key, or a lock's code.
    pub fn detail(&self, detail: impl Into<String>) {
        self.notes().detail = Some(detail.into());
    }

    /// What came of it, when the status says something else: a thing answered as missing that
    /// was only out of someone's rooms was denied, not lost.
    pub fn outcome(&self, outcome: Outcome) {
        self.notes().outcome = Some(outcome);
    }

    /// Names the row, for an address whose table entry is [`Log::Quiet`] because most of what is
    /// asked there is not a change, and this one was (`docs/specs/access.md` §3).
    pub fn said(&self, action: &'static str) {
        self.notes().action = Some(action);
    }

    /// The context of the call a command made, so what it changes can be traced back.
    pub fn context(&self, context: ContextId) {
        self.notes().context_id = Some(context);
    }

    /// Where the request came from.
    pub fn from(&self, client: Option<std::net::IpAddr>) {
        self.notes().client = client;
    }
}

/// What an address that changes something leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Log {
    /// A row, written by the log's own thread.
    Row(&'static str),
    /// A row, written before the answer goes out: what must not be lost to a crash or a restart
    /// that follows, and what the person asking is told is done.
    Sync(&'static str),
    /// Not a change: a check, a preview, or a thing marked as seen.
    Quiet,
}

use Log::{Quiet, Row as Later, Sync as Now};

/// Every address that changes something, and the few that are read but worth a row (a backup
/// taken out of the house, the key that opens it). Reads are otherwise not recorded.
#[rustfmt::skip]
pub const ACTIONS: &[(&str, &str, Log)] = &[
    ("POST", "/api/session", Now("sign_in")),
    ("DELETE", "/api/session", Now("sign_out")),
    ("POST", "/api/setup", Now("owner_set_up")),
    ("POST", "/api/users", Now("person_added")),
    ("PATCH", "/api/users/{id}", Now("person_changed")),
    ("DELETE", "/api/users/{id}", Now("person_removed")),
    ("PUT", "/api/place", Later("place_changed")),
    ("POST", "/api/command", Later("command")),
    ("POST", "/api/tokens", Now("token_made")),
    ("DELETE", "/api/tokens/{id}", Now("token_revoked")),
    ("PUT", "/api/recorder", Later("history_kept_changed")),
    ("POST", "/api/restart", Now("restart")),
    ("POST", "/api/areas", Later("room_added")),
    ("PATCH", "/api/areas/{id}", Later("room_changed")),
    ("DELETE", "/api/areas/{id}", Later("room_removed")),
    ("POST", "/api/floors", Later("floor_added")),
    ("PATCH", "/api/floors/{id}", Later("floor_changed")),
    ("DELETE", "/api/floors/{id}", Later("floor_removed")),
    ("PUT", "/api/floorplan", Later("floorplan_saved")),
    ("PATCH", "/api/devices/{id}", Later("device_changed")),
    ("DELETE", "/api/devices/{id}", Later("device_removed")),
    ("PATCH", "/api/entities/{id}", Later("entity_changed")),
    ("PUT", "/api/extensions/{id}/secrets", Later("extension_secret_given")),
    ("POST", "/api/extensions/{id}/settings", Later("extension_settings_changed")),
    ("POST", "/api/extensions/{id}/actions/{action_id}", Later("extension_action_started")),
    ("DELETE", "/api/extensions/{id}/actions/{action_id}", Later("extension_action_stopped")),
    ("POST", "/api/extensions/{id}/install", Later("extension_installed")),
    ("POST", "/api/extensions/install", Later("extension_installed")),
    ("DELETE", "/api/extensions/{id}", Later("extension_removed")),
    ("POST", "/api/helpers/{table}", Later("helper_added")),
    ("POST", "/api/helpers/{table}/check", Quiet),
    ("POST", "/api/helpers/{table}/preview", Quiet),
    ("PATCH", "/api/helpers/{table}/{id}", Later("helper_changed")),
    ("DELETE", "/api/helpers/{table}/{id}", Later("helper_removed")),
    // What an extension's page asks of its engine. The engine says what it was (`apps.rs`).
    ("POST", "/api/apps/{id}/rpc", Quiet),
    ("PUT", "/api/assistant", Later("assistant_changed")),
    ("POST", "/api/assistant/turns", Quiet),
    ("POST", "/api/assistant/turns/{scope}/stop", Quiet),
    ("POST", "/api/assistant/check", Quiet),
    ("POST", "/api/assistant/pull", Later("assistant_changed")),
    ("POST", "/api/assistant/forget", Later("assistant_changed")),
    ("POST", "/api/assistant/install", Later("assistant_changed")),
    ("POST", "/api/assistant/uninstall", Later("assistant_changed")),
    ("POST", "/api/assistant/load", Later("assistant_changed")),
    ("POST", "/api/assistant/unload", Later("assistant_changed")),
    ("DELETE", "/api/assistant/transcript/{scope}", Later("conversation_cleared")),
    ("POST", "/api/backups", Later("backup_made")),
    ("POST", "/api/backups/upload", Later("backup_uploaded")),
    ("PUT", "/api/backups/settings", Later("backup_settings_changed")),
    ("GET", "/api/backups/key", Now("backup_key_shown")),
    ("POST", "/api/backups/key", Now("backup_key_changed")),
    ("POST", "/api/backups/restore/cancel", Later("restore_cancelled")),
    ("POST", "/api/backups/restore/seen", Quiet),
    ("POST", "/api/backups/destinations/{place}/test", Quiet),
    ("POST", "/api/backups/destinations/{place}/fetch", Later("backup_fetched")),
    ("GET", "/api/backups/{name}", Now("backup_downloaded")),
    ("DELETE", "/api/backups/{name}", Later("backup_removed")),
    ("POST", "/api/backups/{name}/verify", Quiet),
    ("POST", "/api/backups/{name}/restore", Now("restore_asked")),
    ("POST", "/api/setup/backup/upload", Later("backup_uploaded")),
    ("POST", "/api/setup/backup/restore", Now("restore_asked")),
    ("POST", "/api/setup/backup/verify", Quiet),
];

/// Looks the address up. `None` for one that is only read.
fn find(method: &Method, pattern: &str) -> Option<Log> {
    ACTIONS
        .iter()
        .find(|(m, p, _)| *m == method.as_str() && *p == pattern)
        .map(|(_, _, log)| *log)
}

/// The first thing in the address that stands for something: `/api/devices/{id}` called as
/// `/api/devices/lamp` is about `lamp`.
fn first_capture(pattern: &str, path: &str) -> Option<String> {
    pattern
        .split('/')
        .zip(path.split('/'))
        .find(|(wanted, _)| wanted.starts_with('{'))
        .map(|(_, got)| got.to_owned())
}

/// What the status says of how it went, when the handler said nothing.
fn outcome_of(status: StatusCode) -> Outcome {
    match status.as_u16() {
        200..=399 => Outcome::Ok,
        401 | 403 | 429 => Outcome::Denied,
        _ => Outcome::Failed,
    }
}

/// Writes the row a finished request leaves, if it leaves one.
///
/// Somebody who is nobody leaves a row only for signing in and for setting the home up: the
/// rest is anyone on the network knocking, and a log that wrote it down would be a way to
/// push out what matters.
pub async fn record(
    audit: &Audit,
    trail: &Trail,
    method: &Method,
    pattern: &str,
    path: &str,
    status: StatusCode,
) {
    let said = trail.notes().action;
    let log = match find(method, pattern) {
        Some(Log::Quiet) => match said {
            Some(action) => Log::Row(action),
            None => return,
        },
        Some(log) => log,
        // Anything that changes and isn't in the table is still written down, under a name
        // that says so; the test over the table is what keeps this from being the way.
        None if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) => return,
        None => Log::Row("change"),
    };
    let (Log::Row(action) | Log::Sync(action)) = log else {
        return;
    };
    let notes = {
        let mut notes = trail.notes();
        Notes {
            actor: notes.actor.take(),
            target: notes.target.take(),
            detail: notes.detail.take(),
            outcome: notes.outcome.take(),
            context_id: notes.context_id.take(),
            client: notes.client.take(),
            action: notes.action.take(),
        }
    };
    let actor = notes.actor.unwrap_or(AuditActor::Nobody);
    if matches!(actor, AuditActor::Nobody) && !matches!(action, "sign_in" | "owner_set_up") {
        return;
    }
    let outcome = notes.outcome.unwrap_or_else(|| outcome_of(status));
    // A GET that is recorded only matters when it worked: a refused one gave nothing away.
    if *method == Method::GET && outcome != Outcome::Ok {
        return;
    }
    let mut row = Row::new(actor, action, outcome);
    row.target = notes
        .target
        .or_else(|| first_capture(pattern, path))
        .filter(|target| !target.is_empty());
    row.detail = notes
        .detail
        .or_else(|| (log == Log::Row("change")).then(|| format!("{method} {pattern}")));
    row.context_id = notes.context_id;
    // Where from, for the rows that are about getting in or being turned away.
    if matches!(action, "sign_in" | "owner_set_up") || outcome != Outcome::Ok {
        row.client = notes.client.map(|ip| ip.to_string());
    }
    match log {
        Log::Sync(_) => audit.record_and_wait(row).await,
        _ => audit.record(row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `(method, address)` pairs the router is built with, read out of the source, so that
    /// an address somebody adds shows up here without anyone remembering to say so.
    fn routed() -> std::collections::BTreeSet<(String, String)> {
        let mut found = std::collections::BTreeSet::new();
        for source in [include_str!("../server.rs"), include_str!("backups.rs")] {
            for after in source.split(".route(").skip(1) {
                let Some(after) = after.trim_start().strip_prefix('"') else {
                    continue;
                };
                let Some(end) = after.find('"') else {
                    continue;
                };
                let path = &after[..end];
                if !path.starts_with("/api/") {
                    continue;
                }
                // The call ends at the next route, or at the end of the chain.
                let rest = &after[end..];
                let stop = [".route(", ".merge(", ".fallback(", ";"]
                    .iter()
                    .filter_map(|mark| rest.find(mark))
                    .min()
                    .unwrap_or(rest.len());
                let call = &rest[..stop];
                for (word, method) in [
                    ("get", "GET"),
                    ("post", "POST"),
                    ("put", "PUT"),
                    ("patch", "PATCH"),
                    ("delete", "DELETE"),
                ] {
                    let mut from = 0;
                    while let Some(at) = call[from..].find(&format!("{word}(")) {
                        let at = from + at;
                        let before = call[..at].chars().next_back();
                        if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            found.insert((method.to_owned(), path.to_owned()));
                        }
                        from = at + word.len();
                    }
                }
            }
        }
        found
    }

    #[test]
    fn the_test_reads_the_router() {
        let routed = routed();
        for pair in [
            ("POST", "/api/command"),
            ("DELETE", "/api/tokens/{id}"),
            ("PATCH", "/api/users/{id}"),
            ("POST", "/api/backups/{name}/restore"),
            ("DELETE", "/api/extensions/{id}"),
            ("PUT", "/api/backups/settings"),
        ] {
            assert!(
                routed.contains(&(pair.0.to_owned(), pair.1.to_owned())),
                "{pair:?} should be read out of the router: {routed:?}"
            );
        }
        // A read is not mistaken for a change.
        assert!(!routed.contains(&("POST".to_owned(), "/api/home".to_owned())));
    }

    /// An address that changes something is named here, so what it leaves in the log is a
    /// decision somebody made rather than a default.
    #[test]
    fn every_change_is_named() {
        let routed = routed();
        let forgotten: Vec<_> = routed
            .iter()
            .filter(|(method, _)| method != "GET")
            .filter(|(method, path)| !ACTIONS.iter().any(|(m, p, _)| m == method && p == path))
            .collect();
        assert!(
            forgotten.is_empty(),
            "these addresses change something and ACTIONS doesn't say what they leave in the \
             activity log: {forgotten:?}"
        );
        let gone: Vec<_> = ACTIONS
            .iter()
            .filter(|(m, p, _)| !routed.contains(&((*m).to_owned(), (*p).to_owned())))
            .collect();
        assert!(gone.is_empty(), "in ACTIONS and no longer served: {gone:?}");
    }

    #[test]
    fn a_row_is_called_the_same_whoever_made_it() {
        let mut names: Vec<&str> = ACTIONS
            .iter()
            .filter_map(|(_, _, log)| match log {
                Log::Row(name) | Log::Sync(name) => Some(*name),
                Log::Quiet => None,
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name}"
            );
        }
    }

    #[test]
    fn what_an_address_is_about_is_the_first_thing_in_it() {
        assert_eq!(
            first_capture("/api/devices/{id}", "/api/devices/lamp").as_deref(),
            Some("lamp")
        );
        assert_eq!(
            first_capture(
                "/api/extensions/{id}/actions/{action_id}",
                "/api/extensions/zwave/actions/heal"
            )
            .as_deref(),
            Some("zwave")
        );
        assert_eq!(first_capture("/api/command", "/api/command"), None);
    }

    #[test]
    fn a_status_is_an_outcome() {
        assert_eq!(outcome_of(StatusCode::NO_CONTENT), Outcome::Ok);
        assert_eq!(outcome_of(StatusCode::FORBIDDEN), Outcome::Denied);
        assert_eq!(outcome_of(StatusCode::UNAUTHORIZED), Outcome::Denied);
        assert_eq!(outcome_of(StatusCode::TOO_MANY_REQUESTS), Outcome::Denied);
        assert_eq!(outcome_of(StatusCode::BAD_REQUEST), Outcome::Failed);
        assert_eq!(outcome_of(StatusCode::BAD_GATEWAY), Outcome::Failed);
    }
}
