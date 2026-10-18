//! The activity log's rows (`docs/specs/access.md` §3): who did what, to what, and whether it
//! was allowed. Shared with the command line and the page, which is compiled to wasm, so this
//! stays free of the server.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{ContextId, ExtensionId, Timestamp, TokenId, UserId};

/// What came of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It happened.
    Ok,
    /// Somebody was not allowed, or a thing was not theirs to ask for.
    Denied,
    /// Allowed, and it did not work: a refused value, a device that did not answer.
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Outcome {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "ok" => Ok(Self::Ok),
            "denied" => Ok(Self::Denied),
            "failed" => Ok(Self::Failed),
            other => Err(format!("`{other}` is ok, denied or failed")),
        }
    }
}

/// A person, as they were called when it happened. An id can be made again after a person is
/// removed, and a name can change, so the row keeps both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Named {
    pub id: UserId,
    pub name: String,
}

/// Who did it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditActor {
    /// Somebody signed in.
    Person { id: UserId, name: String },
    /// A program, by the token it presented. The token is gone from its table when it is
    /// revoked, so its name and the person it was made for are kept here.
    Token {
        id: TokenId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        person: Option<Named>,
    },
    /// An extension, on the socket it dials in on.
    Extension { id: ExtensionId },
    /// Irori itself: a flow turned off because the home changed under it.
    Irori,
    /// Nobody who could be told: a wrong password, or a request with no sign-in.
    Nobody,
}

impl AuditActor {
    /// The person this is, or the person a token was made for.
    pub fn person(&self) -> Option<&UserId> {
        match self {
            Self::Person { id, .. } => Some(id),
            Self::Token { person, .. } => person.as_ref().map(|person| &person.id),
            _ => None,
        }
    }
}

impl fmt::Display for AuditActor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Person { name, .. } => f.write_str(name),
            Self::Token {
                name,
                person: Some(person),
                ..
            } => write!(f, "token “{name}” (for {})", person.name),
            Self::Token { name, .. } => write!(f, "token “{name}”"),
            Self::Extension { id } => write!(f, "extension {id}"),
            Self::Irori => f.write_str("Irori"),
            Self::Nobody => f.write_str("nobody signed in"),
        }
    }
}

/// One row of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Counts up for ever, so a page of rows can say where it left off even after the oldest
    /// have gone.
    pub id: u64,
    pub at: Timestamp,
    pub actor: AuditActor,
    /// What was done, as a short name that does not change (`sign_in`, `command`,
    /// `person_added`…).
    pub action: String,
    /// What it was done to: an entity, a person, an extension, a device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub outcome: Outcome,
    /// A few words more. Never a password, a key, or a lock's code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// For a command: the context of the call. The state change that follows carries it as its
    /// `parent_id`, which is how an entity's history can say who turned it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    /// Where the request came from, when that is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

/// What `GET /api/audit` answers: the newest rows first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    /// Ask again with `before` set to this to read the rows older than these. Absent: these
    /// were the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older: Option<u64>,
}

/// The most rows one answer holds.
pub const MOST_ROWS: usize = 1000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_reads_as_it_is_written() {
        for outcome in [Outcome::Ok, Outcome::Denied, Outcome::Failed] {
            assert_eq!(outcome.as_str().parse::<Outcome>(), Ok(outcome));
        }
        assert!("allowed".parse::<Outcome>().is_err());
    }

    #[test]
    fn an_actor_reads_as_a_person_would_say_it() {
        let leo = AuditActor::Person {
            id: UserId::try_from("leo").expect("valid"),
            name: "Leo".to_owned(),
        };
        assert_eq!(leo.to_string(), "Leo");
        let tablet = AuditActor::Token {
            id: TokenId::try_from("tablet").expect("valid"),
            name: "Tablet".to_owned(),
            person: Some(Named {
                id: UserId::try_from("nico").expect("valid"),
                name: "Nico".to_owned(),
            }),
        };
        assert_eq!(tablet.to_string(), "token “Tablet” (for Nico)");
        assert_eq!(tablet.person().map(UserId::as_str), Some("nico"));
        assert_eq!(AuditActor::Nobody.person(), None);
    }

    #[test]
    fn a_row_round_trips() {
        let row = AuditEntry {
            id: 7,
            at: Timestamp::from_second(1_700_000_000).expect("in range"),
            actor: AuditActor::Irori,
            action: "flow_turned_off".to_owned(),
            target: Some("porch".to_owned()),
            outcome: Outcome::Ok,
            detail: None,
            context_id: None,
            client: None,
        };
        let text = serde_json::to_string(&row).expect("writes");
        assert!(!text.contains("detail"), "{text}");
        assert_eq!(
            serde_json::from_str::<AuditEntry>(&text).expect("reads"),
            row
        );
    }
}
