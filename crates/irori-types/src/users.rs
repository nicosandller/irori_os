//! The people allowed in (`users.toml`, `docs/specs/config.md` §3.10).
//!
//! Two kinds, and the difference is one sentence: an owner runs the home, a user uses it. What
//! each may do is decided here, in one function, so the server that refuses and the page that
//! hides are reading the same rule.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{InvariantError, Name, UserId};

/// The shortest password Irori takes.
pub const PASSWORD_MIN_LEN: usize = 8;

/// The longest: far past any passphrase, and short enough that hashing one can't be made into
/// work for the machine.
pub const PASSWORD_MAX_LEN: usize = 256;

/// What a person may do.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Runs the home: everything, including who else is let in.
    Owner,
    /// Uses the home: sees everything and controls devices.
    #[default]
    User,
}

impl Role {
    /// Whether this role may change how the home is set up.
    pub fn runs_the_home(self) -> bool {
        matches!(self, Role::Owner)
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::User => "user",
        }
    }
}

/// One person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct User {
    pub id: UserId,
    pub name: Name,
    pub role: Role,
}

/// Why a set of users can't stand, checked the same way for a hand-edited file and for the API.
pub fn check_users(users: &[User]) -> Result<(), InvariantError> {
    if !users.is_empty() && !users.iter().any(|user| user.role.runs_the_home()) {
        return Err(InvariantError::new(
            "a home with people in it needs an owner: give one of them `role = \"owner\"`",
        ));
    }
    Ok(())
}

/// Whether a password is one Irori takes, saying what's wrong with it if not.
pub fn check_password(password: &str) -> Result<(), InvariantError> {
    let length = password.chars().count();
    if length < PASSWORD_MIN_LEN {
        return Err(InvariantError::new(format!(
            "a password needs at least {PASSWORD_MIN_LEN} characters"
        )));
    }
    if length > PASSWORD_MAX_LEN {
        return Err(InvariantError::new(format!(
            "a password is at most {PASSWORD_MAX_LEN} characters"
        )));
    }
    Ok(())
}

/// An id made from a name: "Nico S." becomes `nico_s`. `None` when nothing in the name can be
/// part of an id.
pub fn user_id_from(name: &str) -> Option<UserId> {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug: String = slug.trim_end_matches('_').chars().take(48).collect();
    UserId::try_from(slug.trim_end_matches('_')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, role: Role) -> User {
        User {
            id: UserId::try_from(id).expect("a valid id"),
            name: Name::try_from(id).expect("a valid name"),
            role,
        }
    }

    #[test]
    fn nobody_is_fine_and_somebody_needs_an_owner() {
        assert!(check_users(&[]).is_ok());
        assert!(check_users(&[person("nico", Role::Owner)]).is_ok());
        let why = check_users(&[person("guest", Role::User)]).expect_err("no owner");
        assert!(why.to_string().contains("needs an owner"), "{why}");
    }

    #[test]
    fn a_password_has_a_floor_and_a_ceiling() {
        assert!(check_password("short").is_err());
        assert!(check_password("long enough").is_ok());
        assert!(check_password(&"x".repeat(PASSWORD_MAX_LEN + 1)).is_err());
    }

    #[test]
    fn an_id_is_made_from_a_name() {
        let id = |name| user_id_from(name).map(|id| id.as_str().to_owned());
        assert_eq!(id("Nico").as_deref(), Some("nico"));
        assert_eq!(id("  Nico S. ").as_deref(), Some("nico_s"));
        assert_eq!(id("María-José").as_deref(), Some("mar_a_jos"));
        assert_eq!(id("…"), None);
    }
}
