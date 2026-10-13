//! What the core adds to an `update` entity that no device keeps: the version someone chose to
//! skip, and why the last install didn't finish (`docs/specs/entities.md` §5.3). A protocol
//! reports what the device says; this folds the rest in before anyone sees it.

use std::collections::HashMap;

use irori_types::{Context, EntityId, EntityState, ProtocolId, State, UniqueId, UpdateState};
use serde::{Deserialize, Serialize};

use super::{Home, Key, Stamp, system_context};
use crate::Event;
use crate::services::CallError;

/// What an update says when an install ends where it began.
pub(crate) const STOPPED: &str = "It stopped before the new version was installed.";

/// One skipped version as it's stored: which entity, by what its protocol calls it (an entity id
/// can be given a new name), and the version passed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Skip {
    pub protocol: ProtocolId,
    pub unique_id: UniqueId,
    pub version: String,
}

/// The longest text kept from a device about an update: a version, a title, a summary, a reason.
const TEXT_MAX: usize = 255;

/// The longest link kept.
const LINK_MAX: usize = 2048;

/// What a device says about an update, made safe to keep and to show before anything checks it.
/// A device's text is its own: it is cut to a length worth keeping, and its link is dropped
/// unless it is a web address, since a page turns it into something to click. The skipped
/// version is the core's alone, so whatever a protocol put there goes.
pub(super) fn clean(update: &mut UpdateState) {
    update.skipped_version = None;
    for text in [
        &mut update.installed_version,
        &mut update.latest_version,
        &mut update.title,
        &mut update.release_summary,
        &mut update.last_error,
    ] {
        *text = text
            .take()
            .map(|text| text.trim().chars().take(TEXT_MAX).collect::<String>())
            .filter(|text| !text.is_empty());
    }
    update.release_url = update.release_url.take().filter(|url| {
        url.len() <= LINK_MAX
            && (url.starts_with("https://") || url.starts_with("http://"))
            && !url.chars().any(char::is_whitespace)
    });
}

/// What the device said, with what the core knows folded in. `skipped` is the version passed
/// over, forgotten here once the device has a newer one. Why an install failed is the
/// protocol's to say when it knows (Zigbee2MQTT does); otherwise the core works it out.
pub(super) fn fold(
    old: Option<&UpdateState>,
    reported: UpdateState,
    skipped: &mut Option<String>,
) -> UpdateState {
    let told = reported
        .last_error
        .clone()
        .filter(|_| !reported.in_progress);
    let mut new = UpdateState {
        skipped_version: None,
        last_error: None,
        ..reported
    };
    if skipped.is_some() {
        if new.latest_version == *skipped {
            new.skipped_version.clone_from(skipped);
            new.available = false;
        } else if new.latest_version.is_some() {
            *skipped = None;
        }
    }
    if let Some(old) = old
        && !new.in_progress
    {
        let same_version = old.installed_version == new.installed_version;
        let still_behind = UpdateState::differs(
            new.installed_version.as_deref(),
            new.latest_version.as_deref(),
        );
        new.last_error = if told.is_some() {
            None
        } else if old.in_progress && same_version && still_behind {
            Some(STOPPED.to_owned())
        } else if same_version {
            old.last_error.clone()
        } else {
            None
        };
    }
    new.last_error = told.or(new.last_error);
    new
}

impl Home {
    /// The update `id` is, as it stands. `None` for another kind, or one that hasn't reported.
    pub fn update(&self, id: &EntityId) -> Option<&UpdateState> {
        match self.states.get(id)?.state.as_ref()? {
            State::Update(update) => Some(update),
            _ => None,
        }
    }

    /// Folds what the core knows into a report for an update entity.
    pub(super) fn fold_update(&mut self, id: &EntityId, key: &Key, reported: UpdateState) -> State {
        let mut skipped = self.skipped.get(key).cloned();
        let had = skipped.is_some();
        let new = fold(self.update(id), reported, &mut skipped);
        if had && skipped.is_none() {
            self.skipped.remove(key);
            self.skips_changed = true;
        }
        State::Update(new)
    }

