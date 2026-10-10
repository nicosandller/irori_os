//! Flows, through the Automations extension's RPC. The canvas stays on the page.

use std::path::Path;

use serde_json::{Value, json};

use super::args::{AutomationsCmd, Remote};
use super::connect;
use super::output::{self, explain};
use super::prompt;

fn err(error: irori_client::Error) -> String {
    let message = explain(&error);
    if message.contains("`automations` isn't running") {
        format!("{message}. Install it with `irori extensions install automations`.")
    } else {
        message
    }
}

pub async fn run(remote: &Remote, cmd: AutomationsCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        AutomationsCmd::List => {
            let value = rpc(&link.client, "flows.list", &json!({})).await?;
            output::show(link.json, &value, || print_flows(&value))
        }
        AutomationsCmd::Show { id } => {
            let value = rpc(&link.client, "flows.get", &json!({ "id": id })).await?;
            output::show(link.json, &value, || print_flow(&value))
        }
        AutomationsCmd::Validate { id, file } => validate(&link.client, id, file, link.json).await,
        AutomationsCmd::Enable { id } => {
            let value = rpc(
                &link.client,
                "flows.enable",
                &json!({ "id": id, "enabled": true }),
            )
            .await?;
            output::show(link.json, &value, || println!("enabled {id}"))
        }
        AutomationsCmd::Disable { id } => {
            let value = rpc(
                &link.client,
                "flows.enable",
                &json!({ "id": id, "enabled": false }),
            )
            .await?;
            output::show(link.json, &value, || println!("disabled {id}"))
        }
        AutomationsCmd::Delete { id } => {
            if !prompt::confirm(&format!("Delete the flow {id}?"), link.yes, link.json)? {
                return output::done(link.json, "left the flow");
            }
            let value = rpc(&link.client, "flows.delete", &json!({ "id": id })).await?;
            output::show(link.json, &value, || println!("deleted {id}"))
        }
        AutomationsCmd::Runs { id } => {
            let value = match id {
                Some(id) => rpc(&link.client, "runs.list", &json!({ "id": id })).await?,
                None => rpc(&link.client, "runs.active", &json!({})).await?,
            };
            output::show(link.json, &value, || println!("{}", pretty(&value)))
        }
        AutomationsCmd::Run { id, run_id } => {
            let value = rpc(
                &link.client,
                "runs.get",
                &json!({ "id": id, "run_id": run_id }),
            )
            .await?;
            output::show(link.json, &value, || println!("{}", pretty(&value)))
        }
        AutomationsCmd::Cancel { run_id } => {
            if !prompt::confirm(&format!("Cancel the run {run_id}?"), link.yes, link.json)? {
                return output::done(link.json, "left the run");
            }
            let value = rpc(&link.client, "runs.cancel", &json!({ "run_id": run_id })).await?;
            output::show(link.json, &value, || println!("cancelled {run_id}"))
        }
        AutomationsCmd::Test { id, trigger, dry } => {
            let value = rpc(
                &link.client,
                "test",
                &json!({ "id": id, "trigger": trigger, "dry": dry }),
            )
            .await?;
            output::show(link.json, &value, || println!("{}", pretty(&value)))
        }
        AutomationsCmd::Backtest { id } => {
            let value = rpc(&link.client, "backtest", &json!({ "id": id })).await?;
            output::show(link.json, &value, || println!("{}", pretty(&value)))
        }
        AutomationsCmd::Versions { id } => {
            let value = rpc(&link.client, "versions.list", &json!({ "id": id })).await?;
            output::show(link.json, &value, || println!("{}", pretty(&value)))
        }
        AutomationsCmd::Restore { id, version } => {
            if !prompt::confirm(
                &format!("Restore {id} to version {version}?"),
                link.yes,
                link.json,
            )? {
                return output::done(link.json, "left the flow");
            }
            let value = rpc(
                &link.client,
                "versions.restore",
                &json!({ "id": id, "version": version }),
            )
            .await?;
            output::show(link.json, &value, || println!("restored {id}"))
        }
    }
}

async fn validate(
    client: &irori_client::Client,
    id: Option<String>,
    file: Option<std::path::PathBuf>,
    json: bool,
) -> Result<(), String> {
    let flow = if let Some(file) = file {
        read_flow(&file)?
    } else {
        let id = id.ok_or_else(|| "pass a flow id, or a JSON file".to_owned())?;
        let detail = rpc(client, "flows.get", &json!({ "id": id })).await?;
        detail
            .get("flow")
            .cloned()
            .ok_or_else(|| "the flow didn't come back".to_owned())?
    };
    let problems = rpc(client, "flows.validate", &json!({ "flow": flow })).await?;
    let errors = problems
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("severity").and_then(Value::as_str) == Some("error"))
                .count()
        })
        .unwrap_or(0);
    if errors > 0 {
        if json {
            output::json_out(&json!({ "problems": problems, "errors": errors }))?;
            // The problems are the failure. A second `{ "error" }` line would be a second document.
            std::process::exit(1);
        }
        println!("{}", pretty(&problems));
        return Err(format!("{errors} errors"));
    }
    if json {
        output::json_out(&problems)?;
    } else if problems.as_array().is_some_and(|items| items.is_empty()) {
        println!("the flow is valid");
    } else {
        println!("{}", pretty(&problems));
    }
    Ok(())
}

fn read_flow(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("couldn't read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("that file isn't JSON: {error}"))
}

async fn rpc(client: &irori_client::Client, method: &str, params: &Value) -> Result<Value, String> {
    let body = client
        .post::<Value>(
            "/api/apps/automations/rpc",
            &json!({ "method": method, "params": params }),
        )
        .await
        .map_err(err)?;
    Ok(body.get("value").cloned().unwrap_or(body))
}

fn print_flows(value: &Value) {
    let Some(flows) = value.get("flows").and_then(Value::as_array) else {
        println!("{}", pretty(value));
        return;
    };
    if flows.is_empty() {
        println!("no flows");
    }
    for flow in flows {
        let id = flow.get("id").and_then(Value::as_str).unwrap_or("?");
        let name = flow.get("name").and_then(Value::as_str).unwrap_or("");
        let state = flow.get("state").and_then(Value::as_str).unwrap_or("");
        let problems = flow.get("problems").and_then(Value::as_u64).unwrap_or(0);
        println!("{id}\t{name}\t{state}\t{problems} problems");
    }
    if let Some(problems) = value.get("file_problems").and_then(Value::as_array)
        && !problems.is_empty()
    {
        println!("{}", pretty(&Value::Array(problems.clone())));
    }
}

fn print_flow(value: &Value) {
    let name = value
        .pointer("/flow/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let state = value.get("state").and_then(Value::as_str).unwrap_or("");
    println!("{name}\t{state}");
    if let Some(problems) = value.get("problems").and_then(Value::as_array) {
        for problem in problems {
            let message = problem.get("message").and_then(Value::as_str).unwrap_or("");
            println!("{message}");
        }
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
