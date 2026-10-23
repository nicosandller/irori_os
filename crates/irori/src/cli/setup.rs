//! Sign-in, tokens, people, the place, history settings, logs, and restart.

use irori_client::{Health, Recorder, System};
use irori_types::{ApiScope, Role};
use serde_json::json;

use super::args::{AuditCmd, LoginArgs, PlaceCmd, RecorderCmd, Remote, TokenCmd, UsersCmd};
use super::connect::{self, seg};
use super::output::{self, explain};
use super::prompt;

fn err(error: irori_client::Error) -> String {
    explain(&error)
}

pub async fn login(args: LoginArgs) -> Result<(), String> {
    let (client, json, _) = connect::open(&args.remote, false)?;
    // Said before the password is asked for: the address can come from a `cli.url` in
    // whatever directory this was run in.
    eprintln!("signing in to {}", client.origin());
    let password = prompt::password("Password: ", args.password_stdin)?;
    let (session, cookie) = client.sign_in(&args.user, &password).await.map_err(err)?;
    connect::save_session(client.origin(), &cookie)?;
    let who = session
        .user
        .as_ref()
        .map(|user| format!("{} ({})", user.name.as_str(), user.role.label()))
        .unwrap_or_else(|| args.user.clone());
    output::show(json, &session, || {
        println!("signed in as {who}");
        if !session.owner {
            println!("this session can't change how the home is set up");
        }
    })
}

pub async fn logout(remote: &Remote) -> Result<(), String> {
    let json = remote.json;
    let saved = connect::forget_session()?;
    if let Some(saved) = saved.filter(|saved| !saved.cookie.is_empty()) {
        // The session is forgotten here either way. Irori still has it until it is told,
        // so a server that couldn't be told is said, not passed over. A session it no
        // longer knows (401) is already signed out.
        let told = match connect::with_session(remote, &saved.origin, saved.cookie) {
            Ok(client) => client
                .empty(reqwest::Method::DELETE, "/api/session", None::<&()>)
                .await
                .or_else(|error| match error.status {
                    Some(401) => Ok(()),
                    _ => Err(explain(&error)),
                }),
            Err(error) => Err(error),
        };
        if let Err(error) = told {
            eprintln!(
                "forgot the session on this machine, but couldn't sign it out of {}: {error}",
                saved.origin
            );
        }
    }
    output::done(json, "signed out")
}

pub async fn status(remote: &Remote) -> Result<(), String> {
    let link = connect::connect(remote)?;
    let health: Health = link.client.get("/api/health").await.map_err(err)?;
    let system = link.client.get::<System>("/api/system").await;
    if link.json {
        let system = match system {
            Ok(system) => serde_json::to_value(system).map_err(|error| error.to_string())?,
            Err(error) => json!({ "error": explain(&error) }),
        };
        return output::json_out(&json!({ "health": health, "system": system }));
    }
    println!(
        "irori {}\t{}\tup {}s\t{}",
        health.version,
        health.commit,
        health.uptime_ms / 1000,
        health.status
    );
    match system {
        Ok(system) => {
            let host = system.host.as_deref().unwrap_or("this machine");
            println!(
                "{host}\t{} {}\t{}\tdata {}",
                system.os, system.os_version, system.arch, system.data_dir
            );
        }
        Err(error) => eprintln!("system: {}", explain(&error)),
    }
    Ok(())
}

