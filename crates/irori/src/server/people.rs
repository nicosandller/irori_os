//! `/api/users` and `/api/place`: the people allowed in, and where the home is.
//!
//! Both are Settings rows, and both write files a person may also edit by hand (`users.toml`,
//! `home.toml`), so every rule here is one the files are held to as well.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use irori_types::{AreaId, HomeSettings, Location, Name, Role, TimeZoneName, User, UserId};
use serde::{Deserialize, Serialize};

use super::auth::{
    Actor, Client, NEEDS_PASSWORD, check_current, edit_failed, fingerprint, password_hash,
    session_cookie, token_of,
};
use super::trail::Trail;
use super::{AppState, refused};
use crate::config::{People, Refused};

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

/// Everybody there is, or, for a person who has only some rooms, themselves: who else lives in
/// the home is part of the home they have no room for (`docs/specs/access.md` §2).
pub async fn users(
    State(state): State<AppState>,
    Extension(who): Extension<Actor>,
    Extension(reach): Extension<irori_core::Reach>,
) -> Response {
    Json(narrowed(list(&state).await, &who, &reach)).into_response()
}

/// `everybody` as `who` is shown it: for a person who has only some rooms, themselves alone.
/// Every answer that lists people goes through here.
fn narrowed(mut everybody: Vec<UserView>, who: &Actor, reach: &irori_core::Reach) -> Vec<UserView> {
    if !reach.is_whole() {
        let me = who.user.as_ref().map(|user| &user.id);
        everybody.retain(|view| Some(&view.user.id) == me);
    }
    everybody
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewUser {
    name: Name,
    #[serde(default)]
    role: Role,
    #[serde(default)]
    password: Option<String>,
    /// The rooms they have. Left out: the whole home (`docs/specs/access.md` §1).
    #[serde(default)]
    areas: Option<Vec<AreaId>>,
}

impl std::fmt::Debug for NewUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewUser")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("areas", &self.areas)
            .finish()
    }
}

/// A list of rooms as it would be kept: each once, in the order given. Refused when it names a
/// room that isn't there (a typo shouldn't leave somebody with nothing and no word about it), or
/// when it is given to somebody who has the whole home because they run it.
fn rooms_to_keep(state: &AppState, asked: Vec<AreaId>, role: Role) -> Result<Vec<AreaId>, String> {
    if role.runs_the_home() {
        return Err("an owner has the whole home: rooms are for somebody who isn't one".to_owned());
    }
    // whole-home: an owner sets rooms, checked against the home's rooms
    let have = state.0.core.areas();
    let mut kept: Vec<AreaId> = Vec::new();
    for area in asked {
        if !have.iter().any(|known| known.id == area) {
            return Err(format!("there's no room `{area}`"));
        }
        if !kept.contains(&area) {
            kept.push(area);
        }
    }
    Ok(kept)
}

/// "owner" or "user", for the activity log.
fn role_word(role: Role) -> &'static str {
    if role.runs_the_home() {
        "owner"
    } else {
        "user"
    }
}

