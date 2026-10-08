//! `/api/dev/users` and `/api/dev/place`: the people allowed in, and where the home is.
//!
//! Both are Settings rows, and both write files a person may also edit by hand (`users.toml`,
//! `home.toml`), so every rule here is one the files are held to as well.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use irori_types::{HomeSettings, Location, Name, Role, TimeZoneName, User, UserId};
use serde::{Deserialize, Serialize};

use super::auth::{
    Actor, Client, NEEDS_PASSWORD, check_current, edit_failed, password_hash, session_cookie,
    token_of,
};
use super::{AppState, refused};
use crate::config::Refused;

/// One person, as the page is shown them. Never their password, hashed or not.
#[derive(Debug, Serialize)]
struct UserView {
    #[serde(flatten)]
    user: User,
    has_password: bool,
}

async fn list(state: &AppState) -> Vec<UserView> {
    let people = state.0.config.people().await;
    people
        .users
        .iter()
        .map(|user| UserView {
            has_password: people.hashes.contains_key(&user.id),
            user: user.clone(),
        })
        .collect()
}

pub async fn users(State(state): State<AppState>) -> Response {
    Json(list(&state).await).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewUser {
    name: Name,
    #[serde(default)]
    role: Role,
    #[serde(default)]
    password: Option<String>,
}

impl std::fmt::Debug for NewUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewUser")
            .field("name", &self.name)
            .field("role", &self.role)
            .finish()
    }
}

/// What adding somebody to a home with no owner yet is told.
const NEEDS_OWNER: &str = "set the home up first: it needs an owner with a password before \
     anybody else is let in";

/// Adds a person, with a password of their own. The owner comes first, through the welcome
/// (`auth::set_up`); this is for everybody after.
pub async fn add_user(State(state): State<AppState>, Json(ask): Json<NewUser>) -> Response {
    let password = ask.password.filter(|password| !password.is_empty());
    let hash = match password_hash(password).await {
        Ok(hash) => hash,
        Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
    };
    let Some(base) = irori_types::user_id_from(ask.name.as_str()) else {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "that name has no letters or numbers to make an id from".to_owned(),
        );
    };
    let made = state
        .0
        .config
        .edit_people(&state.0.core, |people| {
            let role = ask.role;
            if !people.locked() {
                return Err(Refused(NEEDS_OWNER.to_owned()));
            }
            if hash.is_none() {
                return Err(Refused(NEEDS_PASSWORD.to_owned()));
            }
            if people
                .users
                .iter()
                .any(|user| same_name(&user.name, &ask.name))
            {
                return Err(Refused(format!(
                    "there's already somebody called {}",
                    ask.name.as_str()
                )));
            }
            // Two people whose names make the same id: the second is numbered.
            let id = (1..100)
                .map(|n| match n {
                    1 => base.clone(),
                    n => UserId::try_from(format!("{base}_{n}")).unwrap_or_else(|_| base.clone()),
                })
                .find(|id| people.user(id).is_none())
                .ok_or_else(|| Refused("too many people share that name".to_owned()))?;
            if let Some(hash) = hash {
                people.hashes.insert(id.clone(), hash);
            }
            people.users.push(User {
                id: id.clone(),
                name: ask.name.clone(),
                role,
            });
            people.users.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(id)
        })
        .await;
    match made {
        Ok(id) => {
            tracing::info!(user = %id, "somebody was added to the home");
            (StatusCode::CREATED, Json(list(&state).await)).into_response()
        }
        Err(error) => edit_failed(error),
    }
}

