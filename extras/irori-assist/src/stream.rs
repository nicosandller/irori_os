//! One provider's bytes, in whatever size they arrived, turned into text or a tool call.
//!
//! A chunk may split a line in half. Each parser keeps the unfinished tail and only reads a
//! line once it has the newline. The tail is kept as bytes, so a character cut in half by a
//! chunk boundary is whole again by the time it is read.

/// What one complete event was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    /// A tool the model wants run. `arguments` is the JSON object, as text, possibly still
    /// arriving in pieces that the caller concatenates for the same `index`.
    Tool {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    /// How many tokens the provider counted: what it was sent, and what it wrote. Either may
    /// come alone, and more than once; the last of each is the one that stands.
    Usage {
        prompt: Option<u64>,
        output: Option<u64>,
    },
    Done,
}

/// Bytes as they arrive, handed back one whole line at a time.
///
/// It keeps bytes, not text: a chunk may also end halfway through a character, and decoding
/// that half on its own would spoil it.
#[derive(Debug, Default)]
pub struct Lines {
    pending: Vec<u8>,
}

impl Lines {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(at) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=at).collect();
            let mut line = String::from_utf8_lossy(&line[..at]).into_owned();
            if line.ends_with('\r') {
                line.pop();
            }
            lines.push(line);
        }
        lines
    }
}

/// OpenAI-compatible chat completions, `stream: true`.
#[derive(Debug, Default)]
pub struct OpenAiParser {
    buf: Lines,
}

impl OpenAiParser {
    pub fn push(&mut self, chunk: impl AsRef<[u8]>) -> Vec<Piece> {
        let mut pieces = Vec::new();
        for line in self.buf.push(chunk.as_ref()) {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                pieces.push(Piece::Done);
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            // Asked for with `stream_options`, it comes in a chunk of its own with no choices.
            let usage = &value["usage"];
            if usage.is_object() {
                pieces.push(Piece::Usage {
                    prompt: usage["prompt_tokens"].as_u64(),
                    output: usage["completion_tokens"].as_u64(),
                });
            }
            let Some(choice) = value["choices"].get(0) else {
                continue;
            };
            if let Some(text) = choice["delta"]["content"].as_str()
                && !text.is_empty()
            {
                pieces.push(Piece::Text(text.to_owned()));
            }
            if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"].as_u64().unwrap_or(0) as usize;
                    pieces.push(Piece::Tool {
                        index,
                        id: call["id"].as_str().map(str::to_owned),
                        name: call["function"]["name"].as_str().map(str::to_owned),
                        arguments: call["function"]["arguments"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned(),
                    });
                }
            }
        }
        pieces
    }
}

/// Ollama's own `/api/chat` stream: one JSON object per line.
#[derive(Debug, Default)]
pub struct OllamaParser {
    buf: Lines,
}

impl OllamaParser {
    pub fn push(&mut self, chunk: impl AsRef<[u8]>) -> Vec<Piece> {
        let mut pieces = Vec::new();
        for line in self.buf.push(chunk.as_ref()) {
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(text) = value["message"]["content"].as_str()
                && !text.is_empty()
            {
                pieces.push(Piece::Text(text.to_owned()));
            }
            if let Some(calls) = value["message"]["tool_calls"].as_array() {
                for (index, call) in calls.iter().enumerate() {
                    let arguments = call["function"]["arguments"].clone();
                    let arguments = if arguments.is_string() {
                        arguments.as_str().unwrap_or("").to_owned()
                    } else if arguments.is_null() {
                        String::new()
                    } else {
                        arguments.to_string()
                    };
                    pieces.push(Piece::Tool {
                        index,
                        id: None,
                        name: call["function"]["name"].as_str().map(str::to_owned),
                        arguments,
                    });
                }
            }
            if value["done"].as_bool() == Some(true) {
                let prompt = value["prompt_eval_count"].as_u64();
                let output = value["eval_count"].as_u64();
                if prompt.is_some() || output.is_some() {
                    pieces.push(Piece::Usage { prompt, output });
                }
                pieces.push(Piece::Done);
            }
        }
        pieces
    }
}

/// Anthropic's `/v1/messages` stream.
#[derive(Debug, Default)]
pub struct AnthropicParser {
    buf: Lines,
    /// The event name from the line above the data line.
    event: String,
}

