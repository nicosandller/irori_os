//! Generates Zigbee2MQTT's own `configuration.yaml` from this extension's settings. Hand-built
//! rather than pulled in through a YAML crate: the shape needed is small and fixed, so a plain
//! string is simpler than a new dependency for it.

use crate::settings::Settings;

/// `broker_port`/`base_topic` are passed separately from `settings` because they're this
/// extension's own choice of how Z2M reaches the embedded broker, not something a person sets
/// directly (`base_topic` is fixed to `"zigbee2mqtt"`, matching what `irori-ha-discovery`'s
/// bridge helpers assume).
///
/// `existing_advanced` is the `advanced:` block's own body, read back out of this same file from
/// the *previous* run (see `existing_advanced_block`) — Zigbee2MQTT generates and persists its
/// own `network_key`/`pan_id` back into this file when neither is configured, and every restart
/// calls this function fresh, so without carrying that block forward, every restart would
/// silently erase Zigbee2MQTT's own generated network identity and it would generate (and
/// persist) a brand new one, dropping every paired device. Every key in that block is kept, not
/// just the ones this extension has settings for (`carried_forward` says why); a setting given
/// here always wins over what's in the file.
pub fn generate(
    settings: &Settings,
    broker_port: u16,
    base_topic: &str,
    existing_advanced: Option<&str>,
) -> Result<String, String> {
    let mut yaml = String::new();
    yaml.push_str("homeassistant:\n  enabled: true\n");
    yaml.push_str(&format!(
        "mqtt:\n  server: \"mqtt://127.0.0.1:{broker_port}\"\n  base_topic: \"{}\"\n",
        escape(base_topic)
    ));
    yaml.push_str(&format!(
        "serial:\n  port: \"{}\"\n  adapter: {}\n",
        escape(&settings.serial_port),
        settings.adapter.as_str()
    ));
    yaml.push_str("frontend:\n  enabled: false\n");
    // Irori's own permit_join action is the way to open this, not the config file — starting
    // closed means a freshly (re)started Z2M never silently accepts joins on its own.
    yaml.push_str("permit_join: false\n");

    let mut advanced = String::new();
    // Which keys this extension's own settings have an opinion about. Everything else in an
    // existing `advanced:` block is carried over untouched, below.
    let mut decided: Vec<&str> = Vec::new();
    if let Some(channel) = settings.channel {
        advanced.push_str(&format!("  channel: {channel}\n"));
        decided.push("channel");
    }
    if let Some(pan_id) = settings.pan_id {
        advanced.push_str(&format!("  pan_id: {pan_id}\n"));
        decided.push("pan_id");
    }
    if let Some(key) = &settings.network_key {
        let bytes = parse_network_key(key.expose())?;
        let listed = bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        advanced.push_str(&format!("  network_key: [{listed}]\n"));
        decided.push("network_key");
    }
    advanced.push_str(&carried_forward(existing_advanced, &decided));
    if !advanced.is_empty() {
        yaml.push_str("advanced:\n");
        yaml.push_str(&advanced);
    }
    Ok(yaml)
}

/// Everything in a previous `advanced:` block that `decided` doesn't replace, verbatim.
///
/// Every key it finds, rather than a list of the ones this module knows about: `pan_id` and
/// `network_key` are only part of what Zigbee2MQTT generates and writes back as the network's
/// identity — `ext_pan_id` is another, and a later Zigbee2MQTT may persist more. Losing any of
/// them on a restart hands the radio a different network, and every paired device is gone. So the
/// rule is to keep whatever is there unless this extension's own settings say otherwise.
fn carried_forward(existing: Option<&str>, decided: &[&str]) -> String {
    let Some(existing) = existing else {
        return String::new();
    };
    let mut kept = String::new();
    // Whether the key currently being read is one the settings replaced — and so whether the
    // lines nested under it are being dropped along with it.
    let mut replacing = false;
    for line in existing.split_inclusive('\n') {
        match key_of(line) {
            Some(key) => {
                replacing = decided.contains(&key);
                if !replacing {
                    kept.push_str(line);
                }
            }
            // Not a key line of its own: a nested mapping's contents, a list item, or a blank
            // line — all belonging to whichever key came before it.
            None => {
                if !replacing && !line.trim().is_empty() {
                    kept.push_str(line);
                }
            }
        }
    }
    kept
}

