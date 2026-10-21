//! Installs the Open Home Foundation Matter Server (`matter-server` on npm) beside a managed
//! Node.js, through `irori-node-runtime`, and runs it. What's the server's own here is the
//! command line it takes and how its log says something went wrong.

use std::path::{Path, PathBuf};
use std::time::Duration;

use irori_node_runtime::child::{self, LogStyle};
use irori_node_runtime::install::{self, Package};
use tokio::process::{Child, Command};

use crate::settings::Settings;

pub use irori_node_runtime::child::Spawned;

/// The version this extension was tested with, installed when settings don't pin one.
pub const DEFAULT_VERSION: &str = "1.4.0";

/// The server, as installed beside the managed Node.js.
const MATTER_SERVER: Package = Package {
    name: "matter-server",
    title: "Matter server",
    dir: "matter",
    project: "irori-matter-server",
    // The file that runs it moved between releases (`dist/esm/MatterServer.js` in 1.4.0,
    // `dist/esm/bin.js` in 1.5.0), so the package's own `package.json` is what says it
    // (`entry_point`). Its presence is what tells an install happened.
    entry: "package.json",
    allow_builds: &[],
    // Bluetooth, serial and USB: native parts for commissioning over Bluetooth, which this
    // extension doesn't do yet. Without a decision pnpm refuses to install at all
    // (`ERR_PNPM_IGNORED_BUILDS`, naming exactly these five).
    decline_builds: &[
        "@serialport/bindings-cpp",
        "@stoprocent/bleno",
        "@stoprocent/bluetooth-hci-socket",
        "@stoprocent/noble",
        "usb",
    ],
};

const STYLE: LogStyle = LogStyle {
    name: "matter-server",
    error: error_sentence,
};

pub async fn ensure_node() -> Result<(), String> {
    install::ensure_node().await
}

/// Installs the server if it isn't there, or if `version` pins a different one than what's
/// installed. `None` installs [`DEFAULT_VERSION`] the first time and leaves it alone after.
pub async fn ensure_server(version: Option<&str>) -> Result<(), String> {
    MATTER_SERVER.ensure(version, DEFAULT_VERSION).await
}

/// The installed server's version, for the log.
pub fn installed_version() -> Option<String> {
    MATTER_SERVER.installed_version()
}

/// The file to hand to `node`, as the installed package's `package.json` names it.
pub fn entry_point() -> Result<PathBuf, String> {
    let package = MATTER_SERVER.package_dir();
    let text = std::fs::read_to_string(package.join("package.json"))
        .map_err(|e| format!("couldn't read the Matter server's package.json: {e}"))?;
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("the Matter server's package.json isn't JSON: {e}"))?;
    let named = entry_in(&manifest).ok_or("the Matter server's package.json names no program")?;
    // Inside the package, never out of it: it's a path someone else's file gives.
    let relative = Path::new(named);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(format!(
            "the Matter server's package.json names {named}, outside the package"
        ));
    }
    Ok(package.join(relative))
}

/// `bin["matter-server"]`, a lone `bin`, or `main`, in that order.
fn entry_in(manifest: &serde_json::Value) -> Option<&str> {
    let bin = manifest.get("bin");
    bin.and_then(|bin| bin.get("matter-server"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| bin.and_then(serde_json::Value::as_str))
        .or_else(|| manifest.get("main").and_then(serde_json::Value::as_str))
}

/// The server's command line, after the program: where it keeps the fabric, and the port it
/// listens on, on loopback only. Its WebSocket API has no authentication, so nothing off this
/// machine may reach it.
pub fn arguments(settings: &Settings, storage: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "--storage-path".into(),
        storage.into(),
        "--port".into(),
        settings.port.to_string().into(),
        "--listen-address".into(),
        "127.0.0.1".into(),
        // What other apps' fabric lists call Irori, once a device is shared with it.
        "--default-fabric-label".into(),
        "Irori".into(),
        "--disable-dashboard".into(),
        // Firmware updates come later, as the `update` kind; until then the server leaves the
        // devices' firmware alone.
        "--disable-ota".into(),
        "--log-level".into(),
        "notice".into(),
    ];
    if let Some(interface) = &settings.primary_interface {
        args.push("--primary-interface".into());
        args.push(interface.into());
    }
    if settings.allow_test_devices {
        args.push("--enable-test-net-dcl".into());
    }
    args
}

/// Starts the server.
pub fn spawn(node: &Path, entry: &Path, args: Vec<std::ffi::OsString>) -> Result<Spawned, String> {
    let mut command = Command::new(node);
    command
        .arg(entry)
        .args(args)
        // matter.js colours its log even into a pipe; plain is what `error_sentence` reads.
        .env("MATTER_LOG_FORMAT", "plain");
    child::spawn(command, STYLE)
}

/// How long the server has to shut down on its own before it's killed: matter-server 1.4.0
/// takes about 2.5 seconds, closing its storage last. Inside the five seconds the core gives an
/// extension to stop; reading its last log lines after a clean exit is immediate.
const SHUTDOWN: Duration = Duration::from_secs(3);

