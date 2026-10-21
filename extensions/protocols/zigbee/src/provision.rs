//! Downloads a managed Node.js runtime and installs Zigbee2MQTT, entirely within this
//! extension's own package directory (`./runtime`, `./z2m`), through `irori-node-runtime`.
//! Never touches any Node.js the host might already have.

use irori_node_runtime::install::{self, Package};

pub use irori_node_runtime::install::node_binary;

/// Zigbee2MQTT, as installed beside the managed Node.js.
const ZIGBEE2MQTT: Package = Package {
    name: "zigbee2mqtt",
    title: "Zigbee2MQTT",
    dir: "z2m",
    project: "irori-zigbee2mqtt",
    entry: "index.js",
    // pnpm blocks native postinstall build scripts by default; Zigbee2MQTT needs exactly two —
    // the serial port driver and one of its transitive Zigbee stack deps — verified by hand
    // (`pnpm add` without this fails with `ERR_PNPM_IGNORED_BUILDS`, naming these two).
    allow_builds: &["@serialport/bindings-cpp", "unix-dgram"],
    decline_builds: &[],
};

pub async fn ensure_node() -> Result<(), String> {
    install::ensure_node().await
}

/// The zigbee2mqtt package's entry point, once `ensure_zigbee2mqtt` has installed it.
pub fn zigbee2mqtt_entry() -> std::path::PathBuf {
    ZIGBEE2MQTT.entry()
}

/// Installs Zigbee2MQTT if it isn't there, or if `version` pins a different one than what's
/// installed. `None` installs the latest the first time and leaves it alone after that.
pub async fn ensure_zigbee2mqtt(version: Option<&str>) -> Result<(), String> {
    ZIGBEE2MQTT.ensure(version, "latest").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigbee2mqtt_runs_from_its_own_folder_beside_the_runtime() {
        assert_eq!(
            zigbee2mqtt_entry(),
            std::path::Path::new("z2m/node_modules/zigbee2mqtt/index.js")
        );
    }
}