pub async fn token(remote: &Remote, cmd: TokenCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        TokenCmd::List => {
            let tokens = link
                .client
                .get::<Vec<irori_client::TokenRow>>("/api/tokens")
                .await
                .map_err(err)?;
            output::show(link.json, &tokens, || {
                for token in &tokens {
                    let may = if let Some(extension) = &token.extension {
                        format!("extension {extension}")
                    } else {
                        token
                            .scopes
                            .iter()
                            .map(|scope| scope_name(*scope))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    println!("{}\t{}\t{may}", token.id, token.name);
                }
            })
        }
        TokenCmd::Create {
            name,
            scopes,
            extension,
        } => {
            if extension.is_some() && !scopes.is_empty() {
                return Err("an extension token has no scopes".to_owned());
            }
            if extension.is_none() && scopes.is_empty() {
                return Err(
                    "pass --scope, or --extension for a token that only connects".to_owned(),
                );
            }
            let scopes = scopes
                .iter()
                .map(|scope| parse_scope(scope))
                .collect::<Result<Vec<_>, _>>()?;
            let mut body = json!({ "name": name, "scopes": scopes });
            if let Some(extension) = &extension {
                body["extension"] = json!(extension);
                body["scopes"] = json!([]);
            }
            let created = link
                .client
                .post::<irori_client::CreatedToken>("/api/tokens", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &created, || {
                println!("{}\t{}", created.token.id, created.secret);
                println!("this secret is shown once");
            })
        }
        TokenCmd::Revoke { id } => {
            if !prompt::confirm(
                &format!("Revoke {id}? The program using it can no longer reach the home."),
                link.yes,
                link.json,
            )? {
                return output::done(link.json, "left the token");
            }
            link.client
                .empty(
                    reqwest::Method::DELETE,
                    &format!("/api/tokens/{}", seg(&id)?),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("revoked {id}"))
        }
    }
}

fn parse_scope(text: &str) -> Result<ApiScope, String> {
    match text {
        "registry:read" => Ok(ApiScope::RegistryRead),
        "states:read" => Ok(ApiScope::StatesRead),
        "services:call" => Ok(ApiScope::ServicesCall),
        "history:read" => Ok(ApiScope::HistoryRead),
        "events:read" => Ok(ApiScope::EventsRead),
        other => Err(format!(
            "`{other}` isn't a scope. They are registry:read, states:read, services:call, history:read, events:read."
        )),
    }
}

fn scope_name(scope: ApiScope) -> &'static str {
    match scope {
        ApiScope::RegistryRead => "registry:read",
        ApiScope::StatesRead => "states:read",
        ApiScope::ServicesCall => "services:call",
        ApiScope::HistoryRead => "history:read",
        ApiScope::EventsRead => "events:read",
    }
}

pub async fn users(remote: &Remote, cmd: UsersCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        UsersCmd::List => {
            let users = link
                .client
                .get::<Vec<irori_client::UserRow>>("/api/users")
                .await
                .map_err(err)?;
            output::show(link.json, &users, || {
                for user in &users {
                    println!(
                        "{}\t{}\t{}\t{}",
                        user.user.id,
                        user.user.name.as_str(),
                        user.user.role.label(),
                        rooms_of(&user.user)
                    );
                }
            })
        }
        UsersCmd::Add {
            name,
            role,
            areas,
            no_rooms,
            password_stdin,
        } => {
            let role = parse_role(&role)?;
            let password = prompt::password("Password: ", password_stdin)?;
            let mut body = json!({ "name": name, "role": role, "password": password });
            if no_rooms {
                body["areas"] = json!([]);
            } else if !areas.is_empty() {
                body["areas"] = json!(areas);
            }
            let users = link
                .client
                .post::<Vec<irori_client::UserRow>>("/api/users", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &users, || println!("added {name}"))
        }
        UsersCmd::Edit {
            id,
            name,
            role,
            areas,
            no_rooms,
            whole_home,
            password_stdin,
            current_password_stdin,
        } => {
            let mut body = json!({});
            if let Some(name) = name {
                body["name"] = json!(name);
            }
            if let Some(role) = role {
                body["role"] = json!(parse_role(&role)?);
            }
            if whole_home {
                body["areas"] = serde_json::Value::Null;
            } else if no_rooms {
                body["areas"] = json!([]);
            } else if !areas.is_empty() {
                body["areas"] = json!(areas);
            }
            if password_stdin {
                body["password"] = json!(prompt::password("New password: ", true)?);
            }
            if current_password_stdin {
                body["current_password"] = json!(prompt::password("Current password: ", true)?);
            }
            if body.as_object().is_some_and(|object| object.is_empty()) {
                return Err(
                    "pass --name, --role, --areas, --no-rooms, --whole-home or --password-stdin"
                        .to_owned(),
                );
            }
            let users = link
                .client
                .patch::<Vec<irori_client::UserRow>>(&format!("/api/users/{}", seg(&id)?), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &users, || println!("edited {id}"))
        }
        UsersCmd::Remove { id } => {
            if !prompt::confirm(
                &format!("Remove {id}? Their tokens go with them."),
                link.yes,
                link.json,
            )? {
                return output::done(link.json, "left them");
            }
            link.client
                .empty(
                    reqwest::Method::DELETE,
                    &format!("/api/users/{}", seg(&id)?),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("removed {id}"))
        }
    }
}