/// The key of one `  key: value` line at an `advanced:` body's own indentation — the convention
/// this module writes and `existing_advanced_block` reads back. `None` for anything indented
/// deeper (a nested mapping's own lines) or not a key at all.
fn key_of(line: &str) -> Option<&str> {
    let (key, _) = line.strip_prefix("  ")?.split_once(':')?;
    (!key.is_empty() && !key.starts_with(char::is_whitespace)).then_some(key)
}

/// Extracts an existing `configuration.yaml`'s `advanced:` block body — its `  key: value` lines,
/// exactly as written — so a fresh `generate()` call can carry forward whatever's there that the
/// current settings don't explicitly override. `None` if `yaml` has no `advanced:` section at
/// all (nothing to carry forward).
pub fn existing_advanced_block(yaml: &str) -> Option<String> {
    let marker = "advanced:\n";
    let start = yaml.find(marker)? + marker.len();
    let mut block = String::new();
    for line in yaml[start..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if trimmed.is_empty() || line.starts_with("  ") {
            block.push_str(line);
        } else {
            break;
        }
    }
    if block.trim().is_empty() {
        None
    } else {
        Some(block)
    }
}

/// A network key as Zigbee2MQTT's config wants it: 16 bytes, as a YAML array — not the hex
/// string a person would actually have to hand (copied from another Z2M's own config, say), so
/// this accepts hex and converts it.
fn parse_network_key(text: &str) -> Result<[u8; 16], String> {
    let hex: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(
            "a network key is 16 bytes as hex (32 hex digits) — e.g. copied from another \
             Zigbee2MQTT's own configuration.yaml"
                .to_owned(),
        );
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .expect("already validated as hex digits");
    }
    Ok(bytes)
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Adapter;

    fn settings() -> Settings {
        serde_json::from_value(serde_json::json!({ "serial_port": "/dev/ttyUSB0" })).expect("valid")
    }

    #[test]
    fn generates_the_minimal_config_with_only_a_serial_port() {
        let yaml = generate(&settings(), 17_883, "zigbee2mqtt", None).expect("generates");
        assert!(yaml.contains("server: \"mqtt://127.0.0.1:17883\""));
        assert!(yaml.contains("base_topic: \"zigbee2mqtt\""));
        assert!(yaml.contains("port: \"/dev/ttyUSB0\""));
        assert!(yaml.contains("adapter: ember"));
        assert!(yaml.contains("permit_join: false"));
        assert!(!yaml.contains("advanced:"), "nothing to put there: {yaml}");
    }

    #[test]
    fn includes_channel_and_pan_id_when_given() {
        let mut settings = settings();
        settings.channel = Some(25);
        settings.pan_id = Some(6754);
        let yaml = generate(&settings, 17_883, "zigbee2mqtt", None).expect("generates");
        assert!(yaml.contains("advanced:"));
        assert!(yaml.contains("channel: 25"));
        assert!(yaml.contains("pan_id: 6754"));
    }

    #[test]
    fn converts_a_hex_network_key_to_the_byte_array_z2m_wants() {
        let mut settings = settings();
        settings.network_key = Some(
            serde_json::from_value(serde_json::json!(
                "01:02:03:04:05:06:07:08:09:0a:0b:0c:0d:0e:0f:10"
            ))
            .expect("valid"),
        );
        let yaml = generate(&settings, 17_883, "zigbee2mqtt", None).expect("generates");
        assert!(
            yaml.contains("network_key: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]"),
            "{yaml}"
        );
    }

    #[test]
    fn a_malformed_network_key_is_a_named_error_not_a_panic() {
        let mut settings = settings();
        settings.network_key =
            Some(serde_json::from_value(serde_json::json!("not hex at all")).expect("valid"));
        let error = generate(&settings, 17_883, "zigbee2mqtt", None).expect_err("bad key");
        assert!(error.contains("16 bytes as hex"));
    }

    #[test]
    fn a_generated_network_key_is_carried_forward_when_settings_doesnt_specify_one() {
        // What a previous run's file looks like once Zigbee2MQTT has generated and persisted
        // its own network identity into it (settings themselves never asked for a key).
        let previous = generate(&settings(), 17_883, "zigbee2mqtt", None).expect("generates");
        let with_generated_key = format!(
            "{previous}advanced:\n  network_key: [11, 22, 33, 44, 55, 66, 77, 88, 99, 100, \
             111, 122, 133, 144, 155, 166]\n"
        );
        let existing = existing_advanced_block(&with_generated_key);

        let regenerated =
            generate(&settings(), 17_883, "zigbee2mqtt", existing.as_deref()).expect("generates");

        assert!(
            regenerated.contains(
                "network_key: [11, 22, 33, 44, 55, 66, 77, 88, 99, 100, 111, 122, 133, 144, \
                 155, 166]"
            ),
            "the previously generated key should have carried forward: {regenerated}"
        );
    }

    #[test]
    fn an_explicit_network_key_setting_overrides_whatever_was_there_before() {
        let existing = Some("  network_key: [9, 9, 9]\n".to_owned());
        let mut settings = settings();
        settings.network_key = Some(
            serde_json::from_value(serde_json::json!(
                "01:02:03:04:05:06:07:08:09:0a:0b:0c:0d:0e:0f:10"
            ))
            .expect("valid"),
        );
        let yaml =
            generate(&settings, 17_883, "zigbee2mqtt", existing.as_deref()).expect("generates");
        assert!(!yaml.contains("[9, 9, 9]"), "{yaml}");
        assert!(
            yaml.contains("network_key: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]"),
            "{yaml}"
        );
    }

    /// Zigbee2MQTT's generated network identity is more than the two keys this extension has
    /// settings for: `ext_pan_id` is part of the same thing, and dropping it on a restart loses
    /// every paired device just as surely as dropping the network key would.
    #[test]
    fn everything_zigbee2mqtt_wrote_for_itself_carries_forward_not_just_the_known_keys() {
        let previous = "  ext_pan_id: [221, 221, 221, 221, 221, 221, 221, 221]\n  \
                        network_key: [11, 22, 33]\n  transmit_power: 20\n  \
                        something_a_later_z2m_added: true\n";
        let yaml = generate(&settings(), 17_883, "zigbee2mqtt", Some(previous)).expect("generates");
        for kept in [
            "ext_pan_id: [221, 221, 221, 221, 221, 221, 221, 221]",
            "network_key: [11, 22, 33]",
            "transmit_power: 20",
            "something_a_later_z2m_added: true",
        ] {
            assert!(yaml.contains(kept), "lost `{kept}`: {yaml}");
        }
    }

    /// A setting replaces its key and anything nested under it, and nothing else.
    #[test]
    fn a_replaced_key_takes_its_own_nested_lines_with_it() {
        let previous = "  channel: 11\n    stale_detail: 1\n  ext_pan_id: [1, 2]\n";
        let mut settings = settings();
        settings.channel = Some(25);
        let yaml = generate(&settings, 17_883, "zigbee2mqtt", Some(previous)).expect("generates");
        assert!(yaml.contains("channel: 25"), "{yaml}");
        assert!(!yaml.contains("channel: 11"), "{yaml}");
        assert!(!yaml.contains("stale_detail"), "{yaml}");
        assert!(yaml.contains("ext_pan_id: [1, 2]"), "{yaml}");
    }

    #[test]
    fn existing_advanced_block_extracts_just_that_sections_body() {
        let yaml = "homeassistant:\n  enabled: true\nadvanced:\n  channel: 25\n  pan_id: 6754\n";
        assert_eq!(
            existing_advanced_block(yaml),
            Some("  channel: 25\n  pan_id: 6754\n".to_owned())
        );
    }

    #[test]
    fn no_advanced_section_at_all_is_none() {
        let yaml = "homeassistant:\n  enabled: true\n";
        assert_eq!(existing_advanced_block(yaml), None);
    }

    #[test]
    fn adapter_names_match_what_zigbee2mqtt_expects() {
        assert_eq!(Adapter::Ember.as_str(), "ember");
        assert_eq!(Adapter::Zstack.as_str(), "zstack");
        assert_eq!(Adapter::Deconz.as_str(), "deconz");
        assert_eq!(Adapter::Zboss.as_str(), "zboss");
    }
}