    /// Passes over the version on offer: it stops counting as available until a newer one comes.
    pub fn skip_update(
        &mut self,
        id: &EntityId,
        context: &Context,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, CallError> {
        let refuse = |what: &str| CallError::NotSupported(format!("`{id}` {what}"));
        let update = self
            .update(id)
            .ok_or_else(|| refuse("hasn't said what version it has"))?;
        if update.in_progress {
            return Err(refuse("is installing; there's nothing to skip"));
        }
        let Some(version) = update.latest_version.clone().filter(|_| update.available) else {
            return Err(refuse("has no update to skip"));
        };
        let key = self
            .key_of(id)
            .ok_or_else(|| refuse("isn't here any more"))?;
        self.skipped.insert(key, version.clone());
        self.skips_changed = true;
        Ok(self.change_update(id, context.clone(), stamp, |update| {
            update.skipped_version = Some(version);
            update.available = false;
        }))
    }

    /// Takes a skip back: the version counts as available again.
    pub fn clear_skipped_update(
        &mut self,
        id: &EntityId,
        context: &Context,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, CallError> {
        let Some(key) = self.key_of(id) else {
            return Ok(vec![]);
        };
        if self.skipped.remove(&key).is_some() {
            self.skips_changed = true;
        }
        Ok(self.change_update(id, context.clone(), stamp, |update| {
            update.skipped_version = None;
            update.available = UpdateState::differs(
                update.installed_version.as_deref(),
                update.latest_version.as_deref(),
            );
        }))
    }

    /// An install is being sent: whatever went wrong last time is over.
    pub fn begin_update(&mut self, id: &EntityId, stamp: &Stamp) -> Vec<Event> {
        self.change_update(id, system_context(stamp), stamp, |update| {
            update.last_error = None;
        })
    }

    /// An install didn't happen, as far as the core can tell: says why on the entity, and takes
    /// it out of "installing" if the device left it there.
    pub fn fail_update(&mut self, id: &EntityId, why: &str, stamp: &Stamp) -> Vec<Event> {
        self.change_update(id, system_context(stamp), stamp, |update| {
            update.last_error = Some(why.to_owned());
            update.in_progress = false;
            update.progress = None;
        })
    }

    /// The skipped versions, when they've changed since this was last asked: what to store.
    pub(crate) fn skips_to_store(&mut self) -> Option<Vec<Skip>> {
        std::mem::take(&mut self.skips_changed).then(|| {
            let mut skips: Vec<Skip> = self
                .skipped
                .iter()
                .map(|((protocol, unique_id), version)| Skip {
                    protocol: protocol.clone(),
                    unique_id: unique_id.clone(),
                    version: version.clone(),
                })
                .collect();
            skips.sort_by(|a, b| (&a.protocol, &a.unique_id).cmp(&(&b.protocol, &b.unique_id)));
            skips
        })
    }

    /// The skipped versions as they were stored, before any protocol has reported.
    pub(crate) fn restore_skips(&mut self, skips: Vec<Skip>) {
        self.skipped = skips
            .into_iter()
            .map(|skip| ((skip.protocol, skip.unique_id), skip.version))
            .collect::<HashMap<_, _>>();
    }

    fn key_of(&self, id: &EntityId) -> Option<Key> {
        let entity = self.entities.get(id)?;
        Some((entity.protocol.clone(), entity.unique_id.clone()))
    }

    /// Changes an update's state in place, as the core's own doing. Nothing happens, and nothing
    /// is said, for an entity that isn't an update with a state, or when `change` changes nothing.
    fn change_update(
        &mut self,
        id: &EntityId,
        context: Context,
        stamp: &Stamp,
        change: impl FnOnce(&mut UpdateState),
    ) -> Vec<Event> {
        let Some(old) = self.states.get(id).cloned() else {
            return vec![];
        };
        let Some(State::Update(mut update)) = old.state.clone() else {
            return vec![];
        };
        let was_available = update.available;
        change(&mut update);
        let state = Some(State::Update(update.clone()));
        if state == old.state {
            return vec![];
        }
        let now = stamp.now.max(old.last_updated);
        let new = EntityState {
            state,
            last_updated: now,
            last_changed: if update.available == was_available {
                old.last_changed
            } else {
                now
            },
            context,
            ..old.clone()
        };
        self.states.insert(id.clone(), new.clone());
        vec![Event::StateChanged {
            entity_id: id.clone(),
            old_state: Some(Box::new(old)),
            new_state: Box::new(new),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(installed: &str, latest: &str) -> UpdateState {
        UpdateState {
            available: installed != latest,
            installed_version: Some(installed.into()),
            latest_version: Some(latest.into()),
            ..UpdateState::default()
        }
    }

    #[test]
    fn a_skipped_version_is_not_available_until_a_newer_one_comes() {
        let mut skipped = Some("1.5.0".to_owned());
        let folded = fold(None, at("1.4.2", "1.5.0"), &mut skipped);
        assert!(!folded.available);
        assert_eq!(folded.skipped_version.as_deref(), Some("1.5.0"));
        assert!(folded.validate().is_ok());

        let newer = fold(Some(&folded), at("1.4.2", "1.6.0"), &mut skipped);
        assert!(newer.available, "1.6.0 was never skipped");
        assert_eq!(newer.skipped_version, None);
        assert_eq!(skipped, None, "and the skip is forgotten");
    }

    #[test]
    fn an_install_that_ends_where_it_began_failed() {
        let running = UpdateState {
            in_progress: true,
            progress: Some(60.0),
            ..at("1.4.2", "1.5.0")
        };
        let stopped = fold(Some(&running), at("1.4.2", "1.5.0"), &mut None);
        assert_eq!(stopped.last_error.as_deref(), Some(STOPPED));
        // It keeps saying so across later reports, until something changes.
        let again = fold(Some(&stopped), at("1.4.2", "1.5.0"), &mut None);
        assert_eq!(again.last_error.as_deref(), Some(STOPPED));
        let retry = fold(Some(&again), running.clone(), &mut None);
        assert_eq!(retry.last_error, None);

        let done = fold(Some(&running), at("1.5.0", "1.5.0"), &mut None);
        assert_eq!(done.last_error, None);
        assert!(!done.available);
    }

    #[test]
    fn a_devices_link_is_kept_only_when_it_is_a_web_address() {
        let said = |url: &str| {
            let mut update = UpdateState {
                release_url: Some(url.to_owned()),
                title: Some("x".repeat(400)),
                skipped_version: Some("2".into()),
                ..at("1", "2")
            };
            clean(&mut update);
            update
        };
        let kept = said("https://example.com/notes");
        assert_eq!(
            kept.release_url.as_deref(),
            Some("https://example.com/notes")
        );
        assert_eq!(kept.title.map(|title| title.len()), Some(TEXT_MAX));
        assert_eq!(kept.skipped_version, None);
        assert!(kept.available, "and so it's a report the core accepts");
        for not_a_web_address in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            " javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "https://example.com/ onclick=x",
        ] {
            assert_eq!(
                said(not_a_web_address).release_url,
                None,
                "{not_a_web_address}"
            );
        }
    }

    #[test]
    fn a_protocol_can_say_why_it_failed_but_not_what_was_skipped() {
        let reported = UpdateState {
            skipped_version: Some("9".into()),
            last_error: Some("The device didn't answer.".into()),
            ..at("1", "2")
        };
        let folded = fold(None, reported, &mut None);
        assert_eq!(folded.skipped_version, None);
        assert_eq!(
            folded.last_error.as_deref(),
            Some("The device didn't answer.")
        );
        assert!(folded.available);
    }
}