/// Which rooms, for the activity log: the ids, or the whole home.
fn rooms_words(areas: Option<&[AreaId]>) -> String {
    match areas {
        None => "the whole home".to_owned(),
        Some([]) => "no rooms".to_owned(),
        Some(areas) => areas
            .iter()
            .map(AreaId::as_str)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// What adding somebody to a home with no owner yet is told.
const NEEDS_OWNER: &str = "set the home up first: it needs an owner with a password before \
     anybody else is let in";

/// Adds a person, with a password of their own. The owner comes first, through the welcome
/// (`auth::set_up`); this is for everybody after.
pub async fn add_user(
    State(state): State<AppState>,
    Extension(trail): Extension<Trail>,
    Json(ask): Json<NewUser>,
) -> Response {
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
    let areas = match ask.areas.clone() {
        Some(asked) => match rooms_to_keep(&state, asked, ask.role) {
            Ok(kept) => Some(kept),
            Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
        },
        None => None,
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
                areas: areas.clone(),
            });
            people.users.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(id)
        })
        .await;
    match made {
        Ok(id) => {
            tracing::info!(user = %id, "somebody was added to the home");
            trail.target(id.as_str());
            trail.detail(format!(
                "{} ({}), with {}",
                ask.name.as_str(),
                role_word(ask.role),
                rooms_words(areas.as_deref())
            ));
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
    /// Their rooms: a list sets them, `null` gives back the whole home, and leaving it out
    /// changes nothing.
    #[serde(default, deserialize_with = "set_or_cleared")]
    areas: Option<Option<Vec<AreaId>>>,
}

/// A field that can be left out (change nothing), `null` (clear it) or given (set it).
fn set_or_cleared<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl std::fmt::Debug for UserEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserEdit")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("areas", &self.areas)
            .field("password", &self.password.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Changes a person. Anyone may change their own name and password; the rest is the owner's.
pub async fn edit_user(
    State(state): State<AppState>,
    Extension(who): Extension<Actor>,
    Extension(Client(from)): Extension<Client>,
    Extension(trail): Extension<Trail>,
    Path(id): Path<UserId>,
    headers: HeaderMap,
    Json(ask): Json<UserEdit>,
) -> Response {
    let reach = who.reach(&state.0.core);
    let own = who.user.as_ref().is_some_and(|user| user.id == id);
    if !who.owner && (!own || ask.role.is_some() || ask.areas.is_some()) {
        return refused(
            StatusCode::FORBIDDEN,
            "only an owner can change somebody else, or what anybody may do".to_owned(),
        );
    }
    // A name that is taken would say that somebody has it, and a person with only some rooms is
    // not shown who else lives in the home. Their name is changed for them.
    if !who.owner && !reach.is_whole() && ask.name.is_some() {
        return refused(
            StatusCode::FORBIDDEN,
            "an owner changes the name of a person who has only some rooms".to_owned(),
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
    // Only an owner makes tokens (`docs/specs/api.md` §2), so an owner made a regular user takes
    // theirs with them. The same refusals the write would give come first, as when removing
    // somebody: a last owner told to step down mustn't lose their tokens and then stay.
    let people = state.0.config.people().await;
    let demotes = ask.role == Some(Role::User)
        && people
            .user(&id)
            .is_some_and(|user| user.role.runs_the_home());
    // Everything that can still say no, before anything is taken away: an edit that is refused
    // leaves the person's tokens where they were.
    if clears {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a password can be changed, not taken away: everybody in the home has one".to_owned(),
        );
    }
    if people.user(&id).is_none() {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("there's nobody with the id `{id}`"),
        );
    }
    if let Some(name) = &ask.name
        && people
            .users
            .iter()
            .any(|user| user.id != id && same_name(&user.name, name))
    {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("there's already somebody called {}", name.as_str()),
        );
    }
    // The rooms to set, checked against the home: `Some(None)` is the whole home again.
    let rooms = match ask.areas.clone() {
        Some(Some(asked)) => {
            // Whether they run the home once this edit is done.
            let role = ask
                .role
                .or_else(|| people.user(&id).map(|user| user.role))
                .unwrap_or_default();
            match rooms_to_keep(&state, asked, role) {
                Ok(kept) => Some(Some(kept)),
                Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
            }
        }
        Some(None) => Some(None),
        None => None,
    };
    if demotes {
        if let Err(why) = can_demote(&people, &id) {
            return refused(StatusCode::UNPROCESSABLE_ENTITY, why);
        }
        if let Err(why) = state.0.auth.tokens.revoke_user(&id) {
            return refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("couldn't write it down: {why}"),
            );
        }
        state.0.sockets.close_user_tokens(&id);
    }
    // What was changed, in words, for the activity log. A password is said to have changed and
    // nothing more.
    let mut changed = Vec::new();
    if let Some(name) = &ask.name {
        changed.push(format!("name to {}", name.as_str()));
    }
    if let Some(role) = ask.role {
        changed.push(format!("role to {}", role_word(role)));
    }
    if let Some(rooms) = &rooms {
        changed.push(format!("rooms to {}", rooms_words(rooms.as_deref())));
    }
    if changes_password {
        changed.push("password".to_owned());
    }
    trail.detail(changed.join("; "));
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
                    if let Some(rooms) = &rooms {
                        user.areas = rooms.clone();
                    }
                    // An owner has the whole home, so being made one gives the rooms back.
                    if user.role.runs_the_home() {
                        user.areas = None;
                    }
                }
            }
            // The last owner stepping down is caught by the check every edit ends with.
            Ok(())
        })
        .await;
    match edited {
        Ok(()) => {
            // A page open as them was shown the home as it was for their old rooms or role.
            // Closed once the change is written, so the page that reconnects is served the new.
            if ask.areas.is_some() || ask.role.is_some() {
                state.0.sockets.close_user_sessions(&id, None);
                forget_chats(&state, &id);
            }
            if changes_password {
                // Every other screen signed in as them is signed out; this one stays.
                // Its socket stays too. Tokens are not sessions: a password doesn't end them.
                let kept = token_of(&headers).map(fingerprint);
                state.0.auth.end_all(&id, token_of(&headers));
                state.0.sockets.close_user_sessions(&id, kept.as_deref());
                tracing::info!(user = %id, "a password was changed");
            }
            let mut response = Json(narrowed(list(&state).await, &who, &reach)).into_response();
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
pub async fn remove_user(
    State(state): State<AppState>,
    Extension(trail): Extension<Trail>,
    Path(id): Path<UserId>,
) -> Response {
    let people = state.0.config.people().await;
    // The name they had: the id can be given to somebody else later.
    if let Some(user) = people.user(&id) {
        trail.detail(user.name.as_str());
    }
    // The same refusals the write would give, before any token is forgotten. Revoking
    // first and then being told the person has to stay would take their tokens anyway.
    if let Err(why) = can_leave(&people, &id) {
        return refused(StatusCode::UNPROCESSABLE_ENTITY, why);
    }
    if let Err(why) = state.0.auth.tokens.revoke_user(&id) {
        return refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write it down: {why}"),
        );
    }
    state.0.sockets.close_user_tokens(&id);
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
            state.0.sockets.close_user_sessions(&id, None);
            forget_chats(&state, &id);
            tracing::info!(user = %id, "somebody was removed from the home");
            Json(list(&state).await).into_response()
        }
        Err(error) => edit_failed(error),
    }
}