/// Asks the server to stop, and kills it if it hasn't within [`SHUTDOWN`]. It holds Irori's
/// fabric and every device's keys, so it gets the chance to finish writing them. `kill` the
/// command, since this workspace has no `unsafe` to send a signal with.
pub async fn stop(child: &mut Child) {
    if let Some(pid) = child.id() {
        let asked = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await;
        if asked.is_ok_and(|status| status.success())
            && tokio::time::timeout(SHUTDOWN, child.wait()).await.is_ok()
        {
            return;
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// The sentence of a line the server calls an error, if it is one. Its lines read
/// `2026-10-21 13:06:13.384 ERROR Facility message`.
fn error_sentence(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let (_date, _time, level) = (words.next()?, words.next()?, words.next()?);
    if !matches!(level, "ERROR" | "FATAL") {
        return None;
    }
    let facility = words.next()?;
    let rest: Vec<&str> = words.collect();
    Some(if rest.is_empty() {
        facility.to_owned()
    } else {
        rest.join(" ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_found_whichever_way_a_release_names_it() {
        let named = |manifest| entry_in(&manifest).map(str::to_owned);
        assert_eq!(
            named(serde_json::json!({"bin": {"matter-server": "dist/esm/bin.js"}})),
            Some("dist/esm/bin.js".to_owned()),
            "1.5.0"
        );
        assert_eq!(
            named(serde_json::json!({"main": "dist/esm/MatterServer.js"})),
            Some("dist/esm/MatterServer.js".to_owned()),
            "1.4.0"
        );
        assert_eq!(
            named(serde_json::json!({"bin": "cli.js"})),
            Some("cli.js".to_owned())
        );
        assert_eq!(named(serde_json::json!({})), None);
    }

    #[test]
    fn the_server_listens_on_loopback_and_takes_what_settings_say() {
        let plain = arguments(&Settings::default(), Path::new("/data/matter-server"));
        let text: Vec<String> = plain
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            text.iter()
                .position(|arg| arg == flag)
                .and_then(|at| text.get(at + 1))
                .cloned()
        };
        assert_eq!(after("--listen-address").as_deref(), Some("127.0.0.1"));
        assert_eq!(after("--port").as_deref(), Some("15580"));
        assert_eq!(
            after("--storage-path").as_deref(),
            Some("/data/matter-server")
        );
        assert!(!text.iter().any(|arg| arg == "--enable-test-net-dcl"));
        assert!(!text.iter().any(|arg| arg == "--primary-interface"));

        let lab = Settings {
            allow_test_devices: true,
            primary_interface: Some("eth0".to_owned()),
            ..Settings::default()
        };
        let text: Vec<String> = arguments(&lab, Path::new("/x"))
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(text.iter().any(|arg| arg == "--enable-test-net-dcl"));
        assert!(
            text.windows(2)
                .any(|pair| pair == ["--primary-interface", "eth0"])
        );
    }

    #[test]
    fn an_error_line_is_read_down_to_its_sentence() {
        assert_eq!(
            error_sentence(
                "2026-10-21 13:06:13.384 ERROR MatterServer Couldn't bind to port 15580"
            )
            .as_deref(),
            Some("Couldn't bind to port 15580")
        );
        assert_eq!(
            error_sentence("2026-10-21 13:06:13.384 NOTICE MatterServer Listening on 15580"),
            None
        );
        assert_eq!(error_sentence("ERROR"), None, "not the server's format");
    }

    #[tokio::test]
    async fn the_server_is_asked_to_stop_before_it_is_killed() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let marker = dir.path().join("stopped");
        let script = dir.path().join("server.sh");
        std::fs::write(
            &script,
            format!(
                "trap 'echo asked > {}; exit 0' TERM\nwhile true; do sleep 0.1; done\n",
                marker.display()
            ),
        )
        .expect("write the script");
        let mut spawned = spawn(Path::new("/bin/sh"), &script, Vec::new()).expect("spawns");
        tokio::time::sleep(Duration::from_millis(200)).await;
        stop(&mut spawned.child).await;
        assert_eq!(
            std::fs::read_to_string(&marker)
                .expect("it was asked")
                .trim(),
            "asked"
        );
    }

    /// The whole path with the server's own format: written just before exit, read back as the
    /// reason.
    #[tokio::test]
    async fn the_reason_the_server_gives_is_what_the_exit_says() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let script = dir.path().join("die.sh");
        std::fs::write(
            &script,
            "echo '2026-10-21 13:06:13.384 FATAL MatterServer storage is locked'\nexit 1\n",
        )
        .expect("write the script");
        let mut spawned = spawn(Path::new("/bin/sh"), &script, Vec::new()).expect("spawns");
        spawned.child.wait().await.expect("the script exits");
        assert_eq!(
            spawned.drain_logs().await.as_deref(),
            Some("storage is locked")
        );
    }
}
