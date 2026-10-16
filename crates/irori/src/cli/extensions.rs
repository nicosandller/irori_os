//! Extensions, including the uninstall menu, and the built-in toggles.

use irori_client::{CatalogEntry, Client, Home};
use serde_json::{Map, Value, json};

use super::args::{ExtensionsCmd, HelperKind, HelperSettings, HelpersCmd, Remote, ToggleCmd};
use super::connect::{self, seg};
use super::output::{self, explain};
use super::prompt;

const FULL_ACCESS: &str = "has full access to this machine. It can download and run other \
     programs, the same as a terminal. Install it only if that is what you want.";

fn err(error: irori_client::Error) -> String {
    explain(&error)
}

pub async fn extensions(remote: &Remote, cmd: ExtensionsCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        ExtensionsCmd::List { installed } => {
            let mut catalog = link
                .client
                .get::<Vec<CatalogEntry>>("/api/catalog")
                .await
                .map_err(err)?;
            if installed {
                catalog.retain(|entry| entry.installed);
            }
            output::show(link.json, &catalog, || print_catalog(&catalog))
        }
        ExtensionsCmd::Show { id } => {
            let catalog = link
                .client
                .get::<Vec<CatalogEntry>>("/api/catalog")
                .await
                .map_err(err)?;
            let entry = find(&catalog, &id)?;
            output::show(link.json, entry, || {
                print_catalog(std::slice::from_ref(entry))
            })
        }
        ExtensionsCmd::Install {
            id,
            update,
            approve_full_access,
        } => {
            let catalog = link
                .client
                .get::<Vec<CatalogEntry>>("/api/catalog")
                .await
                .map_err(err)?;
            let entry = find(&catalog, &id)?;
            let approved = approve(
                &link.client,
                &entry.name,
                entry.full_access,
                approve_full_access,
                link.json,
            )
            .await?;
            if !approved {
                return output::done(link.json, "didn't install it");
            }
            let body = json!({
                "approve_full_access": entry.full_access || approve_full_access,
                "update": update,
            });
            link.client
                .empty_slow(
                    reqwest::Method::POST,
                    &format!("/api/extensions/{}/install", seg(&id)?),
                    Some(&body),
                )
                .await
                .map_err(err)?;
            let verb = if update { "updated" } else { "installed" };
            output::done(link.json, &format!("{verb} {id}"))
        }
        ExtensionsCmd::InstallUrl {
            url,
            approve_full_access,
        } => install_url(&link.client, &url, approve_full_access, link.json).await,
        ExtensionsCmd::Uninstall { id } => uninstall(&link.client, id, link.json, link.yes).await,
        ExtensionsCmd::Settings { id, file } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|error| format!("couldn't read {}: {error}", file.display()))?;
            let body: Value = serde_json::from_str(&text)
                .map_err(|error| format!("that file isn't JSON: {error}"))?;
            if !body.is_object() {
                return Err("settings are a JSON object".to_owned());
            }
            link.client
                .empty(
                    reqwest::Method::POST,
                    &format!("/api/extensions/{}/settings", seg(&id)?),
                    Some(&body),
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("saved settings for {id}"))
        }
        ExtensionsCmd::Secret { id, key, stdin } => {
            let value = prompt::password("Secret: ", stdin)?;
            let path: Vec<&str> = key.split('/').filter(|part| !part.is_empty()).collect();
            let body = json!({ "path": path, "value": value });
            link.client
                .empty(
                    reqwest::Method::PUT,
                    &format!("/api/extensions/{}/secrets", seg(&id)?),
                    Some(&body),
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("gave {id} the secret"))
        }
        ExtensionsCmd::Action { id, action, stop } => {
            let method = if stop {
                reqwest::Method::DELETE
            } else {
                reqwest::Method::POST
            };
            link.client
                .empty(
                    method,
                    &format!("/api/extensions/{}/actions/{}", seg(&id)?, seg(&action)?),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            let verb = if stop { "stopped" } else { "started" };
            output::done(link.json, &format!("{verb} {action}"))
        }
        ExtensionsCmd::Log { id, follow } => follow_log(&link.client, &id, follow, link.json).await,
    }
}