/// What the assistant said to a person with rooms was drawn from the rooms they had then, so it
/// goes when their rooms change, or they do. Only what is kept for them alone: the chats
/// everybody with the whole home shares are not theirs to take away.
fn forget_chats(state: &AppState, id: &UserId) {
    #[cfg(feature = "assist")]
    if let Err(error) = crate::assistant::forget_conversations_of(&state.0.db.path, id) {
        tracing::warn!(user = %id, "couldn't forget a person's chats: {error}");
    }
    #[cfg(not(feature = "assist"))]
    let _ = (state, id);
}

/// Whether `id` can leave, in the same words the write would use. Nothing is changed.
fn can_leave(people: &People, id: &UserId) -> Result<(), String> {
    if people.user(id).is_none() {
        return Err(format!("there's nobody with the id `{id}`"));
    }
    let mut left = people.clone();
    left.users.retain(|user| &user.id != id);
    left.hashes.remove(id);
    still_run(&left)
}

/// Whether `id` can stop being an owner, in the same words the write would use.
fn can_demote(people: &People, id: &UserId) -> Result<(), String> {
    let mut left = people.clone();
    for user in &mut left.users {
        if &user.id == id {
            user.role = Role::User;
        }
    }
    still_run(&left)
}

/// Whether a home as it would be left still has somebody who can run it.
fn still_run(left: &People) -> Result<(), String> {
    irori_types::check_users(&left.users).map_err(|error| error.to_string())?;
    let has = |user: &User| left.hashes.contains_key(&user.id);
    let owner_can_sign_in = left
        .users
        .iter()
        .any(|user| user.role.runs_the_home() && has(user));
    if left.users.iter().any(has) && !owner_can_sign_in {
        return Err(
            "an owner needs a password before anybody else has one: otherwise nobody \
             could sign in to run the home"
                .to_owned(),
        );
    }
    Ok(())
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
