//! Extensions, including the uninstall menu, and the built-in toggles.

use irori_client::{CatalogEntry, Client, Home};
use serde_json::{Value, json};

use super::args::{ExtensionsCmd, HelpersCmd, Remote, ToggleCmd};
use super::connect;
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
                .empty(
                    reqwest::Method::POST,
                    &format!("/api/extensions/{id}/install"),
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
                    &format!("/api/extensions/{id}/settings"),
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
                    &format!("/api/extensions/{id}/secrets"),
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
                    &format!("/api/extensions/{id}/actions/{action}"),
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
    let body = json!({ "url": url, "approve_full_access": flag });
    match client
        .empty(
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
                .empty(
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
                println!("no extensions are installed");
                return Ok(());
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
            &format!("/api/extensions/{id}"),
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
            .get::<irori_client::Lines>(&format!("/api/extensions/{id}/log"))
            .await
            .map_err(err)?;
        let fresh = fresh_lines(&body.lines, last.as_deref());
        if json && !follow {
            return output::json_out(&body.lines);
        }
        for line in fresh {
            println!("{line}");
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

pub async fn helpers(remote: &Remote, cmd: HelpersCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    let HelpersCmd::Toggle { cmd } = cmd;
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
                    &format!("/api/helpers/toggles/{id}"),
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