fn find<'a>(catalog: &'a [CatalogEntry], id: &str) -> Result<&'a CatalogEntry, String> {
    catalog
        .iter()
        .find(|entry| entry.id.as_str() == id)
        .ok_or_else(|| {
            let ids = catalog
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("`{id}` isn't in the catalog. It has: {ids}")
        })
}

fn print_catalog(catalog: &[CatalogEntry]) {
    for entry in catalog {
        let state = entry.state.as_deref().unwrap_or(if entry.installed {
            "installed"
        } else {
            "not installed"
        });
        let access = if entry.full_access {
            "\tfull access"
        } else {
            ""
        };
        let reason = entry
            .reason
            .as_deref()
            .map(|reason| format!("\t{reason}"))
            .unwrap_or_default();
        println!("{}\t{}\t{state}{access}{reason}", entry.id, entry.name);
    }
}

/// `Ok(true)` sends the install. `Ok(false)` means ask on a terminal. A non-interactive
/// full-access install without the flag is an error, `--json` included.
pub fn full_access_gate(full_access: bool, flag: bool, interactive: bool) -> Result<bool, String> {
    if !full_access || flag {
        return Ok(true);
    }
    if interactive {
        return Ok(false);
    }
    Err(format!("{FULL_ACCESS} Re-run with --approve-full-access."))
}

/// Prints the full-access sentence whenever the package has it. Returns whether to send
/// the install.
async fn approve(
    _client: &Client,
    name: &str,
    full_access: bool,
    flag: bool,
    json: bool,
) -> Result<bool, String> {
    let interactive = !json && std::io::IsTerminal::is_terminal(&std::io::stdin());
    if full_access {
        eprintln!("{name} {FULL_ACCESS}");
    }
    match full_access_gate(full_access, flag, interactive)? {
        true => Ok(true),
        false => prompt::confirm("Install anyway?", false, false),
    }
}

async fn install_url(client: &Client, url: &str, flag: bool, json: bool) -> Result<(), String> {
    // What the package asks for isn't known until Irori has downloaded it, so an approval
    // given ahead of time is told what it approves, as the catalog install is.
    if flag {
        eprintln!("If this package asks for it, it {FULL_ACCESS}");
    }
    let body = json!({ "url": url, "approve_full_access": flag });
    match client
        .empty_slow(
            reqwest::Method::POST,
            "/api/extensions/install",
            Some(&body),
        )
        .await
    {
        Ok(()) => output::done(json, "installed the package"),
        Err(error)
            if error.status == Some(409) && error.message.contains("full access") && !flag =>
        {
            let name = "This package";
            if !approve(client, name, true, false, json).await? {
                return output::done(json, "didn't install it");
            }
            let body = json!({ "url": url, "approve_full_access": true });
            client
                .empty_slow(
                    reqwest::Method::POST,
                    "/api/extensions/install",
                    Some(&body),
                )
                .await
                .map_err(err)?;
            output::done(json, "installed the package")
        }
        Err(error) => Err(err(error)),
    }
}

