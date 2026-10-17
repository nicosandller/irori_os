//! `backup.toml`: what is backed up, when, and where it goes (`docs/specs/backup.md` §4).
//!
//! Its own file, written whole by Irori, so the page can change it without being able to touch
//! `irori.toml` (where `allow_unauthenticated_lan` lives). Passwords, the encryption key and
//! cloud secrets are not here: they are in `secrets.toml`, under `[backup]`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "backup.toml";
/// The table of `secrets.toml` that backups keep their secrets in.
const TABLE: &str = "backup";
pub const KEY: &str = "key";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Take a backup on the schedule below. Making one by hand works either way.
    #[serde(default)]
    pub enabled: bool,
    /// Seal backups with the recovery key. On unless turned off; a backup that leaves this
    /// machine is refused without it.
    #[serde(default = "yes")]
    pub encrypt: bool,
    /// Time of day, `HH:MM`, in the home's time zone.
    #[serde(default = "at")]
    pub at: String,
    /// Weekdays (`mon` … `sun`). Empty is every day.
    #[serde(default)]
    pub days: Vec<String>,
    /// Newest backups kept, here and at each destination. At least 1.
    #[serde(default = "keep")]
    pub keep: u32,
    /// Also delete backups older than this many days (but never the newest).
    #[serde(default)]
    pub keep_days: Option<u32>,
    /// Leave the entity history out, for a smaller file.
    #[serde(default = "yes")]
    pub include_history: bool,
    /// Make a backup just before Irori or an extension is updated, so an update that goes
    /// wrong can be taken back. Kept apart from the scheduled ones, three at a time.
    #[serde(default = "yes")]
    pub before_update: bool,
    /// Other places a backup is copied to, besides `<data>/backups`.
    #[serde(default)]
    pub destination: Vec<Destination>,
}

fn yes() -> bool {
    true
}
fn at() -> String {
    "03:00".to_owned()
}
fn keep() -> u32 {
    7
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            encrypt: true,
            at: at(),
            days: Vec::new(),
            keep: keep(),
            keep_days: None,
            include_history: true,
            before_update: true,
            destination: Vec::new(),
        }
    }
}

/// A place a backup is copied to after it is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Destination {
    /// A directory: a mounted network share (SMB, NFS), a USB disk, another volume.
    Path { name: String, path: PathBuf },
    /// A WebDAV folder: Nextcloud, ownCloud, most NAS boxes. The password is in secrets.
    Webdav {
        name: String,
        url: String,
        username: String,
    },
    /// Anything that speaks S3: AWS, Backblaze B2, Cloudflare R2, MinIO. The secret key is in
    /// secrets.
    S3 {
        name: String,
        endpoint: String,
        bucket: String,
        region: String,
        #[serde(default)]
        prefix: String,
        access_key: String,
    },
}

impl Destination {
    pub fn name(&self) -> &str {
        match self {
            Self::Path { name, .. } | Self::Webdav { name, .. } | Self::S3 { name, .. } => name,
        }
    }

    /// The secret that goes with this destination, if it has one.
    pub fn secret_name(&self) -> Option<String> {
        match self {
            Self::Path { .. } => None,
            Self::Webdav { name, .. } => Some(format!("password_{name}")),
            Self::S3 { name, .. } => Some(format!("secret_key_{name}")),
        }
    }
}

