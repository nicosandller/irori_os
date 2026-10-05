//! The Ollama that Irori puts on the machine itself, when the person asks for a local model
//! and none is running.
//!
//! It is Ollama's own release, unpacked into `ollama/` in the data directory and run from
//! there as `ollama serve`, listening on this machine only. Its models live in the same
//! directory, so removing it removes everything it brought. An Ollama the person installed
//! themselves is left alone: if one already answers on the port, that one is used.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt as _;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

pub const HOST: &str = "127.0.0.1:11434";
#[cfg(not(test))]
const RELEASES: &str = "https://github.com/ollama/ollama/releases/latest/download";
/// Tests never download a release: this is a port nothing listens on.
#[cfg(test)]
const RELEASES: &str = "http://127.0.0.1:9";
/// How long the download may say nothing before it is given up on.
const QUIET: Duration = Duration::from_secs(60);
/// How long `ollama serve` gets to start listening.
const STARTS_WITHIN: Duration = Duration::from_secs(30);

/// The one this process started, kept so it can be stopped and reaped.
static CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

/// Ollama's release for the machine this binary was built for.
struct Release {
    asset: &'static str,
    /// Free disk the unpacked release needs, with room to spare for a small model.
    needs: u64,
}

fn release() -> Option<Release> {
    const GB: u64 = 1024 * 1024 * 1024;
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Some(Release {
            asset: "ollama-linux-arm64.tar.zst",
            // About 50 MB once the GPU libraries are left out (see `install`).
            needs: 2 * GB,
        }),
        ("linux", "x86_64") => Some(Release {
            asset: "ollama-linux-amd64.tar.zst",
            needs: 8 * GB,
        }),
        ("macos", _) => Some(Release {
            asset: "ollama-darwin.tgz",
            needs: 3 * GB,
        }),
        _ => None,
    }
}

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("ollama")
}

fn pid_file(data_dir: &Path) -> PathBuf {
    data_dir.join("ollama.pid")
}

/// Where Irori's Ollama writes what it says. A file, not a pipe: a restart replaces this
/// process in place, and an Ollama left writing into a pipe nobody holds would be killed for it.
fn log_file(data_dir: &Path) -> PathBuf {
    data_dir.join("ollama.log")
}

/// How much of Ollama's output is kept. Past this the file is started again.
const LOG_MAX: u64 = 4 * 1024 * 1024;
/// How many lines the Settings page is shown.
const LOG_LINES: usize = 400;

/// The last things Irori's Ollama said, oldest first. Its line for every request it served
/// is left out: Irori asks it what it has every few seconds, and those would be all there is.
pub fn log(data_dir: &Path) -> Vec<String> {
    let path = log_file(data_dir);
    if std::fs::metadata(&path).is_ok_and(|file| file.len() > LOG_MAX) {
        // Ollama appends, so emptying the file under it is safe: its next line starts it again.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path);
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("[GIN]"))
        .collect();
    lines[lines.len().saturating_sub(LOG_LINES)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

/// Linux releases keep the binary under `bin/`. The macOS one has it at the top.
fn binary(data_dir: &Path) -> Option<PathBuf> {
    let dir = dir(data_dir);
    [dir.join("bin").join("ollama"), dir.join("ollama")]
        .into_iter()
        .find(|path| path.is_file())
}

/// Whether Irori's own Ollama is on the disk.
pub fn installed(data_dir: &Path) -> bool {
    binary(data_dir).is_some()
}

pub async fn listening() -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(HOST)).await,
        Ok(Ok(_))
    )
}