async fn uninstall(
    client: &Client,
    id: Option<String>,
    json: bool,
    yes: bool,
) -> Result<(), String> {
    let catalog = client
        .get::<Vec<CatalogEntry>>("/api/catalog")
        .await
        .map_err(err)?;
    let installed: Vec<&CatalogEntry> = catalog.iter().filter(|entry| entry.installed).collect();
    let id = match id {
        Some(id) => {
            if !installed.iter().any(|entry| entry.id.as_str() == id) {
                let ids = installed
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let ids = if ids.is_empty() {
                    "none are installed".to_owned()
                } else {
                    format!("installed: {ids}")
                };
                return Err(format!("`{id}` isn't installed. {ids}"));
            }
            id
        }
        None => {
            if installed.is_empty() {
                return output::done(json, "no extensions are installed");
            }
            if json {
                return Err("pass the extension id. --json doesn't open the menu.".to_owned());
            }
            let rows = installed
                .iter()
                .map(|entry| {
                    let state = entry.state.as_deref().unwrap_or("installed");
                    let access = if entry.full_access {
                        "    full access"
                    } else {
                        ""
                    };
                    format!("{:<10}  {:<12}  {state}{access}", entry.id, entry.name)
                })
                .collect::<Vec<_>>();
            let Some(index) = prompt::choose("Installed extensions:", &rows)? else {
                println!("left them installed");
                return Ok(());
            };
            installed[index].id.to_string()
        }
    };
    let entry = installed
        .iter()
        .find(|entry| entry.id.as_str() == id)
        .copied();
    let home: Home = client.get("/api/home").await.map_err(err)?;
    let devices = home
        .devices
        .iter()
        .filter(|device| device.protocol.as_str() == id)
        .count();
    let name = entry
        .map(|entry| entry.name.as_str())
        .unwrap_or(id.as_str());
    let question = format!(
        "Uninstall {name}? The package is deleted, and {devices} of its devices leave the home."
    );
    if !prompt::confirm(&question, yes, json)? {
        return output::done(json, "left it installed");
    }
    client
        .empty(
            reqwest::Method::DELETE,
            &format!("/api/extensions/{}", seg(&id)?),
            None::<&()>,
        )
        .await
        .map_err(err)?;
    output::done(json, &format!("uninstalled {id}"))
}

async fn follow_log(client: &Client, id: &str, follow: bool, json: bool) -> Result<(), String> {
    let mut last: Option<String> = None;
    loop {
        let body = client
            .get::<irori_client::Lines>(&format!("/api/extensions/{}/log", seg(id)?))
            .await
            .map_err(err)?;
        let fresh = fresh_lines(&body.lines, last.as_deref());
        if json && !follow {
            return output::json_out(&body.lines);
        }
        for line in fresh {
            // Following has no last line to close one document on, so each line is its
            // own, as `watch` does with frames.
            if json {
                output::json_out(line)?;
            } else {
                println!("{line}");
            }
        }
        last = body.lines.last().cloned().or(last);
        if !follow {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
    }
}

fn fresh_lines<'a>(lines: &'a [String], last: Option<&str>) -> &'a [String] {
    let Some(last) = last else {
        return lines;
    };
    match lines.iter().rposition(|line| line == last) {
        Some(index) => &lines[index + 1..],
        None => lines,
    }
}

/// What was given on the command line, as the fields the API takes. `initial` is read as what
/// the kind holds: a toggle's is `true` or `false`, a number's a number.
fn helper_fields(kind: HelperKind, settings: HelperSettings) -> Result<Map<String, Value>, String> {
    let mut fields = Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            fields.insert(key.to_owned(), value);
        }
    };
    put("min", settings.min.map(|n| json!(n)));
    put("max", settings.max.map(|n| json!(n)));
    // A timer's step is how long it runs.
    let step = if kind == HelperKind::Timer {
        "duration"
    } else {
        "step"
    };
    put(step, settings.step.map(|n| json!(n)));
    put("unit", settings.unit.map(|unit| json!(unit)));
    put("mode", settings.mode.map(|mode| json!(mode)));
    put(
        "options",
        (!settings.options.is_empty()).then(|| json!(settings.options)),
    );
    put("min_length", settings.min_length.map(|n| json!(n)));
    put("max_length", settings.max_length.map(|n| json!(n)));
    let initial = match (kind, settings.initial) {
        (_, None) => None,
        (HelperKind::Toggle, Some(text)) => Some(match text.as_str() {
            "true" | "on" => json!(true),
            "false" | "off" => json!(false),
            other => return Err(format!("a toggle starts `on` or `off`, not `{other}`")),
        }),
        (HelperKind::Number | HelperKind::Counter, Some(text)) => Some(json!(
            text.parse::<f64>()
                .map_err(|_| format!("`{text}` isn't a number to start at"))?
        )),
        (_, Some(text)) => Some(json!(text)),
    };
    put("initial", initial);
    for setting in settings.set {
        let (key, value) = setting
            .split_once('=')
            .ok_or_else(|| format!("`--set {setting}` isn't a setting: write it as key=value"))?;
        let value = value.trim();
        fields.insert(
            key.trim().to_owned(),
            serde_json::from_str(value).unwrap_or_else(|_| json!(value)),
        );
    }
    Ok(fields)
}

