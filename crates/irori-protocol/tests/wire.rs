//! The extension wire as someone outside Rust sees it (`docs/specs/protocols.md` §10): the
//! sample conversations in `fixtures/` are real messages, and every message type in the code is
//! in the spec's tables.

use std::path::{Path, PathBuf};

use irori_protocol::{FromExt, ToExt};
use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether everything `written` says is in `read`, the message as the types read it, written
/// back. A misspelt or unknown field would otherwise pass unnoticed: reading ignores it.
fn kept(written: &Value, read: &Value) -> bool {
    match (written, read) {
        (Value::Object(written), Value::Object(read)) => written
            .iter()
            .all(|(key, value)| read.get(key).is_some_and(|read| kept(value, read))),
        (Value::Array(written), Value::Array(read)) => {
            written.len() == read.len() && written.iter().zip(read).all(|(w, r)| kept(w, r))
        }
        _ => written == read,
    }
}

#[test]
fn every_sample_conversation_is_made_of_real_messages() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut lines = 0;
    for entry in std::fs::read_dir(&dir).expect("fixtures") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("readable");
        for (n, line) in text.lines().enumerate() {
            let at = format!("{}:{}", path.display(), n + 1);
            let line: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{at}: {e}"));
            let message = line["message"].clone();
            let read = match line["from"].as_str() {
                Some("extension") => serde_json::from_value::<FromExt>(message.clone())
                    .map(|m| serde_json::to_value(m).expect("writes")),
                Some("core") => serde_json::from_value::<ToExt>(message.clone())
                    .map(|m| serde_json::to_value(m).expect("writes")),
                other => panic!("{at}: `from` is {other:?}, not \"extension\" or \"core\""),
            }
            .unwrap_or_else(|e| panic!("{at}: not a message: {e}"));
            assert!(kept(&message, &read), "{at}: a field was dropped: {read}");
            lines += 1;
        }
    }
    assert!(lines > 0, "no sample conversations in {}", dir.display());
}

#[test]
fn every_message_type_is_in_the_spec() {
    let spec = std::fs::read_to_string(repo().join("docs/specs/protocols.md")).expect("spec");
    let start = spec.find("## 10. ").expect("§10");
    let end = spec[start..]
        .find("\n## 11. ")
        .map_or(spec.len(), |end| start + end);
    let section = &spec[start..end];
    for (schema, generated) in [
        ("extension-to-core", irori_protocol::schemas().remove(0)),
        ("core-to-extension", irori_protocol::schemas().remove(1)),
    ] {
        assert_eq!(generated.name, schema);
        let schema = generated.schema.as_value();
        let types = schema["oneOf"]
            .as_array()
            .expect("one schema per message")
            .iter()
            .map(|variant| {
                variant["properties"]["type"]["const"]
                    .as_str()
                    .expect("tagged by `type`")
                    .to_owned()
            });
        for name in types {
            assert!(
                section.contains(&format!("`{name}`")),
                "`{name}` ({}) is missing from protocols.md §10",
                generated.name
            );
        }
    }
}