fn same_name(a: &Name, b: &Name) -> bool {
    a.as_str().trim().to_lowercase() == b.as_str().trim().to_lowercase()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserEdit {
    #[serde(default)]
    name: Option<Name>,
    #[serde(default)]
    role: Option<Role>,
    /// A new password. An empty one is refused: a password is changed, never taken away.
    #[serde(default)]
    password: Option<String>,
    /// The password they have now. Needed to change or take away your own.
    #[serde(default)]
    current_password: Option<String>,
}

impl std::fmt::Debug for UserEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserEdit")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("password", &self.password.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Changes a person. Anyone may change their own name and password; the rest is the owner's.
pub async fn edit_user(
    State(state): State<AppState>,
    Extension(who): Extension<Actor>,
    Extension(Client(from)): Extension<Client>,
    Path(id): Path<UserId>,
    headers: HeaderMap,
    Json(ask): Json<UserEdit>,
) -> Response {
    let own = who.user.as_ref().is_some_and(|user| user.id == id);
    if !who.owner && (!own || ask.role.is_some()) {
        return refused(
            StatusCode::FORBIDDEN,
            "only an owner can change somebody else, or what anybody may do".to_owned(),
        );
    }
    let clears = ask.password.as_deref() == Some("");
    // Your own password is changed with the one you have: a screen left signed in mustn't be
    // enough to take the home over. An owner resetting somebody else's doesn't know theirs.
    if own && ask.password.is_some() {
        let current = state.0.config.people().await.hashes.get(&id).cloned();
        if let Some(current) = current {
            let trier = (Some(id.clone()), from);
            if let Some(no) =
                check_current(&state.0.auth, &trier, ask.current_password.clone(), current).await
            {
                return no;
            }
        }
    }
    let hash = match password_hash(ask.password.filter(|password| !password.is_empty())).await {
        Ok(hash) => hash,
        Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
    };
    let changes_password = clears || hash.is_some();
    // Whether this is the password that makes the home start asking who is there.
    let locks = hash.is_some() && !state.0.config.people().await.locked();
    let edited = state
        .0
        .config
        .edit_people(&state.0.core, |people| {
            if people.user(&id).is_none() {
                return Err(Refused(format!("there's nobody with the id `{id}`")));
            }
            if let Some(name) = &ask.name
                && people
                    .users
                    .iter()
                    .any(|user| user.id != id && same_name(&user.name, name))
            {
                return Err(Refused(format!(
                    "there's already somebody called {}",
                    name.as_str()
                )));
            }
            if clears {
                return Err(Refused(
                    "a password can be changed, not taken away: everybody in the home has one"
                        .to_owned(),
                ));
            }
            if let Some(hash) = hash {
                people.hashes.insert(id.clone(), hash);
            }
            for user in &mut people.users {
                if user.id == id {
                    if let Some(name) = &ask.name {
                        user.name = name.clone();
                    }
                    if let Some(role) = ask.role {
                        user.role = role;
                    }
                }
            }
            // The last owner stepping down is caught by the check every edit ends with.
            Ok(())
        })
        .await;
    match edited {
        Ok(()) => {
            if changes_password {
                // Every other screen signed in as them is signed out; this one stays.
                state.0.auth.end_all(&id, token_of(&headers));
                tracing::info!(user = %id, "a password was changed");
            }
            let mut response = Json(list(&state).await).into_response();
            if locks {
                // The home asks who is there from here on. Whoever just set the password is
                // signed in as its owner, rather than shown the door they just fitted.
                let token = state.0.auth.begin(&id);
                response
                    .headers_mut()
                    .insert(axum::http::header::SET_COOKIE, session_cookie(&token));
            }
            response
        }
        Err(error) => edit_failed(error),
    }
}

/// Takes a person out of the home, and signs them out everywhere.
pub async fn remove_user(State(state): State<AppState>, Path(id): Path<UserId>) -> Response {
    let removed = state
        .0
        .config
        .edit_people(&state.0.core, |people| {
            if people.user(&id).is_none() {
                return Err(Refused(format!("there's nobody with the id `{id}`")));
            }
            people.users.retain(|user| user.id != id);
            people.hashes.remove(&id);
            Ok(())
        })
        .await;
    match removed {
        Ok(()) => {
            state.0.auth.end_all(&id, None);
            tracing::info!(user = %id, "somebody was removed from the home");
            Json(list(&state).await).into_response()
        }
        Err(error) => edit_failed(error),
    }
}

pub async fn place(State(state): State<AppState>) -> Response {
    Json(state.0.config.home().await).into_response()
}

/// Where the home is, as the page sends it: all of it at once, like the plan.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceEdit {
    #[serde(default)]
    time_zone: Option<TimeZoneName>,
    #[serde(default)]
    location: Option<Location>,
}

/// Replaces where the home is and its time zone. A zone has to be one Irori can tell the time
/// in: one nobody has heard of would arm nothing and say nothing.
pub async fn set_place(State(state): State<AppState>, Json(ask): Json<PlaceEdit>) -> Response {
    if let Some(zone) = &ask.time_zone
        && jiff::tz::TimeZone::get(zone.as_str()).is_err()
    {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{zone} isn't a time zone Irori knows; one looks like Europe/Brussels"),
        );
    }
    // The zone is the part that's needed: a time of day means nothing without it, and the sun
    // is worked out in it. Where the home is on the map is the optional part.
    if ask.location.is_some() && ask.time_zone.is_none() {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a location needs a time zone with it; pick the home's time zone first".to_owned(),
        );
    }
    let home = HomeSettings {
        time_zone: ask.time_zone,
        location: ask.location,
    };
    let saved = state
        .0
        .config
        .edit_home(&state.0.core, |current| {
            *current = home.clone();
            Ok(())
        })
        .await;
    match saved {
        Ok(()) => Json(home).into_response(),
        Err(error) => edit_failed(error),
    }
}
