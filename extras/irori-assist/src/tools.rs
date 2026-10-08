//! The read-only tools a model may call. A few rounds, then a plain answer. Writing the home is
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

/// One tool, said once. Each provider's shape is made from this.
struct Tool {
    name: &'static str,
    description: &'static str,
    /// Each parameter: its name, what it is, and whether the call must give it. All strings.
    params: &'static [(&'static str, &'static str, bool)],
    /// Whether the conversation `scope` is offered it.
    offered: fn(&str) -> bool,
}

fn about_the_home(scope: &str) -> bool {
    scope != "settings"
}

/// The chat about the whole home, which may ask about everything Irori holds.
fn general(scope: &str) -> bool {
    scope == "general"
}

const TOOLS: [Tool; 7] = [
    Tool {
        name: "list_devices",
        description: "List the devices in the home, with the room and the word each entity is reporting.",
        params: &[],
        offered: |_| true,
    },
    Tool {
        name: "get_device",
        description: "One device's entities and what they report right now.",
        params: &[("id", "The device id.", true)],
        offered: |_| true,
    },
    Tool {
        name: "recent_states",
        description: "Recent reported values for one entity.",
        params: &[("entity_id", "The entity id.", true)],
        offered: about_the_home,
    },
    Tool {
        name: "read_logs",
        description: "The newest lines of a log, oldest first. Irori's own log also carries what its extensions said.",
        params: &[
            (
                "source",
                "`irori` for Irori's own log, `model` for the log of the model on this machine, or an extension's id for that extension's own output.",
                true,
            ),
            (
                "contains",
                "Only lines containing this text, whatever its case. Leave out for every line.",
                false,
            ),
        ],
        offered: |scope| scope == "settings" || general(scope),
    },
    Tool {
        name: "read_settings",
        description: "Everything Irori's Settings page holds: the machine it runs on, the assistant's own configuration, floors and areas with the devices in each, extensions and their settings, and the newest warnings and errors.",
        params: &[],
        offered: general,
    },
    Tool {
        name: "list_automations",
        description: "Every automation, with its id, whether it is on, how its last run came out, and how many problems it has.",
        params: &[],
        offered: general,
    },
    Tool {
        name: "get_automation",
        description: "One automation in full: its logic step by step, what the entities it reads report now, its last run, near-misses, and what is wrong with it.",
        params: &[(
            "id",
            "The automation's id, as list_automations gives it.",
            true,
        )],
        offered: general,
    },
];

fn schema(tool: &Tool) -> Value {
    let properties: serde_json::Map<String, Value> = tool
        .params
        .iter()
        .map(|(name, description, _)| {
            (
                (*name).to_owned(),
                json!({ "type": "string", "description": description }),
            )
        })
        .collect();
    let required: Vec<&str> = tool
        .params
        .iter()
        .filter(|(_, _, required)| *required)
        .map(|(name, _, _)| *name)
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Whether the conversation `scope` is offered the tool `name`.
pub fn tool_offered(scope: &str, name: &str) -> bool {
    TOOLS
        .iter()
        .any(|tool| tool.name == name && (tool.offered)(scope))
}

/// The tools the conversation `scope` is offered, in OpenAI's shape. Anthropic uses
/// [`anthropic_tools`].
pub fn openai_tools(scope: &str) -> Value {
    TOOLS
        .iter()
        .filter(|tool| (tool.offered)(scope))
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": schema(tool),
                }
            })
        })
        .collect()
}

pub fn anthropic_tools(scope: &str) -> Value {
    TOOLS
        .iter()
        .filter(|tool| (tool.offered)(scope))
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": schema(tool),
            })
        })
        .collect()
}

/// The highest fragment index [`assemble`] reads, plus one. The index is the provider's word,
/// and Anthropic counts every block of the answer, so this is roomier than the tool list.
pub const LIMIT_CALLS: usize = 16;

/// Joins streamed tool fragments that share an index into one call each. A fragment whose
/// index is past [`LIMIT_CALLS`] is dropped.
pub fn assemble(parts: &[(usize, Option<String>, Option<String>, String)]) -> Vec<ToolCall> {
    let mut calls: Vec<ToolCall> = Vec::new();
    for (index, id, name, arguments) in parts {
        if *index >= LIMIT_CALLS {
            continue;
        }
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
    fn a_call_after_a_block_of_text_is_still_found() {
        let calls = assemble(&[
            (
                1,
                Some("a".into()),
                Some("list_devices".into()),
                String::new(),
            ),
            (1, None, None, "{}".into()),
        ]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_devices");
    }

    #[test]
    fn an_absurd_index_is_dropped_instead_of_allocated() {
        let calls = assemble(&[
            (
                usize::MAX,
                Some("a".into()),
                Some("get_device".into()),
                "{}".into(),
            ),
            (
                0,
                Some("b".into()),
                Some("list_devices".into()),
                String::new(),
            ),
        ]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_devices");
    }

    fn names(tools: &Value, at: &str) -> Vec<String> {
        tools
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.pointer(at)?.as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn each_conversation_is_offered_its_own_tools_in_both_shapes() {
        let home = ["list_devices", "get_device", "recent_states"];
        let settings = ["list_devices", "get_device", "read_logs"];
        // The chat about the whole home is offered all of it.
        let general = [
            "list_devices",
            "get_device",
            "recent_states",
            "read_logs",
            "read_settings",
            "list_automations",
            "get_automation",
        ];
        for scope in ["device:lamp", "automation:kettle"] {
            assert_eq!(names(&openai_tools(scope), "/function/name"), home);
            assert_eq!(names(&anthropic_tools(scope), "/name"), home);
        }
        assert_eq!(names(&openai_tools("general"), "/function/name"), general);
        assert_eq!(names(&anthropic_tools("general"), "/name"), general);
        assert_eq!(names(&openai_tools("settings"), "/function/name"), settings);
        assert_eq!(names(&anthropic_tools("settings"), "/name"), settings);
        let logs = &anthropic_tools("settings")[2];
        assert_eq!(logs["input_schema"]["required"], json!(["source"]));
        assert!(tool_offered("settings", "read_logs"));
        assert!(tool_offered("general", "read_logs"));
        assert!(tool_offered("general", "get_automation"));
        assert!(!tool_offered("device:lamp", "read_logs"));
        assert!(!tool_offered("automation:kettle", "list_automations"));
        assert!(!tool_offered("settings", "read_settings"));
        assert!(!tool_offered("settings", "recent_states"));
        assert!(logs["input_schema"]["properties"]["contains"].is_object());
    }

    #[test]
    fn a_third_round_is_refused() {
        assert!(execute_round(0));
        assert!(execute_round(1));
        assert!(!execute_round(2));
    }
}