/// Downloads and unpacks Ollama's release. `report` hears how many bytes have arrived out of
/// how many, and answers whether to carry on.
///
/// The archive is unpacked as it arrives, so it never sits on the disk beside its own
/// contents. It lands in a directory of its own first, and only takes the real place once its
/// checksum matches the one published with the release.
pub async fn install(
    data_dir: &Path,
    free: u64,
    report: &mut (dyn FnMut(u64, u64) -> bool + Send),
) -> Result<(), String> {
    let Some(release) = release() else {
        return Err(
            "Irori can't install Ollama on this kind of machine. Install it from \
             https://ollama.com/download and it will be used."
                .to_owned(),
        );
    };
    if free < release.needs {
        return Err(format!(
            "Ollama needs about {} GB of free disk, and there is {:.1} GB.",
            release.needs >> 30,
            free as f64 / (1u64 << 30) as f64
        ));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let unreachable = |_| "Couldn't reach github.com to download Ollama.".to_owned();
    let sums = client
        .get(format!("{RELEASES}/sha256sum.txt"))
        .timeout(QUIET)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(unreachable)?
        .text()
        .await
        .map_err(unreachable)?;
    let expected = checksum_for(&sums, release.asset)
        .ok_or("Ollama's release has no checksum for this machine's download.")?;
    let response = client
        .get(format!("{RELEASES}/{}", release.asset))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(unreachable)?;
    let total = response.content_length().unwrap_or(0);

    let staging = data_dir.join("ollama.installing");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let mut tar = tokio::process::Command::new("tar");
    tar.arg("-x")
        .arg(if release.asset.ends_with(".zst") {
            "--zstd"
        } else {
            "-z"
        })
        .arg("-f")
        .arg("-")
        .arg("-C")
        .arg(&staging);
    if std::env::consts::ARCH == "aarch64" && std::env::consts::OS == "linux" {
        // The arm64 release carries NVIDIA's GPU libraries, which are most of its size and no
        // use on a Pi.
        tar.arg("--exclude=*cuda_*");
    }
    let mut tar = tar
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Unpacking Ollama needs tar on this machine.".to_owned())?;
    let mut pipe = tar.stdin.take().ok_or("tar has no input")?;

    let unpacked = async {
        let mut hasher = Sha256::new();
        let mut done = 0u64;
        let mut stream = response.bytes_stream();
        loop {
            let chunk = match tokio::time::timeout(QUIET, stream.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(None) => break,
                Ok(Some(Err(_))) | Err(_) => {
                    return Err("The Ollama download stopped early.".to_owned());
                }
            };
            hasher.update(&chunk);
            pipe.write_all(&chunk).await.map_err(|_| {
                "Couldn't unpack Ollama. On Linux this needs zstd installed, and enough disk."
                    .to_owned()
            })?;
            done += chunk.len() as u64;
            if !report(done, total) {
                return Err("The install was stopped.".to_owned());
            }
        }
        drop(pipe);
        let status = tar.wait().await.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(
                "Couldn't unpack Ollama. On Linux this needs zstd installed, and enough disk."
                    .to_owned(),
            );
        }
        let got: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if got != expected {
            return Err("The Ollama download doesn't match its published checksum.".to_owned());
        }
        Ok(())
    }
    .await;
    if let Err(error) = unpacked {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let dir = dir(data_dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&staging, &dir).map_err(|error| error.to_string())?;
    if binary(data_dir).is_none() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("Ollama's release didn't contain the program.".to_owned());
    }
    Ok(())
}

/// The hex digest on the line of `sums` that names `asset`.
fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let digest = words.next()?;
        let name = words.next()?.trim_start_matches(['*', '.', '/']);
        (name == asset && digest.len() == 64).then(|| digest.to_ascii_lowercase())
    })
}

