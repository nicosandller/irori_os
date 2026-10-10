//! The tools a model may call. A few rounds, then a plain answer. All but one only read, and
//! writing the home is not among them: the one that changes anything draws on the copy of the
//! floorplan a person has open for editing, which is theirs to undo and theirs to save.

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
    /// Whether the conversation `scope` is offered it. The second is whether the person asking
    /// has the floorplan open for editing, and sent the copy they are drawing on.
    offered: fn(&str, bool) -> bool,
}

fn about_the_home(scope: &str, _drawing: bool) -> bool {
    scope != "settings"
}

/// The chat on the Floorplan page, about one floor of the home.
fn plan(scope: &str) -> bool {
    scope.starts_with("floorplan:")
}

/// The chat about the whole home, which may ask about everything Irori holds.
fn general(scope: &str) -> bool {
    scope == "general"
}

const TOOLS: [Tool; 10] = [
    Tool {
        name: "list_devices",
        description: "List the devices in the home, with the room and the word each entity is reporting.",
        params: &[],
        offered: |_, _| true,
    },
    Tool {
        name: "get_device",
        description: "One device's entities and what they report right now.",
        params: &[("id", "The device id.", true)],
        offered: |_, _| true,
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
        offered: |scope, _| scope == "settings" || general(scope),
    },
    Tool {
        name: "read_settings",
        description: "Everything Irori's Settings page holds: the machine it runs on, the assistant's own configuration, floors and areas with the devices in each, extensions and their settings, and the newest warnings and errors.",
        params: &[],
        offered: |scope, _| general(scope),
    },
    Tool {
        name: "list_automations",
        description: "Every automation, with its id, whether it is on, how its last run came out, and how many problems it has.",
        params: &[],
        offered: |scope, _| general(scope) || plan(scope),
    },
    Tool {
        name: "get_automation",
        description: "One automation in full: its logic step by step, what the entities it reads report now, its last run, near-misses, and what is wrong with it.",
        params: &[(
            "id",
            "The automation's id, as list_automations gives it.",
            true,
        )],
        offered: |scope, _| general(scope) || plan(scope),
    },
    Tool {
        name: "read_floorplan",
        description: "One floor of the home as it is drawn: its walls by number with their doors and windows, the rooms traced on it, and where each device stands. Whole centimetres, x rightwards and y down the page.",
        params: &[(
            "floor",
            "The floor's id. Leave out for the floor being looked at, or for every floor when none is.",
            false,
        )],
        offered: |scope, _| general(scope) || plan(scope),
    },
    Tool {
        name: "edit_floorplan",
        description: "Draw on the floor being looked at: add or remove walls, doors, windows, and the outlines of rooms. Every change in one call is made together or not at all. The person sees it on their plan at once and can undo it; nothing is saved until they press Save.",
        params: &[(
            "ops",
            "A JSON array of changes. Each is an object with an `op`: \
             {\"op\":\"add_wall\",\"from\":[x,y],\"to\":[x,y],\"thickness\":10} (thickness optional); \
             {\"op\":\"add_opening\",\"wall\":1,\"kind\":\"door\",\"at\":200,\"width\":80,\"side\":\"left\",\"hinge\":\"near\"} \
             (kind is door or window; at is centimetres along the wall from its `from` end to the \
             middle of the opening; width, side (left or right) and hinge (near or far) optional); \
             {\"op\":\"trace_area\",\"area\":\"kitchen\",\"points\":[[x,y],[x,y],[x,y]],\"tint\":\"sky\"} \
             (area is the id of a room the home already has; at least three corners, not repeating \
             the first; tint optional: ember, moss, slate, sand, plum, teal, rose, sky, olive or stone); \
             {\"op\":\"remove_wall\",\"wall\":1}; {\"op\":\"remove_opening\",\"wall\":1,\"opening\":1}; \
             {\"op\":\"remove_area\",\"area\":\"kitchen\"}. Whole centimetres, x rightwards and y down \
             the page. Walls and openings are numbered from 1 as read_floorplan lists them, all \
             the way through a call; the walls a call adds are numbered on from the last.",
            true,
        )],
        offered: |scope, drawing| plan(scope) && drawing,
    },
    Tool {
        name: "edit_home",
        description: "Add floors and rooms to the home, or ask for one to be removed. What is added is saved at once. Nothing is removed by this: the person is asked to confirm on the page first.",
        params: &[(
            "ops",
            "A JSON array of changes. Each is an object with an `op`: \
             {\"op\":\"add_floor\",\"name\":\"Upstairs\",\"level\":1} (level 0 is the entrance \
             floor, negative is below ground; optional); \
             {\"op\":\"add_area\",\"name\":\"Study\",\"floor\":\"ground\"} (floor is a floor's id; \
             left out, the floor being looked at); {\"op\":\"remove_area\",\"area\":\"study\"}; \
             {\"op\":\"remove_floor\",\"floor\":\"attic\"}. What comes back gives the id of each \
             floor and room that was made.",
            true,
        )],
        offered: |scope, drawing| plan(scope) && drawing,
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

/// Whether the conversation `scope` is offered the tool `name`. `drawing` is whether the
/// person asking sent the plan they are editing.
pub fn tool_offered(scope: &str, drawing: bool, name: &str) -> bool {
    TOOLS
        .iter()
        .any(|tool| tool.name == name && (tool.offered)(scope, drawing))
}

/// The tools the conversation `scope` is offered, in OpenAI's shape. Anthropic uses
/// [`anthropic_tools`].
pub fn openai_tools(scope: &str, drawing: bool) -> Value {
    TOOLS
        .iter()
        .filter(|tool| (tool.offered)(scope, drawing))
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

pub fn anthropic_tools(scope: &str, drawing: bool) -> Value {
    TOOLS
        .iter()
        .filter(|tool| (tool.offered)(scope, drawing))
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
            "read_floorplan",
        ];
        for scope in ["device:lamp", "automation:kettle"] {
            assert_eq!(names(&openai_tools(scope, false), "/function/name"), home);
            assert_eq!(names(&anthropic_tools(scope, false), "/name"), home);
        }
        let all = |scope| names(&openai_tools(scope, false), "/function/name");
        assert_eq!(all("general"), general);
        assert_eq!(names(&anthropic_tools("general", false), "/name"), general);
        assert_eq!(all("settings"), settings);
        assert_eq!(
            names(&anthropic_tools("settings", false), "/name"),
            settings
        );
        let logs = &anthropic_tools("settings", false)[2];
        assert_eq!(logs["input_schema"]["required"], json!(["source"]));
        assert!(tool_offered("settings", false, "read_logs"));
        assert!(tool_offered("general", false, "read_logs"));
        assert!(tool_offered("general", false, "get_automation"));
        assert!(!tool_offered("device:lamp", false, "read_logs"));
        assert!(!tool_offered(
            "automation:kettle",
            false,
            "list_automations"
        ));
        assert!(!tool_offered("settings", false, "read_settings"));
        assert!(!tool_offered("settings", false, "recent_states"));
        assert!(logs["input_schema"]["properties"]["contains"].is_object());
    }

    /// The one tool that changes anything is offered in one place: the chat on the Floorplan
    /// page, and only while the person there is editing and has sent what they are drawing.
    #[test]
    fn drawing_is_offered_only_on_a_plan_that_is_being_edited() {
        let reading = [
            "list_devices",
            "get_device",
            "recent_states",
            "list_automations",
            "get_automation",
            "read_floorplan",
        ];
        assert_eq!(
            names(&anthropic_tools("floorplan:ground", false), "/name"),
            reading
        );
        let drawing = names(&openai_tools("floorplan:ground", true), "/function/name");
        assert_eq!(drawing[..reading.len()], reading);
        assert_eq!(
            drawing[reading.len()..],
            ["edit_floorplan", "edit_home"],
            "the two that change anything"
        );
        for scope in ["general", "settings", "device:lamp", "automation:kettle"] {
            assert!(!tool_offered(scope, true, "edit_floorplan"), "{scope}");
            assert!(!tool_offered(scope, true, "edit_home"), "{scope}");
        }
        assert!(!tool_offered("floorplan:ground", false, "edit_home"));
        assert!(!tool_offered("floorplan:ground", false, "edit_floorplan"));
        assert!(tool_offered("general", false, "read_floorplan"));
        let edit = &anthropic_tools("floorplan:ground", true)[6];
        assert_eq!(edit["input_schema"]["required"], json!(["ops"]));
    }

    #[test]
    fn a_third_round_is_refused() {
        assert!(execute_round(0));
        assert!(execute_round(1));
        assert!(!execute_round(2));
    }
}
