//! Human lines, and `--json`. A failure exits 1. Clap keeps 2 for a bad invocation.

use serde::Serialize;
use serde_json::json;

use irori_client::Error;

pub fn json_out(value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("couldn't write JSON: {error}"))?;
    println!("{text}");
    Ok(())
}

pub fn show(json: bool, value: &impl Serialize, human: impl FnOnce()) -> Result<(), String> {
    if json {
        json_out(value)
    } else {
        human();
        Ok(())
    }
}

pub fn done(json: bool, sentence: &str) -> Result<(), String> {
    if json {
        json_out(&json!({ "ok": true }))
    } else {
        println!("{sentence}");
        Ok(())
    }
}

/// The server's sentence, plus how to sign in when a token was the wrong kind of key.
pub fn explain(error: &Error) -> String {
    if error
        .message
        .contains("a token can't change how the home is set up")
    {
        format!(
            "{}. Sign in with `irori login`",
            error.message.trim_end_matches('.')
        )
    } else if error.code.as_deref() == Some("unpair_failed") {
        format!(
            "{} The device stayed. Re-run with --force to remove it anyway.",
            error.message
        )
    } else {
        error.message.clone()
    }
}

pub fn fail(json: bool, message: &str) -> ! {
    if json {
        let text = serde_json::to_string_pretty(&json!({ "error": message }))
            .unwrap_or_else(|_| format!("{{\"error\":{message:?}}}"));
        println!("{text}");
    } else {
        eprintln!("error: {message}");
    }
    std::process::exit(1);
}

pub fn finish(json: bool, result: Result<(), String>) -> anyhow::Result<()> {
    if let Err(message) = result {
        fail(json, &message);
    }
    Ok(())
}
