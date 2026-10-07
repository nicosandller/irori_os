//! What `assistant.toml` says, and when that counts as a model the UI may use.

use serde::{Deserialize, Serialize};

/// The on-device tag a new install offers. Qwen3 1.7B, Q4_K_M, about 1.4 GB, Apache-2.0.
pub const DEFAULT_TAG: &str = "qwen3:1.7b";

/// How much conversation a local model is given room for, in tokens, until the person says
/// otherwise. Asked for by name on every request, so what is estimated here is what Ollama
/// sets aside.
pub const LOCAL_CONTEXT: u64 = 4096;
/// The least and the most context a person may ask for. Under the least, the picture of the
/// home alone doesn't fit.
pub const CONTEXT_MIN: u64 = 2048;
pub const CONTEXT_MAX: u64 = 262_144;
/// The longest the person's own instructions may be, in characters. They go in front of every
/// question, so they come out of the room a model has for the conversation.
pub const INSTRUCTIONS_MAX: usize = 4_000;

/// Memory left for the rest of the machine once a model is loaded.
const HEADROOM: u64 = 200 * 1024 * 1024;
/// The working buffers a loaded model keeps besides its weights and its context.
const WORKING: u64 = 64 * 1024 * 1024;

/// The numbers in a model's own description that decide how much memory its context takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub layers: u64,
    pub kv_heads: u64,
    pub key_length: u64,
    pub value_length: u64,
}

/// What a model takes once loaded with room for `context` tokens: its weights, the memory of
/// the conversation, and its working buffers. The conversation's part is one key and one value
/// per token, layer and head, two bytes a number. Without the model's shape, a fifth of the
/// weights and half a gigabyte stand in for it at the usual context, and grow with it.
pub fn local_needs(weight_bytes: u64, shape: Option<Shape>, context: u64) -> u64 {
    let context = match shape {
        Some(shape) => context
            .saturating_mul(shape.layers)
            .saturating_mul(shape.kv_heads)
            .saturating_mul(shape.key_length + shape.value_length)
            .saturating_mul(2),
        None => (weight_bytes / 5 + 512 * 1024 * 1024).saturating_mul(context) / LOCAL_CONTEXT,
    };
    weight_bytes.saturating_add(context).saturating_add(WORKING)
}

/// Which way the assistant is meant to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Local,
    Cloud,
}

/// A cloud preset. `Compatible` is any OpenAI-shaped endpoint the person names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudPreset {
    #[default]
    Openai,
    Anthropic,
    Grok,
    Compatible,
}

impl CloudPreset {
    /// The endpoint a preset fills in. `Compatible` has none of its own.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Openai => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
            Self::Grok => "https://api.x.ai/v1",
            Self::Compatible => "",
        }
    }

    pub fn is_anthropic(self) -> bool {
        matches!(self, Self::Anthropic)
    }
}

/// `assistant.toml`. The key is not in here; it lives in `secrets.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantFile {
    #[serde(default)]
    pub mode: Mode,
    /// What the person wants the assistant to keep to in every conversation, in their words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(default)]
    pub local: LocalFile,
    #[serde(default)]
    pub cloud: CloudFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalFile {
    #[serde(default = "default_tag")]
    pub tag: String,
    /// How many tokens of conversation the model is loaded with room for.
    #[serde(default = "default_context")]
    pub context: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudFile {
    #[serde(default)]
    pub preset: CloudPreset,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}

fn default_tag() -> String {
    DEFAULT_TAG.to_owned()
}

fn default_context() -> u64 {
    LOCAL_CONTEXT
}

impl Default for LocalFile {
    fn default() -> Self {
        Self {
            tag: default_tag(),
            context: default_context(),
        }
    }
}

impl Default for CloudFile {
    fn default() -> Self {
        Self {
            preset: CloudPreset::Openai,
            base_url: CloudPreset::Openai.default_base_url().to_owned(),
            model: String::new(),
        }
    }
}

impl Default for AssistantFile {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            instructions: String::new(),
            local: LocalFile::default(),
            cloud: CloudFile::default(),
        }
    }
}

