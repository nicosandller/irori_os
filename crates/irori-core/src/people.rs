//! Who is in the home and what each can reach, as the core is told (`docs/specs/access.md` §4).
//!
//! The core never reads `users.toml`: whoever does hands the people over, the way settings
//! are handed over, and the core tells engines when they changed. An engine that keeps a
//! person's flows asks here what that person can see and send now.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use irori_protocol::engine::PersonReach;
use irori_types::{AreaId, ExtensionId, UserId};

use crate::{Core, Event, read, write};

/// One person of the home, as `users.toml` says. Whether they have a password makes no
/// difference here: it is for the one who signs people in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: UserId,
    pub name: String,
    /// Whether they may change how the home is set up.
    pub owner: bool,
    /// Their rooms; `None` is the whole home.
    pub rooms: Option<Vec<AreaId>>,
}

/// Where an engine's remarks about the home go, for the activity log: a flow turned off
/// because the home changed under it. Given to the core by whoever keeps that log.
pub trait Notes: Send + Sync + fmt::Debug {
    fn note(&self, extension: &ExtensionId, action: &str, target: &str, detail: &str);
}

/// What an engine may say through [`Notes`]: the log keeps no other kind of remark from one.
pub const NOTE_ACTIONS: [&str; 2] = ["flow_turned_off", "flow_handed_over"];

impl Core {
    /// Replaces who is in the home. Engines are told when anyone's rooms or role changed:
    /// what a flow of theirs may touch is not the same as it was.
    pub fn apply_people(&self, people: Vec<Person>) {
        {
            let mut held = write(&self.0.people);
            if *held == people {
                return;
            }
            *held = people;
        }
        self.publish(vec![Event::SettingsChanged]);
    }

    /// The extensions whose page a regular user may open when their manifest says
    /// `users = true`, until permissions are approved one by one (ROADMAP C26): the official
    /// ones.
    pub fn trust_with_people(&self, extensions: impl IntoIterator<Item = ExtensionId>) {
        *write(&self.0.trusted_with_people) = extensions.into_iter().collect::<BTreeSet<_>>();
    }

    /// Whether this extension's page is for regular users too, and may ask who can reach what:
    /// its manifest says it limits what they can do, and the home trusts it to.
    pub fn limits_people(&self, extension: &ExtensionId) -> bool {
        read(&self.0.trusted_with_people).contains(extension)
            && read(&self.0.extensions)
                .get(extension)
                .and_then(|overview| overview.info.as_ref())
                .and_then(|info| info.app.as_ref())
                .is_some_and(|app| app.users)
    }

    /// Where engines' remarks about the home are written. Set it before starting extensions.
    pub fn use_notes(&self, notes: Arc<dyn Notes>) {
        *write(&self.0.notes) = Some(notes);
    }

    pub(crate) fn note(&self, extension: &ExtensionId, action: &str, target: &str, detail: &str) {
        if !NOTE_ACTIONS.contains(&action) {
            return;
        }
        if let Some(notes) = read(&self.0.notes).as_ref() {
            notes.note(extension, action, target, detail);
        }
    }

    /// Where one person stands now. Someone who is not in the home any more is `here: false`.
    pub fn person_reach(&self, user: &UserId) -> PersonReach {
        let person = read(&self.0.people)
            .iter()
            .find(|person| &person.id == user)
            .cloned();
        let Some(person) = person else {
            return PersonReach {
                here: false,
                name: String::new(),
                owner: false,
                entities: None,
            };
        };
        let entities = if person.owner {
            None
        } else {
            self.reach_for(person.rooms.as_deref()).entity_ids()
        };
        PersonReach {
            here: true,
            name: person.name,
            owner: person.owner,
            entities,
        }
    }
}