impl Settings {
    /// Whether this can be written down. The sentence says what to change.
    pub fn check(&self) -> Result<(), String> {
        if self.keep == 0 {
            return Err("keep at least 1 backup".to_owned());
        }
        if self.keep_days == Some(0) {
            return Err("keep_days must be at least 1, or left out".to_owned());
        }
        if parse_time(&self.at).is_none() {
            return Err(format!(
                "`{}` isn't a time of day; write it as 03:00",
                self.at
            ));
        }
        for day in &self.days {
            if weekday(day).is_none() {
                return Err(format!(
                    "`{day}` isn't a weekday; use mon, tue, wed, thu, fri, sat, sun"
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for destination in &self.destination {
            let name = destination.name();
            let valid = !name.is_empty()
                && name.len() <= 32
                && name != "local"
                && name.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                });
            if !valid {
                return Err(format!(
                    "`{name}` can't name a destination: use lowercase letters, digits, - and _ (and not `local`)"
                ));
            }
            if !seen.insert(name.to_owned()) {
                return Err(format!("two destinations are called `{name}`"));
            }
            match destination {
                Destination::Path { path, .. } if !path.is_absolute() => {
                    return Err(format!("`{name}`: the path has to be absolute"));
                }
                Destination::Webdav { url, .. } => secure_address(name, url)?,
                Destination::S3 {
                    endpoint,
                    bucket,
                    region,
                    ..
                } => {
                    if bucket.is_empty() || region.is_empty() {
                        return Err(format!(
                            "`{name}`: an S3 destination needs an https:// endpoint, a bucket and a region"
                        ));
                    }
                    secure_address(name, endpoint)?;
                }
                _ => {}
            }
        }
        // Nothing that leaves the machine goes out readable.
        if !self.destination.is_empty() && !self.encrypt {
            return Err(
                "backups copied to another place have to be encrypted: turn encryption on, or remove the destinations"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

/// An address a password may be sent to: https anywhere, or http only on a network of the
/// home's own (a NAS by name or private address, `localhost`), where nobody is listening in.
fn secure_address(name: &str, address: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(address).map_err(|_| {
        format!("`{name}`: the address has to start with https:// (or http:// on your own network)")
    })?;
    let host = url.host_str().unwrap_or("");
    match url.scheme() {
        "https" if !host.is_empty() => Ok(()),
        "http" if is_home_network(host) => Ok(()),
        "http" if !host.is_empty() => Err(format!(
            "`{name}`: this address sends the password in the clear across the internet; use https://"
        )),
        _ => Err(format!(
            "`{name}`: the address has to start with https:// (or http:// on your own network)"
        )),
    }
}

fn is_home_network(host: &str) -> bool {
    use std::net::IpAddr;
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
            IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        };
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    !name.contains('.')
        || ["local", "lan", "home", "internal", "localhost"]
            .iter()
            .any(|tld| name.ends_with(&format!(".{tld}")))
        || name.ends_with(".home.arpa")
}

/// `HH:MM` as hour and minute.
pub fn parse_time(text: &str) -> Option<(i8, i8)> {
    let (h, m) = text.split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m) && text.len() == 5).then_some((h, m))
}

pub fn weekday(name: &str) -> Option<jiff::civil::Weekday> {
    use jiff::civil::Weekday::*;
    Some(match name {
        "mon" => Monday,
        "tue" => Tuesday,
        "wed" => Wednesday,
        "thu" => Thursday,
        "fri" => Friday,
        "sat" => Saturday,
        "sun" => Sunday,
        _ => return None,
    })
}

pub fn path(config: &Path) -> PathBuf {
    config.join(FILE)
}

/// What `backup.toml` says; a missing or unreadable file is "nothing configured".
pub fn load(config: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(path(config)) else {
        return Settings::default();
    };
    match toml::from_str::<Settings>(&text) {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "backup.toml can't be read; backups use the defaults until it is fixed");
            Settings::default()
        }
    }
}

pub fn save(config: &Path, settings: &Settings) -> Result<(), String> {
    settings.check()?;
    let body = toml::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let text = format!(
        "# Backups (docs/specs/backup.md). Written by Irori and yours to edit. Passwords, the\n\
         # recovery key and cloud secrets are not here: they are in secrets.toml under [backup].\n\n{body}"
    );
    std::fs::create_dir_all(config).map_err(|e| e.to_string())?;
    let target = path(config);
    let part = target.with_extension("toml.part");
    std::fs::write(&part, text).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &target).map_err(|e| e.to_string())
}

fn table() -> irori_types::ExtensionId {
    irori_types::ExtensionId::try_from(TABLE).expect("a valid id")
}