/// A person's rooms, as one cell of the list: the whole home, none, or the ids.
fn rooms_of(user: &irori_types::User) -> String {
    match &user.areas {
        None => "whole home".to_owned(),
        Some(rooms) if rooms.is_empty() => "no rooms".to_owned(),
        Some(rooms) => rooms
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
    }
}

fn parse_role(text: &str) -> Result<Role, String> {
    match text {
        "owner" => Ok(Role::Owner),
        "user" => Ok(Role::User),
        other => Err(format!("`{other}` is owner or user")),
    }
}

pub async fn place(remote: &Remote, cmd: PlaceCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        PlaceCmd::Show => {
            let home = link
                .client
                .get::<irori_types::HomeSettings>("/api/place")
                .await
                .map_err(err)?;
            output::show(link.json, &home, || {
                let zone = home
                    .time_zone
                    .as_ref()
                    .map(|zone| zone.as_str())
                    .unwrap_or("no time zone");
                println!("{zone}");
                if let Some(location) = &home.location {
                    println!("{}\t{}", location.latitude, location.longitude);
                }
            })
        }
        PlaceCmd::Set {
            label,
            latitude,
            longitude,
            timezone,
        } => {
            let mut location = json!({ "latitude": latitude, "longitude": longitude });
            if let Some(label) = label {
                location["label"] = json!(label);
            }
            let body = json!({
                "location": location,
                "time_zone": timezone,
            });
            let home = link
                .client
                .put::<irori_types::HomeSettings>("/api/place", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &home, || println!("saved the place"))
        }
    }
}

pub async fn recorder(remote: &Remote, cmd: RecorderCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        RecorderCmd::Show => {
            let settings: Recorder = link.client.get("/api/recorder").await.map_err(err)?;
            output::show(link.json, &settings, || print_recorder(&settings))
        }
        RecorderCmd::Set {
            retain_days,
            summary_days,
        } => {
            let body = Recorder {
                retain_days,
                summary_days,
            };
            let settings = link
                .client
                .put::<Recorder>("/api/recorder", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &settings, || print_recorder(&settings))
        }
    }
}

fn print_recorder(settings: &Recorder) {
    match settings.summary_days {
        Some(days) => println!(
            "detailed {} days, then hourly summaries for {days} days",
            settings.retain_days
        ),
        None => println!(
            "detailed {} days, then hourly summaries",
            settings.retain_days
        ),
    }
}

pub async fn logs(remote: &Remote) -> Result<(), String> {
    let link = connect::connect(remote)?;
    let body = link
        .client
        .get::<irori_client::Lines>("/api/system/log")
        .await
        .map_err(err)?;
    if link.json {
        output::json_out(&body.lines)
    } else {
        for line in &body.lines {
            println!("{line}");
        }
        Ok(())
    }
}

