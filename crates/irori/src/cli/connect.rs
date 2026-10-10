//! Which Irori, and whose credential.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use irori_client::{Builder, Client};

use super::args::Remote;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

pub struct Link {
    pub client: Client,
    pub json: bool,
    pub yes: bool,
}

/// `--url`, else `<data>/cli.url`, else `./data/cli.url` when that file is there, else
/// the loopback default. A `--data` with no `cli.url` is an error: that flag named a
/// directory, and guessing 8480 would talk to a different Irori.
pub fn resolve_origin(url: Option<&str>, data: Option<&Path>) -> Result<String, String> {
    if let Some(url) = url {
        return irori_client::normalize_origin(url).map_err(|error| error.message);
    }
    if let Some(data) = data {
        let path = data.join("cli.url");
        let text = fs::read_to_string(&path).map_err(|_| {
            format!(
                "no cli.url in {}. Start Irori, or pass --url.",
                data.display()
            )
        })?;
        return irori_client::normalize_origin(text.trim()).map_err(|error| error.message);
    }
    let local = Path::new("./data/cli.url");
    if local.is_file() {
        let text = fs::read_to_string(local)
            .map_err(|error| format!("couldn't read {}: {error}", local.display()))?;
        return irori_client::normalize_origin(text.trim()).map_err(|error| error.message);
    }
    Ok("http://127.0.0.1:8480".to_owned())
}

pub fn connect(remote: &Remote) -> Result<Link, String> {
    open(remote, true).map(|(client, json, yes)| Link { client, json, yes })
}

/// `use_token` is false for `login`, which must not send a bearer token at the sign-in.
pub fn open(remote: &Remote, use_token: bool) -> Result<(Client, bool, bool), String> {
    let origin = resolve_origin(remote.url.as_deref(), remote.data.as_deref())?;
    let mut builder = Builder::new(&origin).map_err(|error| error.message)?;
    builder = builder.insecure(remote.insecure);
    if let Some(ca) = &remote.ca {
        let pem =
            fs::read(ca).map_err(|error| format!("couldn't read {}: {error}", ca.display()))?;
        builder = builder.ca_pem(pem);
    }
    if use_token {
        if let Some(token) = remote.token.as_ref().filter(|token| !token.is_empty()) {
            builder = builder.bearer(token);
        } else if let Some(cookie) = session_for(&origin)? {
            builder = builder.session(cookie);
        }
    }
    let client = builder.build().map_err(|error| error.message)?;
    Ok((client, remote.json, remote.yes))
}

pub fn session_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            "neither XDG_CONFIG_HOME nor HOME is set, so the session has nowhere to go".to_owned()
        })?;
    Ok(base.join("irori").join("session"))
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SavedSession {
    pub origin: String,
    pub cookie: String,
}

pub fn session_for(origin: &str) -> Result<Option<String>, String> {
    let path = session_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("couldn't read {}: {error}", path.display()))?;
    let saved: SavedSession = serde_json::from_str(&text)
        .map_err(|error| format!("{} isn't a session file: {error}", path.display()))?;
    Ok((saved.origin == origin).then_some(saved.cookie))
}

pub fn save_session(origin: &str, cookie: &str) -> Result<(), String> {
    let path = session_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("couldn't create {}: {error}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let body = serde_json::to_string_pretty(&SavedSession {
        origin: origin.to_owned(),
        cookie: cookie.to_owned(),
    })
    .map_err(|error| format!("couldn't write the session: {error}"))?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .map_err(|error| format!("couldn't write {}: {error}", path.display()))?;
    file.write_all(body.as_bytes())
        .map_err(|error| format!("couldn't write {}: {error}", path.display()))?;
    Ok(())
}

pub fn forget_session() -> Result<Option<SavedSession>, String> {
    let path = session_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("couldn't read {}: {error}", path.display()))?;
    fs::remove_file(&path)
        .map_err(|error| format!("couldn't remove {}: {error}", path.display()))?;
    let saved = serde_json::from_str(&text).unwrap_or(SavedSession {
        origin: String::new(),
        cookie: String::new(),
    });
    Ok(Some(saved))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_named_data_dir_without_cli_url_is_an_error() {
        let dir = std::env::temp_dir().join(format!("irori-cli-origin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let error = resolve_origin(None, Some(&dir)).unwrap_err();
        assert!(error.contains("cli.url"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_url_wins_over_the_default() {
        let dir = std::env::temp_dir().join(format!("irori-cli-url-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cli.url"), "http://10.0.0.8:9000\n").unwrap();
        assert_eq!(
            resolve_origin(None, Some(&dir)).unwrap(),
            "http://10.0.0.8:9000"
        );
        assert_eq!(
            resolve_origin(Some("http://127.0.0.1:1"), Some(&dir)).unwrap(),
            "http://127.0.0.1:1"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