/// Starts Irori's Ollama if it is installed and nothing is answering yet. `Ok(false)` means
/// there is nothing of Irori's to start.
pub async fn start(data_dir: &Path) -> Result<bool, String> {
    if listening().await {
        return Ok(true);
    }
    let Some(binary) = binary(data_dir) else {
        return Ok(false);
    };
    // What it says goes to a file the Settings page reads. Without it, a model that dies
    // while loading leaves nothing to read.
    // It starts empty each time, and both of Ollama's outputs append to it.
    let _ = std::fs::File::create(log_file(data_dir));
    let output = || {
        std::fs::OpenOptions::new()
            .append(true)
            .open(log_file(data_dir))
            .map_or_else(|_| Stdio::null(), Stdio::from)
    };
    let child = std::process::Command::new(&binary)
        .arg("serve")
        .env("OLLAMA_HOST", HOST)
        .env("OLLAMA_MODELS", dir(data_dir).join("models"))
        // One model, one question at a time: this is a small machine, and Irori asks in turn.
        .env("OLLAMA_MAX_LOADED_MODELS", "1")
        .env("OLLAMA_NUM_PARALLEL", "1")
        // The model's server keeps every finished prompt in memory, up to 8 GB by default,
        // to save reading it again. Irori's prompts carry the home as it is now, so they are
        // rarely the same twice, and that memory is the model's own to load into.
        .env("LLAMA_ARG_CACHE_RAM", "0")
        .stdin(Stdio::null())
        .stdout(output())
        .stderr(output())
        .spawn()
        .map_err(|error| format!("Ollama wouldn't start ({error})."))?;
    let _ = std::fs::write(pid_file(data_dir), child.id().to_string());
    if let Ok(mut held) = CHILD.lock() {
        *held = Some(child);
    }
    let deadline = tokio::time::Instant::now() + STARTS_WITHIN;
    while tokio::time::Instant::now() < deadline {
        if listening().await {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("Ollama was started and never began listening.".to_owned())
}

/// Stops Irori's Ollama and deletes it, models and all.
pub async fn uninstall(data_dir: &Path) -> Result<(), String> {
    let data_dir = data_dir.to_owned();
    tokio::task::spawn_blocking(move || {
        let held = CHILD.lock().ok().and_then(|mut held| held.take());
        if let Some(mut child) = held {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Ok(pid) = std::fs::read_to_string(pid_file(&data_dir))
            && pid.trim().parse::<u32>().is_ok()
            && runs_from(pid.trim(), &dir(&data_dir))
        {
            // Started by an earlier run of this server, which a restart replaces in place.
            // Signalled directly: a small image has no `kill` program to ask.
            if let Some(pid) = pid
                .trim()
                .parse::<i32>()
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
        }
        let _ = std::fs::remove_file(pid_file(&data_dir));
        let _ = std::fs::remove_file(log_file(&data_dir));
        let _ = std::fs::remove_dir_all(data_dir.join("ollama.installing"));
        match std::fs::remove_dir_all(dir(&data_dir)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Couldn't delete Ollama ({error}).")),
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    // Gone from the disk is not the same as stopped. One left running would answer on the
    // port with nothing behind it, and look like an Ollama that needs no installing.
    for _ in 0..20 {
        if !listening().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("Ollama's files are deleted, but it is still running. Restart Irori's machine or container to stop it.".to_owned())
}

/// Whether process `pid` is a program from inside `dir`. The number in the pid file can
/// outlive the Ollama it was written for, and by then belong to anything.
fn runs_from(pid: &str, dir: &Path) -> bool {
    // Linux says it directly, and a small image may have no `ps` to ask.
    if let Ok(program) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        return program.starts_with(dir);
    }
    std::process::Command::new("ps")
        .args(["-p", pid, "-o", "command="])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|ps| {
            let command = String::from_utf8_lossy(&ps.stdout);
            dir.to_str()
                .is_some_and(|dir| command.trim_start().starts_with(dir))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_the_one_on_the_assets_own_line() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  ./ollama-darwin.tgz\n{b} *ollama-linux-arm64.tar.zst\n");
        assert_eq!(checksum_for(&sums, "ollama-darwin.tgz"), Some(a));
        assert_eq!(
            checksum_for(&sums, "ollama-linux-arm64.tar.zst"),
            Some("b".repeat(64))
        );
        assert_eq!(checksum_for(&sums, "ollama-linux-amd64.tar.zst"), None);
        assert_eq!(
            checksum_for("short  ollama-darwin.tgz\n", "ollama-darwin.tgz"),
            None
        );
    }

    #[tokio::test]
    async fn removing_what_was_never_installed_is_fine() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        assert!(!installed(dir.path()));
        std::fs::write(
            log_file(dir.path()),
            "[GIN] 200 GET /api/tags\nloading model\n\nsignal: killed\n",
        )?;
        assert_eq!(log(dir.path()), ["loading model", "signal: killed"]);
        // A pid left behind names a process that is not Irori's Ollama: this test. It stays.
        std::fs::write(pid_file(dir.path()), std::process::id().to_string())?;
        assert!(!runs_from(
            &std::process::id().to_string(),
            &super::dir(dir.path())
        ));
        uninstall(dir.path()).await.map_err(anyhow::Error::msg)?;
        Ok(())
    }
}
