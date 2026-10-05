//! What `assistant.toml` says, and when that counts as a model the UI may use.

use serde::{Deserialize, Serialize};

/// The on-device tag a new install offers. Qwen3 1.7B, Q4_K_M, about 1.4 GB, Apache-2.0.
pub const DEFAULT_TAG: &str = "qwen3:1.7b";

/// Extra free memory a pulled model needs beyond its file, for the cache beside the weights.
const HEADROOM: u64 = 400 * 1024 * 1024;

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

impl Default for LocalFile {
    fn default() -> Self {
        Self { tag: default_tag() }
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

    /// Fills an empty cloud URL from the preset, and refuses a blank or oversized tag.
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

/// Whether a pulled model can be served. `weight_bytes` is what Ollama reported for the tag.
pub fn local_fits(weight_bytes: u64, memory_free: u64) -> bool {
    weight_bytes > 0 && memory_free.saturating_add(1) > weight_bytes.saturating_add(HEADROOM)
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
    fn a_model_fits_when_free_memory_covers_the_weights_and_a_cache() {
        let weight = 1_400_000_000;
        assert!(!local_fits(weight, weight));
        assert!(local_fits(weight, weight + HEADROOM));
        assert!(!local_fits(0, u64::MAX));
    }

    #[test]
    fn the_default_tag_links_to_its_library_card() {
        assert_eq!(
            library_page(DEFAULT_TAG),
            "https://ollama.com/library/qwen3:1.7b"
        );
    }
}
