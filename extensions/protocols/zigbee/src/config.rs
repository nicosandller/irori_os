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
/// persist) a brand new one, dropping every paired device. Only used when `settings` itself
/// doesn't say anything for the `advanced` section — an explicit setting always wins.
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
    if let Some(channel) = settings.channel {
        advanced.push_str(&format!("  channel: {channel}\n"));
    } else if let Some(line) = existing_line(existing_advanced, "channel") {
        advanced.push_str(line);
    }
    if let Some(pan_id) = settings.pan_id {
        advanced.push_str(&format!("  pan_id: {pan_id}\n"));
    } else if let Some(line) = existing_line(existing_advanced, "pan_id") {
        advanced.push_str(line);
    }
    if let Some(key) = &settings.network_key {
        let bytes = parse_network_key(key.expose())?;
        let listed = bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        advanced.push_str(&format!("  network_key: [{listed}]\n"));
    } else if let Some(line) = existing_line(existing_advanced, "network_key") {
        advanced.push_str(line);
    }
    if !advanced.is_empty() {
        yaml.push_str("advanced:\n");
        yaml.push_str(&advanced);
    }
    Ok(yaml)
}

/// Extracts one `  <key>: ...` line, verbatim with its trailing newline, out of `existing`'s
/// `advanced:` body — `existing_advanced_block`'s own indentation convention, matching what this
/// module itself writes. `None` if there's no existing block, or `key` isn't in it.
fn existing_line<'a>(existing: Option<&'a str>, key: &str) -> Option<&'a str> {
    let existing = existing?;
    let prefix = format!("  {key}:");
    existing
        .split_inclusive('\n')
        .find(|line| line.starts_with(&prefix))
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