impl AnthropicParser {
    pub fn push(&mut self, chunk: impl AsRef<[u8]>) -> Vec<Piece> {
        let mut pieces = Vec::new();
        for line in self.buf.push(chunk.as_ref()) {
            if let Some(name) = line.strip_prefix("event:") {
                self.event = name.trim().to_owned();
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
                continue;
            };
            match value["type"].as_str().unwrap_or(self.event.as_str()) {
                "content_block_start" => {
                    let block = &value["content_block"];
                    if block["type"] == "tool_use" {
                        pieces.push(Piece::Tool {
                            index: value["index"].as_u64().unwrap_or(0) as usize,
                            id: block["id"].as_str().map(str::to_owned),
                            name: block["name"].as_str().map(str::to_owned),
                            arguments: String::new(),
                        });
                    }
                }
                "content_block_delta" => {
                    let delta = &value["delta"];
                    if let Some(text) = delta["text"].as_str()
                        && !text.is_empty()
                    {
                        pieces.push(Piece::Text(text.to_owned()));
                    }
                    if delta["type"] == "input_json_delta" {
                        pieces.push(Piece::Tool {
                            index: value["index"].as_u64().unwrap_or(0) as usize,
                            id: None,
                            name: None,
                            arguments: delta["partial_json"].as_str().unwrap_or("").to_owned(),
                        });
                    }
                }
                // What it was sent is counted as the answer starts, with what it read from
                // its cache counted apart; what it wrote, as the answer ends.
                "message_start" => {
                    let usage = &value["message"]["usage"];
                    if usage.is_object() {
                        let read = |key: &str| usage[key].as_u64().unwrap_or(0);
                        pieces.push(Piece::Usage {
                            prompt: Some(
                                read("input_tokens")
                                    + read("cache_read_input_tokens")
                                    + read("cache_creation_input_tokens"),
                            ),
                            output: None,
                        });
                    }
                }
                "message_delta" => {
                    if let Some(output) = value["usage"]["output_tokens"].as_u64() {
                        pieces.push(Piece::Usage {
                            prompt: None,
                            output: Some(output),
                        });
                    }
                }
                "message_stop" => pieces.push(Piece::Done),
                _ => {}
            }
        }
        pieces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_openai_stream_split_across_chunks_still_joins() {
        let mut parser = OpenAiParser::default();
        assert!(
            parser
                .push("data: {\"choices\":[{\"delta\":{\"con")
                .is_empty()
        );
        let pieces = parser.push("tent\":\"Hi\"}}]}\n\ndata: [DONE]\n");
        assert_eq!(pieces, vec![Piece::Text("Hi".into()), Piece::Done]);
    }

    #[test]
    fn a_character_split_across_chunks_arrives_whole() {
        let line = "{\"message\":{\"content\":\"café 🙂\"},\"done\":false}\n".as_bytes();
        let cut = line.len() - 20;
        assert!(std::str::from_utf8(&line[..cut]).is_err());
        let mut parser = OllamaParser::default();
        assert!(parser.push(&line[..cut]).is_empty());
        assert_eq!(
            parser.push(&line[cut..]),
            vec![Piece::Text("café 🙂".into())]
        );
    }

    #[test]
    fn an_openai_tool_call_keeps_its_index_and_its_arguments() {
        let mut parser = OpenAiParser::default();
        let pieces = parser.push(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"get_device\",\"arguments\":\"{\\\"id\\\"\"}}]}}]}\n",
        );
        assert_eq!(
            pieces,
            vec![Piece::Tool {
                index: 0,
                id: Some("a".into()),
                name: Some("get_device".into()),
                arguments: "{\"id\"".into(),
            }]
        );
    }

    #[test]
    fn ollama_reports_text_and_then_that_it_is_done() {
        let mut parser = OllamaParser::default();
        let pieces = parser.push(
            "{\"message\":{\"content\":\"Hi\"},\"done\":false}\n{\"message\":{\"content\":\"\"},\"done\":true}\n",
        );
        assert_eq!(pieces, vec![Piece::Text("Hi".into()), Piece::Done]);
    }

    #[test]
    fn each_provider_says_how_many_tokens_it_counted() {
        let usage = |prompt, output| Piece::Usage { prompt, output };
        let mut ollama = OllamaParser::default();
        assert_eq!(
            ollama.push(
                "{\"message\":{\"content\":\"\"},\"done\":true,\"prompt_eval_count\":1200,\"eval_count\":80}\n"
            ),
            vec![usage(Some(1200), Some(80)), Piece::Done]
        );
        let mut openai = OpenAiParser::default();
        assert_eq!(
            openai.push(
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":900,\"completion_tokens\":40}}\n"
            ),
            vec![usage(Some(900), Some(40))]
        );
        // A chunk that carries no usage says `null` for it, which is not a count.
        assert!(
            openai
                .push("data: {\"choices\":[{\"delta\":{}}],\"usage\":null}\n")
                .is_empty()
        );
        let mut anthropic = AnthropicParser::default();
        assert_eq!(
            anthropic.push(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":700}}}\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":55}}\n"
            ),
            vec![usage(Some(710), None), usage(None, Some(55))]
        );
    }

    #[test]
    fn anthropic_text_deltas_are_the_answer() {
        let mut parser = AnthropicParser::default();
        let pieces = parser.push(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n",
        );
        assert_eq!(pieces, vec![Piece::Text("Hi".into())]);
    }
}
