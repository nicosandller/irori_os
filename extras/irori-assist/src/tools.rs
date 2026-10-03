//! The read-only tools a model may call. Two rounds, then a plain answer. Writing the home is
//! not one of them.

use serde_json::{Value, json};

/// How many times the model may ask for a tool before it has to answer in words.
pub const TOOL_ROUNDS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// OpenAI-shaped tool list. Anthropic uses [`anthropic_tools`].
pub fn openai_tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_devices",
                "description": "List the devices in the home, with the room and the word each entity is reporting.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_device",
                "description": "One device's entities and what they report right now.",
                "parameters": {
                    "type": "object",
                    "properties": { "id": { "type": "string", "description": "The device id." } },
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "recent_states",
                "description": "Recent reported values for one entity.",
                "parameters": {
                    "type": "object",
                    "properties": { "entity_id": { "type": "string" } },
                    "required": ["entity_id"],
                    "additionalProperties": false
                }
            }
        }
    ])
}

pub fn anthropic_tools() -> Value {
    json!([
        {
            "name": "list_devices",
            "description": "List the devices in the home, with the room and the word each entity is reporting.",
            "input_schema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_device",
            "description": "One device's entities and what they report right now.",
            "input_schema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "recent_states",
            "description": "Recent reported values for one entity.",
            "input_schema": {
                "type": "object",
                "properties": { "entity_id": { "type": "string" } },
                "required": ["entity_id"]
            }
        }
    ])
}

/// Joins streamed tool fragments that share an index into one call each.
pub fn assemble(parts: &[(usize, Option<String>, Option<String>, String)]) -> Vec<ToolCall> {
    let mut calls: Vec<ToolCall> = Vec::new();
    for (index, id, name, arguments) in parts {
        if calls.len() <= *index {
            calls.resize(
                index + 1,
                ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                },
            );
        }
        let call = &mut calls[*index];
        if let Some(id) = id
            && !id.is_empty()
        {
            call.id = id.clone();
        }
        if let Some(name) = name
            && !name.is_empty()
        {
            call.name = name.clone();
        }
        call.arguments.push_str(arguments);
    }
    calls.retain(|call| !call.name.is_empty());
    for (n, call) in calls.iter_mut().enumerate() {
        if call.id.is_empty() {
            call.id = format!("call-{n}");
        }
        if call.arguments.is_empty() {
            call.arguments = "{}".to_owned();
        }
    }
    calls
}

/// Whether another tool round is allowed. `rounds_done` is how many have already run.
pub fn execute_round(rounds_done: u8) -> bool {
    rounds_done < TOOL_ROUNDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_of_one_call_become_one_call() {
        let calls = assemble(&[
            (
                0,
                Some("a".into()),
                Some("get_device".into()),
                "{\"id\":".into(),
            ),
            (0, None, None, "\"lamp\"}".into()),
        ]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_device");
        assert_eq!(calls[0].arguments, "{\"id\":\"lamp\"}");
    }

    #[test]
    fn a_third_round_is_refused() {
        assert!(execute_round(0));
        assert!(execute_round(1));
        assert!(!execute_round(2));
    }
}