/// A time, or how far back from `now`: `12h`, `7d`, `30m`.
fn since_from(text: &str, now: jiff::Timestamp) -> Result<irori_types::Timestamp, String> {
    let text = text.trim();
    let span = text.char_indices().last().and_then(|(at, unit)| {
        let n = text[..at].parse::<i64>().ok()?;
        let seconds = match unit {
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        n.checked_mul(seconds)
    });
    match span {
        Some(seconds) => now
            .checked_sub(jiff::SignedDuration::from_secs(seconds))
            .map(irori_types::Timestamp::from_jiff)
            .map_err(|_| format!("`{text}` reaches back further than there is a clock for")),
        None => text.parse::<irori_types::Timestamp>().map_err(|_| {
            format!("`{text}` isn't a time like 2026-10-19T08:00:00Z, or a span like 12h or 7d")
        }),
    }
}

pub async fn audit(remote: &Remote, cmd: AuditCmd) -> Result<(), String> {
    let AuditCmd::List {
        user,
        kind,
        outcome,
        since,
        limit,
    } = cmd;
    let link = connect::connect(remote)?;
    let mut query = vec![format!("limit={limit}")];
    for (name, value) in [("user", user), ("action", kind), ("outcome", outcome)] {
        if let Some(value) = value {
            query.push(format!("{name}={}", connect::encode(&value)));
        }
    }
    if let Some(since) = since {
        let at = since_from(&since, jiff::Timestamp::now())?;
        query.push(format!("since={}", connect::encode(&at.to_string())));
    }
    let page = link
        .client
        .get::<irori_types::AuditPage>(&format!("/api/audit?{}", query.join("&")))
        .await
        .map_err(err)?;
    output::show(link.json, &page, || {
        for row in &page.entries {
            // One row a line, whatever was typed into it: rows written before the log cleaned
            // them may hold line breaks, tabs and escape sequences.
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                row.at,
                row.outcome,
                plain(&row.actor.to_string()),
                plain(&row.action),
                plain(row.target.as_deref().unwrap_or("-")),
                plain(row.detail.as_deref().unwrap_or(""))
            );
        }
        if page.older.is_some() {
            eprintln!("(older rows exist: raise --limit, or narrow it)");
        }
    })
}

/// `text` with each control character (a line break, a tab, an escape) shown as a space.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub async fn restart(remote: &Remote) -> Result<(), String> {
    let link = connect::connect(remote)?;
    if !prompt::confirm(
        "Restart Irori? It stays where it runs while it starts again (same container, \
         same service). The page goes quiet for a few seconds, and every device reconnects.",
        link.yes,
        link.json,
    )? {
        return output::done(link.json, "left it running");
    }
    // Restart checks `x-irori-ui` on the request itself, including an open home. A saved
    // session already sends it. A bearer token must not: the server refuses a token, and
    // the header would not change that. An open home with no session sends the header alone.
    let client = if remote.token.is_none() && connect::session_for(link.client.origin())?.is_none()
    {
        link.client.with_ui_header()
    } else {
        link.client.clone()
    };
    match client
        .empty(reqwest::Method::POST, "/api/restart", None::<&()>)
        .await
    {
        Ok(()) => output::done(link.json, "restarting"),
        Err(error) => Err(err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_goes_back_from_now_and_a_time_is_a_time() {
        let now: jiff::Timestamp = "2026-10-19T12:00:00Z".parse().expect("a time");
        assert_eq!(
            since_from("12h", now).map(|t| t.to_string()),
            Ok("2026-10-19T00:00:00Z".to_owned())
        );
        assert_eq!(
            since_from("7d", now).map(|t| t.to_string()),
            Ok("2026-10-12T12:00:00Z".to_owned())
        );
        assert_eq!(
            since_from("2026-01-02T03:04:05Z", now).map(|t| t.to_string()),
            Ok("2026-01-02T03:04:05Z".to_owned())
        );
        assert!(since_from("yesterday", now).is_err());
        assert!(since_from("h", now).is_err());
    }
}
