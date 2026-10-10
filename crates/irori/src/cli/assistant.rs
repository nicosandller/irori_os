//! The assistant, only in a build that includes it.

use serde_json::{Value, json};

use super::args::{AssistantCmd, Remote};
use super::connect;
use super::output::{self, explain};
use super::prompt;

fn err(error: irori_client::Error) -> String {
    explain(&error)
}

pub async fn run(remote: &Remote, cmd: AssistantCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        AssistantCmd::Status => {
            let status = link
                .client
                .get::<Value>("/api/assistant")
                .await
                .map_err(err)?;
            output::show(link.json, &status, || print_status(&status))
        }
        AssistantCmd::Settings { file } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|error| format!("couldn't read {}: {error}", file.display()))?;
            let body: Value = serde_json::from_str(&text)
                .map_err(|error| format!("that file isn't JSON: {error}"))?;
            let status = link
                .client
                .put::<Value>("/api/assistant", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &status, || print_status(&status))
        }
        AssistantCmd::Install => link
            .client
            .sse("/api/assistant/install", &json!({}), print_event)
            .await
            .map_err(err),
        AssistantCmd::Uninstall => {
            if !prompt::confirm(
                "Uninstall the assistant's local model runner?",
                link.yes,
                link.json,
            )? {
                return output::done(link.json, "left it installed");
            }
            let status = link
                .client
                .post::<Value>("/api/assistant/uninstall", &json!({}))
                .await
                .map_err(err)?;
            output::show(link.json, &status, || print_status(&status))
        }
        AssistantCmd::Pull { tag } => link
            .client
            .sse("/api/assistant/pull", &json!({ "tag": tag }), print_event)
            .await
            .map_err(err),
        AssistantCmd::Load { tag } => {
            let status = link
                .client
                .post::<Value>("/api/assistant/load", &json!({ "tag": tag }))
                .await
                .map_err(err)?;
            output::show(link.json, &status, || print_status(&status))
        }
        AssistantCmd::Unload { tag } => {
            let status = link
                .client
                .post::<Value>("/api/assistant/unload", &json!({ "tag": tag }))
                .await
                .map_err(err)?;
            output::show(link.json, &status, || print_status(&status))
        }
        AssistantCmd::Forget { tag } => {
            if !prompt::confirm(&format!("Forget the model {tag}?"), link.yes, link.json)? {
                return output::done(link.json, "left the model");
            }
            let status = link
                .client
                .post::<Value>("/api/assistant/forget", &json!({ "tag": tag }))
                .await
                .map_err(err)?;
            output::show(link.json, &status, || print_status(&status))
        }
        AssistantCmd::Ask { scope, message } => {
            let mut failed = None;
            link.client
                .sse(
                    "/api/assistant/turns",
                    &json!({ "scope": scope, "message": message }),
                    |data| {
                        if let Ok(value) = serde_json::from_str::<Value>(data) {
                            if let Some(text) = value.get("delta").and_then(Value::as_str) {
                                print!("{text}");
                                let _ = std::io::Write::flush(&mut std::io::stdout());
                            } else if let Some(text) = value.get("error").and_then(Value::as_str) {
                                failed = Some(text.to_owned());
                            } else if let Some(step) = value.get("step").and_then(Value::as_str) {
                                eprintln!("[{step}]");
                            }
                        }
                    },
                )
                .await
                .map_err(err)?;
            println!();
            if let Some(failed) = failed {
                return Err(failed);
            }
            Ok(())
        }
        AssistantCmd::Log => {
            let body = link
                .client
                .get::<irori_client::Lines>("/api/assistant/log")
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
    }
}

fn print_status(status: &Value) {
    let ready = status
        .get("ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let detail = status.get("detail").and_then(Value::as_str).unwrap_or("");
    let model = status
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("none");
    println!(
        "{}\t{model}\t{detail}",
        if ready { "ready" } else { "not ready" }
    );
}

fn print_event(data: &str) {
    if let Ok(value) = serde_json::from_str::<Value>(data) {
        if let Some(text) = value.get("error").and_then(Value::as_str) {
            eprintln!("error: {text}");
            return;
        }
        if value.get("done").and_then(Value::as_bool) == Some(true) {
            return;
        }
    }
    println!("{data}");
}