pub async fn helpers(remote: &Remote, cmd: HelpersCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    let cmd = match cmd {
        HelpersCmd::Toggle { cmd } => cmd,
        HelpersCmd::List => {
            let helpers = link
                .client
                .get::<Vec<Value>>("/api/helpers")
                .await
                .map_err(err)?;
            return output::show(link.json, &helpers, || {
                for helper in &helpers {
                    let text = |key: &str| helper[key].as_str().unwrap_or("—");
                    println!(
                        "{}\t{}\t{}\t{}",
                        text("kind"),
                        text("id"),
                        text("entity_id"),
                        text("name")
                    );
                }
            });
        }
        HelpersCmd::Add {
            kind,
            name,
            settings,
        } => {
            let mut body = helper_fields(kind, settings)?;
            body.insert("name".into(), json!(name));
            let made = link
                .client
                .post::<Value>(&format!("/api/helpers/{}", kind.table()), &body)
                .await
                .map_err(err)?;
            return output::show(link.json, &made, || {
                println!(
                    "added {}",
                    made["entity_id"].as_str().unwrap_or(name.as_str())
                );
            });
        }
        HelpersCmd::Set {
            kind,
            id,
            name,
            settings,
        } => {
            let mut body = helper_fields(kind, settings)?;
            if let Some(name) = name {
                body.insert("name".into(), json!(name));
            }
            if body.is_empty() {
                return Err("nothing to change: give a setting, such as --max 80".into());
            }
            link.client
                .empty(
                    reqwest::Method::PATCH,
                    &format!("/api/helpers/{}/{}", kind.table(), seg(&id)?),
                    Some(&body),
                )
                .await
                .map_err(err)?;
            return output::done(link.json, &format!("changed {id}"));
        }
        HelpersCmd::Remove { kind, id } => {
            if !prompt::confirm(&format!("Remove the helper {id}?"), link.yes, link.json)? {
                return output::done(link.json, "left the helper");
            }
            link.client
                .empty(
                    reqwest::Method::DELETE,
                    &format!("/api/helpers/{}/{}", kind.table(), seg(&id)?),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            return output::done(link.json, &format!("removed {id}"));
        }
    };
    match cmd {
        ToggleCmd::Add { name } => {
            link.client
                .empty(
                    reqwest::Method::POST,
                    "/api/helpers/toggles",
                    Some(&json!({ "name": name })),
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("added a toggle called {name}"))
        }
        ToggleCmd::Remove { id } => {
            if !prompt::confirm(&format!("Remove the toggle {id}?"), link.yes, link.json)? {
                return output::done(link.json, "left the toggle");
            }
            link.client
                .empty(
                    reqwest::Method::DELETE,
                    &format!("/api/helpers/toggles/{}", seg(&id)?),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("removed {id}"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn json_without_the_flag_refuses_full_access() {
        let error = full_access_gate(true, false, false).unwrap_err();
        assert!(error.contains("full access to this machine"), "{error}");
        assert!(error.contains("--approve-full-access"), "{error}");
        assert!(full_access_gate(true, true, false).unwrap());
        assert!(full_access_gate(false, false, false).unwrap());
        assert!(!full_access_gate(true, false, true).unwrap());
    }
}