impl AssistantFile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        file.validated()
    }

    /// Fills an empty cloud URL from the preset, and refuses a blank or oversized tag, a
    /// context outside what a model can be loaded with, and instructions too long to send.
    pub fn validated(mut self) -> Result<Self, String> {
        let tag = model_tag(&self.local.tag);
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/');
        if tag.is_empty() || tag.len() > 128 || !tag.chars().all(plain) {
            return Err(
                "a local model is an Ollama tag such as qwen3:1.7b, or the address of its page"
                    .to_owned(),
            );
        }
        self.local.tag = tag;
        if !(CONTEXT_MIN..=CONTEXT_MAX).contains(&self.local.context) {
            return Err(format!(
                "a local model's context is between {CONTEXT_MIN} and {CONTEXT_MAX} tokens"
            ));
        }
        self.instructions = self.instructions.trim().to_owned();
        if self.instructions.chars().count() > INSTRUCTIONS_MAX {
            return Err(format!(
                "the assistant's instructions can be {INSTRUCTIONS_MAX} characters at most"
            ));
        }
        if self.cloud.base_url.trim().is_empty() {
            self.cloud.base_url = self.cloud.preset.default_base_url().to_owned();
        }
        self.cloud.base_url = self.cloud.base_url.trim().trim_end_matches('/').to_owned();
        self.cloud.model = self.cloud.model.trim().to_owned();
        if self.cloud.model.len() > 128 {
            return Err("a cloud model name is too long".to_owned());
        }
        Ok(self)
    }

    pub fn to_toml(&self) -> Result<String, String> {
        let body = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        Ok(format!(
            "# How Irori's assistant answers. The API key, if there is one, is in secrets.toml\n\
             # under [assistant], not in this file.\n\
             #\n\
             # Written by Irori, and yours to edit. See docs/specs/config.md.\n\n{body}"
        ))
    }
}

/// A cloud model is configured when the endpoint, the model name, and a key are all present.
pub fn cloud_ready(file: &AssistantFile, has_key: bool) -> bool {
    file.mode == Mode::Cloud
        && has_key
        && !file.cloud.model.is_empty()
        && !file.cloud.base_url.is_empty()
}

/// The context a model is loaded with: what the person asked for, and no more than the model
/// itself was made for, when it says.
pub fn local_context(asked: u64, model_most: Option<u64>) -> u64 {
    model_most.map_or(asked, |most| asked.min(most.max(CONTEXT_MIN)))
}

/// How much room a local model's context leaves, next to the usual one. What a model on this
/// machine is handed is cut to fit the usual context; with more, it is handed more.
pub fn context_scale(context: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let scale = context as f64 / LOCAL_CONTEXT as f64;
    scale.clamp(0.5, 8.0)
}

/// Whether a model that `needs` this much can be loaded into `memory_free`. One that is
/// already loaded fits by being there: the memory it holds is why less is free.
pub fn local_fits(needs: u64, memory_free: u64, loaded: bool) -> bool {
    loaded || (needs > 0 && memory_free >= needs.saturating_add(HEADROOM))
}

/// The name Ollama pulls, from whatever was pasted: a tag as it is, and a model's page as the
/// tag it is the page of. `https://ollama.com/library/qwen3:1.7b` is `qwen3:1.7b`, and
/// `https://huggingface.co/someone/some-GGUF` is `hf.co/someone/some-GGUF`.
pub fn model_tag(pasted: &str) -> String {
    let text = pasted.trim();
    let text = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let text = text.split(['?', '#']).next().unwrap_or(text);
    let text = text.trim_end_matches('/');
    let text = text.strip_prefix("www.").unwrap_or(text);
    if let Some(rest) = text.strip_prefix("ollama.com/library/") {
        return rest.to_owned();
    }
    if let Some(rest) = text.strip_prefix("ollama.com/") {
        return rest.to_owned();
    }
    if let Some(rest) = text.strip_prefix("huggingface.co/") {
        return format!("hf.co/{rest}");
    }
    text.to_owned()
}