/// One of backup's secrets, from `secrets.toml`.
pub fn secret(config: &Path, name: &str) -> Option<String> {
    let mut store = irori_config::Store::new(config);
    let _ = store.reload();
    match store.secrets().of(&table()).get(name) {
        Some(serde_json::Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

pub fn set_secret(config: &Path, name: &str, value: &str) -> Result<(), String> {
    let mut store = irori_config::Store::new(config);
    let _ = store.reload();
    let mut secrets = store.secrets();
    secrets
        .set(&table(), &[name.to_owned()], value.to_owned())
        .map_err(|e| e.to_string())?;
    store
        .save_secrets(&secrets)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The recovery key: the one in `secrets.toml`, or a new one written there.
pub fn key_or_make(config: &Path) -> Result<(String, bool), String> {
    if let Some(key) = secret(config, KEY) {
        return Ok((key, false));
    }
    let key = super::crypt::generate_key();
    set_secret(config, KEY, &key)?;
    Ok((key, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3() -> Destination {
        Destination::S3 {
            name: "b2".into(),
            endpoint: "https://s3.example.com".into(),
            bucket: "irori".into(),
            region: "us-west-1".into(),
            prefix: String::new(),
            access_key: "AK".into(),
        }
    }

    #[test]
    fn a_file_round_trips_and_missing_means_defaults() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(load(dir.path()), Settings::default());
        let settings = Settings {
            enabled: true,
            days: vec!["mon".into(), "thu".into()],
            keep_days: Some(30),
            destination: vec![
                s3(),
                Destination::Path {
                    name: "nas".into(),
                    path: "/mnt/nas/irori".into(),
                },
            ],
            ..Settings::default()
        };
        save(dir.path(), &settings).expect("saves");
        assert_eq!(load(dir.path()), settings);
    }

    #[test]
    fn what_cannot_be_kept_is_refused_with_a_reason() {
        let bad = |change: fn(&mut Settings)| {
            let mut s = Settings::default();
            change(&mut s);
            s.check().expect_err("refused")
        };
        assert!(bad(|s| s.keep = 0).contains("at least 1"));
        assert!(bad(|s| s.at = "3am".into()).contains("time of day"));
        assert!(bad(|s| s.days = vec!["funday".into()]).contains("weekday"));
        assert!(bad(|s| s.destination = vec![s3(), s3()]).contains("two destinations"));
        assert!(
            bad(|s| {
                s.destination = vec![Destination::Path {
                    name: "local".into(),
                    path: "/x".into(),
                }]
            })
            .contains("can't name")
        );
        assert!(
            bad(|s| {
                s.destination = vec![Destination::Path {
                    name: "usb".into(),
                    path: "rel".into(),
                }]
            })
            .contains("absolute")
        );
        // Leaving the machine unsealed is the one thing that is never allowed.
        assert!(
            bad(|s| {
                s.encrypt = false;
                s.destination = vec![s3()];
            })
            .contains("encrypted")
        );
        // A password doesn't cross the internet in the clear, but may cross the home's network.
        let dav = |url: &str| {
            Settings {
                destination: vec![Destination::Webdav {
                    name: "nas".into(),
                    url: url.into(),
                    username: "me".into(),
                }],
                ..Settings::default()
            }
            .check()
        };
        assert!(dav("https://cloud.example.com/dav").is_ok());
        assert!(dav("http://192.168.1.5:5005/dav").is_ok());
        assert!(dav("http://nas.local/dav").is_ok());
        assert!(dav("http://nas/dav").is_ok());
        assert!(
            dav("http://cloud.example.com/dav")
                .expect_err("refused")
                .contains("in the clear")
        );
        assert!(dav("ftp://nas/dav").is_err());
        // Encryption off with nowhere to send is fine.
        let plain = Settings {
            encrypt: false,
            ..Settings::default()
        };
        assert!(plain.check().is_ok());
    }

    #[test]
    fn the_key_is_made_once_and_kept_in_secrets() {
        let dir = tempfile::tempdir().expect("dir");
        let (first, made) = key_or_make(dir.path()).expect("made");
        assert!(made);
        let (again, made) = key_or_make(dir.path()).expect("read");
        assert!(!made);
        assert_eq!(first, again);
        let secrets = std::fs::read_to_string(dir.path().join("secrets.toml")).expect("written");
        assert!(
            secrets.contains("[backup]") && secrets.contains(&first),
            "{secrets}"
        );
    }
}
