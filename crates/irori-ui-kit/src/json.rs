//! JSON for a person to read, copy and change: written with its short lists on one line, and
//! cut into pieces by what each is, for a box that colours what is typed into it.
//!
//! A text box can't colour what is in it, so the colour is a second copy of the text drawn
//! underneath, in the same letters at the same place, and the box itself is typed into with
//! its own letters see-through. [`pieces`] is what that second copy is made of.

use serde_json::Value;

/// What a piece of the text is, for the colour it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The name of a field: `"walls"`.
    Name,
    /// A piece of text that is a value: `"door"`.
    Text,
    Number,
    /// `true`, `false` or `null`.
    Word,
    /// Brackets, commas, colons, the space between, and anything that isn't JSON at all.
    Plain,
}

impl Kind {
    pub fn class(self) -> &'static str {
        match self {
            Kind::Name => "json-name",
            Kind::Text => "json-text",
            Kind::Number => "json-number",
            Kind::Word => "json-word",
            Kind::Plain => "",
        }
    }
}

/// JSON as it's typed, cut into pieces by what each is, for colouring. Every character comes back
/// in exactly one piece, in order, whatever was typed: this runs on each key, half-way through
/// a word as often as not, so it reads what is there rather than deciding whether it is JSON.
pub fn pieces(text: &str) -> Vec<(Kind, String)> {
    let mut pieces: Vec<(Kind, String)> = Vec::new();
    let mut push = |kind: Kind, piece: &str| match pieces.last_mut() {
        // Runs of the plain stuff are one piece, so a document is hundreds of spans, not thousands.
        Some((Kind::Plain, last)) if kind == Kind::Plain => last.push_str(piece),
        _ if piece.is_empty() => {}
        _ => pieces.push((kind, piece.to_owned())),
    };
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b'"' => {
                at += 1;
                // To the quote that closes it, or the end of the line if nothing does: a
                // string being typed shouldn't colour the rest of the text with it.
                while at < bytes.len() && bytes[at] != b'"' && bytes[at] != b'\n' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
                at = (at + 1).min(bytes.len());
                // A name is a string with a colon after it.
                let after = bytes[at..].iter().find(|byte| !byte.is_ascii_whitespace());
                let kind = if after == Some(&b':') {
                    Kind::Name
                } else {
                    Kind::Text
                };
                // Only ever cut on a quote or a line end, which are whole characters.
                push(kind, text.get(start..at).unwrap_or_default());
            }
            b'-' | b'0'..=b'9' => {
                while at < bytes.len()
                    && matches!(bytes[at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    at += 1;
                }
                push(Kind::Number, &text[start..at]);
            }
            b'a'..=b'z' => {
                while at < bytes.len() && bytes[at].is_ascii_lowercase() {
                    at += 1;
                }
                let word = &text[start..at];
                let kind = if matches!(word, "true" | "false" | "null") {
                    Kind::Word
                } else {
                    Kind::Plain
                };
                push(kind, word);
            }
            _ => {
                // One whole character, however many bytes it is.
                at += 1;
                while !text.is_char_boundary(at) {
                    at += 1;
                }
                push(Kind::Plain, &text[start..at]);
            }
        }
    }
    pieces
}

/// A value as text to read: indented, with each short list — a point, a run of points, a pair
/// of names — on one line. The usual pretty-printing gives every number a line of its own, and
/// a room's outline or a flow's wires then run off the bottom before they have said anything.
pub fn written(value: &Value) -> String {
    let mut text = String::new();
    write(value, 0, &mut text);
    text
}

/// The most words a list can hold and still be said on one line: a wire is two.
const BRIEF_WORDS: usize = 3;

/// Whether a value is short enough to say on one line: a number, a point, a run of points, or
/// a few words (a wire's two ends, a colour's three numbers).
fn brief(value: &Value) -> bool {
    match value {
        Value::Array(items) if items.iter().all(Value::is_string) => {
            !items.is_empty() && items.len() <= BRIEF_WORDS
        }
        Value::Array(items) => items.iter().all(|item| match item {
            Value::Number(_) => true,
            Value::Array(inner) => inner.iter().all(Value::is_number),
            _ => false,
        }),
        Value::Object(_) => false,
        _ => true,
    }
}

fn write(value: &Value, depth: usize, into: &mut String) {
    let indent = |depth: usize, into: &mut String| into.push_str(&"  ".repeat(depth));
    match value {
        Value::Array(items) if brief(value) => {
            into.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    into.push_str(", ");
                }
                write(item, depth, into);
            }
            into.push(']');
        }
        Value::Array(items) => {
            into.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                indent(depth + 1, into);
                write(item, depth + 1, into);
                into.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(depth, into);
            into.push(']');
        }
        Value::Object(fields) if fields.is_empty() => into.push_str("{}"),
        Value::Object(fields) => {
            into.push_str("{\n");
            for (index, (name, field)) in fields.iter().enumerate() {
                indent(depth + 1, into);
                into.push_str(&Value::from(name.as_str()).to_string());
                into.push_str(": ");
                write(field, depth + 1, into);
                into.push_str(if index + 1 < fields.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            indent(depth, into);
            into.push('}');
        }
        other => into.push_str(&other.to_string()),
    }
}