/// The library page for a tag: `qwen3:1.7b` → the model card that includes that tag.
pub fn library_page(tag: &str) -> String {
    if tag.starts_with("hf.co/") {
        format!("https://{tag}")
    } else if tag.contains('/') {
        format!("https://ollama.com/{tag}")
    } else {
        format!("https://ollama.com/library/{tag}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_small_default() {
        let file = AssistantFile::default();
        assert_eq!(file.mode, Mode::Off);
        assert_eq!(file.local.tag, "qwen3:1.7b");
        assert!(!cloud_ready(&file, true));
    }

    #[test]
    fn cloud_is_ready_only_with_a_model_a_url_and_a_key() {
        let mut file = AssistantFile {
            mode: Mode::Cloud,
            cloud: CloudFile {
                model: "gpt-4o-mini".into(),
                ..CloudFile::default()
            },
            ..AssistantFile::default()
        };
        assert!(!cloud_ready(&file, false));
        assert!(cloud_ready(&file, true));
        file.cloud.model.clear();
        assert!(!cloud_ready(&file, true));
    }

    #[test]
    fn a_blank_url_is_filled_from_the_preset() {
        let file = AssistantFile::parse(
            "mode = \"cloud\"\n[cloud]\nmodel = \"grok-3\"\npreset = \"grok\"\n",
        )
        .expect("the grok preset parses");
        assert_eq!(file.cloud.base_url, "https://api.x.ai/v1");
    }

    #[test]
    fn a_tag_with_a_space_is_refused() {
        let error =
            AssistantFile::parse("[local]\ntag = \"qwen 3\"\n").expect_err("a space is not a tag");
        assert!(error.contains("Ollama tag"), "{error}");
    }

    #[test]
    fn a_pasted_page_address_is_the_tag_it_is_the_page_of() {
        assert_eq!(model_tag(" qwen3:1.7b "), "qwen3:1.7b");
        assert_eq!(
            model_tag("https://ollama.com/library/gemma3:1b"),
            "gemma3:1b"
        );
        assert_eq!(
            model_tag("https://ollama.com/someone/tiny:latest/"),
            "someone/tiny:latest"
        );
        assert_eq!(
            model_tag("https://huggingface.co/bartowski/SmolLM2-GGUF?show=1"),
            "hf.co/bartowski/SmolLM2-GGUF"
        );
        let file =
            AssistantFile::parse("[local]\ntag = \"https://ollama.com/library/gemma3:1b\"\n")
                .expect("a page address is a tag");
        assert_eq!(file.local.tag, "gemma3:1b");
        assert_eq!(
            library_page("hf.co/bartowski/SmolLM2-GGUF"),
            "https://hf.co/bartowski/SmolLM2-GGUF"
        );
    }

    #[test]
    fn a_model_fits_when_free_memory_covers_what_it_needs_and_some_room() {
        let needs = 1_900_000_000;
        assert!(!local_fits(needs, needs, false));
        assert!(local_fits(needs, needs + HEADROOM, false));
        assert!(!local_fits(0, u64::MAX, false));
    }

    #[test]
    fn a_loaded_model_fits_however_little_is_left() {
        // A 4 GB Pi with qwen3:1.7b loaded: 1.66 GB free, which is less than it needs.
        let needs = local_needs(1_359_293_444, None, LOCAL_CONTEXT);
        assert!(!local_fits(needs, 1_660_000_000, false));
        assert!(local_fits(needs, 1_660_000_000, true));
    }

    #[test]
    fn the_estimate_is_what_ollama_reports_once_loaded() {
        // qwen3:1.7b: 28 layers, 8 heads, 128 + 128 a head. Ollama says 1,882,424,605 loaded.
        let shape = Shape {
            layers: 28,
            kv_heads: 8,
            key_length: 128,
            value_length: 128,
        };
        let needs = local_needs(1_359_293_444, Some(shape), LOCAL_CONTEXT);
        assert_eq!(needs, 1_359_293_444 + 448 * 1024 * 1024 + WORKING);
        assert!(needs.abs_diff(1_882_424_605) < 32 * 1024 * 1024, "{needs}");
        // Twice the context is twice the memory the conversation takes, and nothing else.
        let doubled = local_needs(1_359_293_444, Some(shape), 2 * LOCAL_CONTEXT);
        assert_eq!(doubled - needs, 448 * 1024 * 1024);
        let unknown = local_needs(1_000_000_000, None, LOCAL_CONTEXT);
        assert!(local_needs(1_000_000_000, None, 2 * LOCAL_CONTEXT) > unknown);
    }

    #[test]
    fn a_file_from_before_context_and_instructions_reads_as_the_defaults() {
        let file = AssistantFile::parse("mode = \"local\"\n[local]\ntag = \"gemma3:1b\"\n")
            .expect("an older file still parses");
        assert_eq!(file.local.context, LOCAL_CONTEXT);
        assert_eq!(file.instructions, "");
        // And one that was never given instructions doesn't grow an empty line for them.
        assert!(!file.to_toml().expect("writes").contains("instructions"));
    }

    #[test]
    fn context_and_instructions_are_kept_and_kept_within_bounds() {
        let file = AssistantFile {
            instructions: "  Answer in Spanish.\nCall me Nico.  ".into(),
            local: LocalFile {
                context: 8192,
                ..LocalFile::default()
            },
            ..AssistantFile::default()
        };
        let file = file.validated().expect("both are fine");
        assert_eq!(file.instructions, "Answer in Spanish.\nCall me Nico.");
        let again = AssistantFile::parse(&file.to_toml().expect("writes")).expect("reads back");
        assert_eq!(again, file);

        let small = AssistantFile {
            local: LocalFile {
                context: 512,
                ..LocalFile::default()
            },
            ..AssistantFile::default()
        };
        assert!(
            small
                .validated()
                .expect_err("too small")
                .contains("context")
        );
        let wordy = AssistantFile {
            instructions: "x".repeat(INSTRUCTIONS_MAX + 1),
            ..AssistantFile::default()
        };
        assert!(
            wordy
                .validated()
                .expect_err("too long")
                .contains("instructions")
        );
    }

    #[test]
    fn a_model_is_never_loaded_with_more_context_than_it_was_made_for() {
        assert_eq!(local_context(8192, None), 8192);
        assert_eq!(local_context(8192, Some(40_960)), 8192);
        assert_eq!(local_context(65_536, Some(32_768)), 32_768);
        assert_eq!(context_scale(LOCAL_CONTEXT), 1.0);
        assert_eq!(context_scale(8192), 2.0);
        assert_eq!(context_scale(1_000_000), 8.0);
    }

    #[test]
    fn the_default_tag_links_to_its_library_card() {
        assert_eq!(
            library_page(DEFAULT_TAG),
            "https://ollama.com/library/qwen3:1.7b"
        );
    }
}
