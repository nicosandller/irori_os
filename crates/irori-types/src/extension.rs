//! Extension manifests: what an extension is, what it contributes, and what it's allowed to do.
//! See `docs/specs/extensions.md`.
//!
//! Manifests are written as TOML (`irori-extension.toml`) and read through the JSON data model,
//! so the same types and JSON Schema check them whatever the file format.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::id::{err, string_newtype};
use crate::{Description, EntityKind, ExtensionId, IdError, IntegrationId, InvariantError, Name};

/// The contents of an extension's `irori-extension.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub extension: ExtensionInfo,
    /// What the extension adds to Irori, by contribution kind.
    #[serde(default, skip_serializing_if = "Contributions::is_empty")]
    pub contributes: Contributions,
    /// What the extension may access. Shown to the owner for approval at install.
    #[serde(default, skip_serializing_if = "Permissions::is_empty")]
    pub permissions: Permissions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtensionManifest {
    extension: ExtensionInfo,
    #[serde(default)]
    contributes: Contributions,
    #[serde(default)]
    permissions: Permissions,
}

impl<'de> Deserialize<'de> for ExtensionManifest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawExtensionManifest::deserialize(deserializer)?;
        let manifest = ExtensionManifest {
            extension: raw.extension,
            contributes: raw.contributes,
            permissions: raw.permissions,
        };
        manifest.validate().map_err(serde::de::Error::custom)?;
        Ok(manifest)
    }
}

impl ExtensionManifest {
    /// Checks the rules that span fields. Deserialization runs this; call it yourself when
    /// building a manifest in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        self.contributes.validate()?;
        self.permissions.validate()
    }

    /// The id of the integration this extension contributes, if any. It's always the extension
    /// id (ROADMAP D25).
    pub fn integration_id(&self) -> Option<IntegrationId> {
        (!self.contributes.integration.is_empty()).then(|| {
            IntegrationId::try_from(self.extension.id.as_str())
                .expect("extension ids and integration ids share the slug format")
        })
    }

    /// Parts of the manifest this version of Irori ignores. Not errors: an extension can offer
    /// newer contribution kinds and still work on an older core. Show these to the user.
    pub fn warnings(&self) -> Vec<String> {
        let id = &self.extension.id;
        let mut warnings = Vec::new();
        for (kind, entries) in [
            ("dashboard", &self.contributes.dashboard),
            ("card", &self.contributes.card),
            ("app", &self.contributes.app),
            ("automation", &self.contributes.automation),
        ] {
            if !entries.is_empty() {
                warnings.push(format!(
                    "extension `{id}`: `contributes.{kind}` isn't supported by this version of Irori yet; ignoring it"
                ));
            }
        }
        for kind in self.contributes.other.keys() {
            warnings.push(format!(
                "extension `{id}`: `contributes.{kind}` isn't a contribution kind this version of Irori knows; ignoring it"
            ));
        }
        warnings
    }
}

/// The `[extension]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInfo {
    /// Also the id of the integration it contributes, if any (ROADMAP D25).
    pub id: ExtensionId,
    pub name: Name,
    pub version: Version,
    /// Which versions of Irori it works with, e.g. `>=0.1.0, <0.2.0`.
    pub irori: CoreRequirement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
    /// JSON Schema for the extension's settings, relative to the package root. External
    /// extensions only: a built-in extension's schema comes from its Rust config type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<PackagePath>,
    /// A square SVG, relative to the package root, shown beside the extension and its devices so
    /// they can be told apart at a glance. Always displayed as an image, never inlined into a
    /// page, so it can't run script. A built-in extension embeds the file as well
    /// (`Integration::ICON`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PackagePath>,
}

/// The `[contributes]` table: one list per contribution kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = crate::schema::contributions_other_kinds)]
pub struct Contributions {
    /// At most one per extension in this version (ROADMAP D25).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 1))]
    pub integration: Vec<IntegrationContribution>,
    /// Reserved for Phase 2c; read but ignored, with a warning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dashboard: Vec<ReservedContribution>,
    /// Reserved for Phase 2c; read but ignored, with a warning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card: Vec<ReservedContribution>,
    /// Reserved for Phase 3; read but ignored, with a warning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app: Vec<ReservedContribution>,
    /// Reserved: an automation engine. Read but ignored, with a warning, until a later Irori
    /// implements the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<ReservedContribution>,
    /// Kinds this version of Irori doesn't know; read but ignored, with a warning.
    #[serde(flatten)]
    #[schemars(skip)]
    pub other: BTreeMap<String, Vec<ReservedContribution>>,
}

/// A contribution of a kind this version of Irori doesn't implement. Kept as written.
pub type ReservedContribution = serde_json::Map<String, serde_json::Value>;

impl Contributions {
    pub fn is_empty(&self) -> bool {
        self.integration.is_empty()
            && self.dashboard.is_empty()
            && self.card.is_empty()
            && self.app.is_empty()
            && self.automation.is_empty()
            && self.other.is_empty()
    }

    fn validate(&self) -> Result<(), InvariantError> {
        if self.integration.len() > 1 {
            return Err(InvariantError(format!(
                "an extension contributes at most one integration (found {})",
                self.integration.len()
            )));
        }
        for (i, integration) in self.integration.iter().enumerate() {
            integration
                .validate()
                .map_err(|e| InvariantError(format!("contributes.integration[{i}]: {e}")))?;
        }
        Ok(())
    }
}

/// A `[[contributes.integration]]` entry: the extension brings in devices and entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegrationContribution {
    /// Where its devices live and how it hears about changes. Shown as a badge.
    pub iot_class: IotClass,
    /// The kinds of entity it creates. It must handle the standard services of each kind.
    #[schemars(length(min = 1), extend("uniqueItems" = true))]
    pub entity_kinds: Vec<EntityKind>,
    /// How to start it, for external extensions. Built-in extensions leave this out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunCommand>,
}

impl IntegrationContribution {
    fn validate(&self) -> Result<(), InvariantError> {
        if self.entity_kinds.is_empty() {
            return Err(InvariantError(
                "entity_kinds must list at least one entity kind".into(),
            ));
        }
        no_duplicates("entity_kinds", &self.entity_kinds)
    }
}

/// Where an integration's devices live and how it learns about changes (ROADMAP D24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IotClass {
    /// On the local network; devices report changes as they happen.
    LocalPush,
    /// On the local network; Irori asks for changes periodically.
    LocalPolling,
    /// Through a cloud service that reports changes as they happen.
    CloudPush,
    /// Through a cloud service that Irori asks periodically.
    CloudPolling,
}

impl IotClass {
    /// Whether it needs the internet to work.
    pub fn is_cloud(self) -> bool {
        matches!(self, Self::CloudPush | Self::CloudPolling)
    }
}

/// How the core starts an external extension's process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunCommand {
    /// The executable, relative to the package root, e.g. `bin/esphome`.
    pub command: PackagePath,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// The `[permissions]` table (ROADMAP D23). Everything defaults to "no access".
///
/// An integration never needs a permission to manage its own devices and entities or to
/// receive service calls for them; these cover reaching anything beyond that.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Irori API access beyond its own devices and entities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("uniqueItems" = true))]
    pub api: Vec<ApiScope>,
    /// Devices on the local network: private and link-local addresses, and mDNS (`.local`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lan: bool,
    /// Internet hosts it connects to, e.g. `api.switch-bot.com` or `*.example.com`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("uniqueItems" = true))]
    pub network: Vec<NetworkHost>,
    /// Serial or USB devices, e.g. `/dev/ttyUSB0`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("uniqueItems" = true))]
    pub serial: Vec<SerialPath>,
    /// Files and folders on the machine. `$CONFIG` and `$DATA` are Irori's own folders; any
    /// other path means full access to this machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("uniqueItems" = true))]
    pub host_fs: Vec<HostPath>,
    /// Run commands on the machine. Means full access to this machine.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub host_shell: bool,
}

impl Permissions {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether these permissions amount to full control of the machine Irori runs on. The UI and
    /// CLI must say so plainly, and only the owner may approve them.
    pub fn full_access(&self) -> bool {
        self.host_shell || self.host_fs.iter().any(|p| !p.is_irori_folder())
    }

    fn validate(&self) -> Result<(), InvariantError> {
        no_duplicates("permissions.api", &self.api)?;
        no_duplicates("permissions.network", &self.network)?;
        no_duplicates("permissions.serial", &self.serial)?;
        no_duplicates("permissions.host_fs", &self.host_fs)
    }
}

fn no_duplicates<T: PartialEq + fmt::Display>(
    field: &str,
    items: &[T],
) -> Result<(), InvariantError> {
    for (i, item) in items.iter().enumerate() {
        if items[..i].contains(item) {
            return Err(InvariantError(format!(
                "{field} lists `{item}` more than once"
            )));
        }
    }
    Ok(())
}

/// Access to the Irori API beyond the extension's own devices and entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ApiScope {
    /// Read every device, entity, area, and floor.
    #[serde(rename = "registry:read")]
    RegistryRead,
    /// Read every entity's current state.
    #[serde(rename = "states:read")]
    StatesRead,
    /// Subscribe to events, including state changes of every entity.
    #[serde(rename = "events:read")]
    EventsRead,
    /// Call services on any entity, e.g. turn on a light another integration provides.
    #[serde(rename = "services:call")]
    ServicesCall,
}

impl fmt::Display for ApiScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RegistryRead => "registry:read",
            Self::StatesRead => "states:read",
            Self::EventsRead => "events:read",
            Self::ServicesCall => "services:call",
        })
    }
}

// --- Versions -------------------------------------------------------------------------------

/// Largest number allowed in a version component: 9 digits keeps every component in a `u32`.
const VERSION_PART_MAX_DIGITS: usize = 9;

const NUMBER: &str = "(0|[1-9][0-9]{0,8})";
const PRERELEASE_IDENT: &str = "(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)";

/// An extension's version: [Semantic Versioning](https://semver.org) `MAJOR.MINOR.PATCH`, with an
/// optional pre-release (`0.3.0-beta.1`). Build metadata (`+…`) isn't allowed, so equal versions
/// are spelled the same. At most 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

string_newtype!(Version, |v| parse_version(v).map(|_| ()));

impl Version {
    /// `(major, minor, patch)`, ignoring any pre-release.
    pub fn release(&self) -> (u32, u32, u32) {
        parse_version(&self.0)
            .expect("Version is validated on construction")
            .0
    }

    /// The part after `-`, if any.
    pub fn pre_release(&self) -> Option<&str> {
        self.0.split_once('-').map(|(_, pre)| pre)
    }
}

type Release = (u32, u32, u32);

fn parse_version(value: &str) -> Result<(Release, Option<&str>), IdError> {
    const WHAT: &str = "version";
    const SHAPE: &str =
        "must be MAJOR.MINOR.PATCH with an optional -pre-release, like 1.4.0 or 0.3.0-beta.1";
    if value.len() > 64 {
        return Err(err(WHAT, value, "must be at most 64 characters"));
    }
    if value.contains('+') {
        return Err(err(
            WHAT,
            value,
            "build metadata (`+...`) isn't allowed; use a pre-release (`-...`) instead",
        ));
    }
    let (release, pre) = match value.split_once('-') {
        Some((release, pre)) => (release, Some(pre)),
        None => (value, None),
    };
    let release = parse_release(release).ok_or_else(|| err(WHAT, value, SHAPE))?;
    if let Some(pre) = pre {
        let valid_ident = |id: &str| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !(id.len() > 1 && id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()))
        };
        if !pre.split('.').all(valid_ident) {
            return Err(err(
                WHAT,
                value,
                "the pre-release must be dot-separated words of letters, digits, and `-`, with no leading zeros in numbers",
            ));
        }
    }
    Ok((release, pre))
}

/// `MAJOR.MINOR.PATCH`: numbers with no leading zeros and at most 9 digits.
fn parse_release(value: &str) -> Option<Release> {
    let number = |part: &str| {
        let digits_ok = !part.is_empty()
            && part.len() <= VERSION_PART_MAX_DIGITS
            && part.bytes().all(|b| b.is_ascii_digit())
            && !(part.len() > 1 && part.starts_with('0'));
        digits_ok.then(|| part.parse::<u32>().ok()).flatten()
    };
    let mut parts = value.split('.');
    let release = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    parts.next().is_none().then_some(release)
}

impl JsonSchema for Version {
    fn schema_name() -> Cow<'static, str> {
        "Version".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Semantic version MAJOR.MINOR.PATCH with an optional -pre-release, e.g. 1.4.0 or 0.3.0-beta.1. No build metadata. At most 64 characters.",
            "pattern": format!(
                "^{NUMBER}\\.{NUMBER}\\.{NUMBER}(-{PRERELEASE_IDENT}(\\.{PRERELEASE_IDENT})*)?$"
            ),
            "maxLength": 64,
        })
    }
}

/// Which versions of Irori an extension works with: `>=A.B.C`, or `>=A.B.C, <X.Y.Z` with
/// the upper bound above the lower one (checked here, but not by the JSON Schema).
///
/// Deliberately narrower than Cargo's version requirements: one way to write each range.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CoreRequirement(String);

string_newtype!(CoreRequirement, |v| parse_requirement(v).map(|_| ()));

fn parse_requirement(value: &str) -> Result<(Release, Option<Release>), IdError> {
    const SHAPE: &str = "must be `>=A.B.C` or `>=A.B.C, <X.Y.Z`, like `>=0.1.0, <0.2.0`";
    let invalid = || err("Irori version requirement", value, SHAPE);
    let rest = value.strip_prefix(">=").ok_or_else(invalid)?;
    let (lower, upper) = match rest.split_once(", <") {
        Some((lower, upper)) => (lower, Some(upper)),
        None => (rest, None),
    };
    let lower = parse_release(lower).ok_or_else(invalid)?;
    let upper = upper
        .map(|u| parse_release(u).ok_or_else(invalid))
        .transpose()?;
    // Not expressible in JSON Schema, so only Rust checks it.
    if upper.is_some_and(|upper| upper <= lower) {
        return Err(err(
            "Irori version requirement",
            value,
            "matches no version: the upper bound must be above the lower bound",
        ));
    }
    Ok((lower, upper))
}

impl CoreRequirement {
    fn bounds(&self) -> (Release, Option<Release>) {
        parse_requirement(&self.0).expect("CoreRequirement is validated on construction")
    }

    /// Whether Irori `core` satisfies this requirement. A pre-release of Irori counts as its
    /// release, so `0.2.0-dev` satisfies `>=0.2.0`.
    pub fn matches(&self, core: &Version) -> bool {
        let (lower, upper) = self.bounds();
        let core = core.release();
        core >= lower && upper.is_none_or(|upper| core < upper)
    }
}

impl JsonSchema for CoreRequirement {
    fn schema_name() -> Cow<'static, str> {
        "CoreRequirement".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Compatible Irori versions: `>=A.B.C`, or `>=A.B.C, <X.Y.Z`, e.g. `>=0.1.0, <0.2.0`.",
            "pattern": format!("^>={NUMBER}\\.{NUMBER}\\.{NUMBER}(, <{NUMBER}\\.{NUMBER}\\.{NUMBER})?$"),
        })
    }
}

// --- Paths and hosts ------------------------------------------------------------------------

/// One path segment: starts with a letter, digit, or `_` (so never `.`, `..`, or hidden), then
/// letters, digits, `.`, `_`, `-`.
const SEGMENT: &str = "[A-Za-z0-9_][A-Za-z0-9._-]*";

fn is_segment(segment: &str) -> bool {
    segment
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn check_length(what: &'static str, value: &str) -> Result<(), IdError> {
    if value.len() > 255 {
        return Err(err(what, value, "must be at most 255 characters"));
    }
    Ok(())
}

/// A file inside an extension package, relative to its root, e.g. `bin/esphome`. Segments are
/// separated by `/`; none may be empty or start with `.`, so a path can't leave the package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PackagePath(String);

string_newtype!(PackagePath, check_package_path);

fn check_package_path(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "package path";
    check_length(WHAT, value)?;
    if !value.split('/').all(is_segment) {
        return Err(err(
            WHAT,
            value,
            "must be a relative path like `bin/esphome`: `/`-separated names of letters, digits, `.`, `_`, `-`, none starting with `.`",
        ));
    }
    Ok(())
}

impl JsonSchema for PackagePath {
    fn schema_name() -> Cow<'static, str> {
        "PackagePath".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A path relative to the extension package root, e.g. `bin/esphome`. Segments use letters, digits, `.`, `_`, `-` and don't start with `.`.",
            "pattern": format!("^{SEGMENT}(/{SEGMENT})*$"),
            "maxLength": 255,
        })
    }
}

/// A serial or USB device under `/dev`, e.g. `/dev/ttyUSB0` or `/dev/serial/by-id/usb-…`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SerialPath(String);

string_newtype!(SerialPath, check_serial_path);

fn check_serial_path(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "serial device";
    check_length(WHAT, value)?;
    match value.strip_prefix("/dev/") {
        Some(rest) if rest.split('/').all(is_segment) => Ok(()),
        _ => Err(err(
            WHAT,
            value,
            "must be a device path under /dev, like `/dev/ttyUSB0`",
        )),
    }
}

impl JsonSchema for SerialPath {
    fn schema_name() -> Cow<'static, str> {
        "SerialPath".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A serial or USB device path under /dev, e.g. `/dev/ttyUSB0`.",
            "pattern": format!("^/dev(/{SEGMENT})+$"),
            "maxLength": 255,
        })
    }
}

/// A file or folder on the machine: under Irori's own folders (`$CONFIG`, `$DATA`, optionally
/// followed by `/sub/path`), or an absolute path (`/`, `/home/pi`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostPath(String);

string_newtype!(HostPath, check_host_path);

fn check_host_path(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "host path";
    check_length(WHAT, value)?;
    let segments_ok = |rest: &str| {
        rest.is_empty()
            || rest
                .strip_prefix('/')
                .is_some_and(|r| r.split('/').all(is_segment))
    };
    let valid = if let Some(rest) = value
        .strip_prefix("$CONFIG")
        .or_else(|| value.strip_prefix("$DATA"))
    {
        segments_ok(rest)
    } else {
        value == "/" || (!value.is_empty() && segments_ok(value))
    };
    if !valid {
        return Err(err(
            WHAT,
            value,
            "must be `$CONFIG`, `$DATA`, or an absolute path like `/home/pi`, optionally followed by `/`-separated names (no trailing `/`, no names starting with `.`)",
        ));
    }
    Ok(())
}

impl HostPath {
    /// Inside Irori's config or data folder, rather than anywhere on the machine.
    pub fn is_irori_folder(&self) -> bool {
        self.0.starts_with("$CONFIG") || self.0.starts_with("$DATA")
    }
}

impl JsonSchema for HostPath {
    fn schema_name() -> Cow<'static, str> {
        "HostPath".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "`$CONFIG`, `$DATA`, or an absolute path, optionally followed by `/`-separated names, e.g. `$CONFIG/rules` or `/home/pi`. Anything outside `$CONFIG` and `$DATA` means full access to the machine.",
            "pattern": format!("^((\\$CONFIG|\\$DATA)(/{SEGMENT})*|/|(/{SEGMENT})+)$"),
            "maxLength": 255,
        })
    }
}

/// An internet host an extension connects to: a hostname (`api.switch-bot.com`), or every
/// subdomain of one (`*.example.com`). Lowercase, so each host has one spelling.
///
/// Local-network targets are rejected so they can't hide behind this permission: IP addresses,
/// single-label names, and names under local-only domains (`.local`, `.home.arpa`, …) need `lan`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NetworkHost(String);

string_newtype!(NetworkHost, check_network_host);

const LABEL: &str = "[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?";
/// The last label: starts with a letter, so an IP address never matches.
const TOP_LABEL: &str = "[a-z]([a-z0-9-]{0,61}[a-z0-9])?";

/// Domains that only exist on a local network: mDNS (`local`), RFC 6761 (`localhost`), RFC 8375
/// (`home.arpa`), ICANN's private-use `internal`, and common router defaults (`lan`, `home`).
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

fn check_network_host(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "network host";
    let host = value.strip_prefix("*.").unwrap_or(value);
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if host.len() > 253 || !host.split('.').all(label_ok) {
        return Err(err(
            WHAT,
            value,
            "must be a lowercase hostname like `api.example.com` or `*.example.com` (labels of a-z, 0-9, `-`, up to 63 characters each, 253 in total)",
        ));
    }
    let top = host.rsplit('.').next().unwrap_or(host);
    if !host.contains('.') || !top.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(err(
            WHAT,
            value,
            "must be an internet hostname with a domain, like `api.example.com`; for IP addresses and devices on your network, use `lan = true`",
        ));
    }
    if LOCAL_DOMAINS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
    {
        return Err(err(
            WHAT,
            value,
            "is a local-network name; use `lan = true` for devices on your network",
        ));
    }
    Ok(())
}

impl JsonSchema for NetworkHost {
    fn schema_name() -> Cow<'static, str> {
        "NetworkHost".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let local = LOCAL_DOMAINS.map(|d| d.replace('.', "\\.")).join("|");
        // Two branches so the 253-character limit applies to the hostname, with or without `*.`.
        json_schema!({
            "type": "string",
            "description": "An internet hostname, e.g. `api.example.com`, or `*.example.com` for every subdomain. Not an IP address or a local-network name (`.local`, `.home.arpa`, `.internal`, `.lan`, `.home`): those need `lan`.",
            "anyOf": [
                { "pattern": format!("^({LABEL}\\.)+{TOP_LABEL}$"), "maxLength": 253 },
                { "pattern": format!("^\\*\\.({LABEL}\\.)+{TOP_LABEL}$"), "maxLength": 255 },
            ],
            "not": { "pattern": format!("(^|\\.)({local})$") },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validator<T: JsonSchema>() -> jsonschema::Validator {
        let schema = T::json_schema(&mut SchemaGenerator::default());
        jsonschema::validator_for(schema.as_value()).expect("valid schema")
    }

    /// Rust and the schema must agree on every case.
    fn agree<T>(cases: &[(&str, bool)])
    where
        T: JsonSchema + for<'a> TryFrom<&'a str>,
    {
        let validator = validator::<T>();
        for &(s, expected) in cases {
            assert_eq!(T::try_from(s).is_ok(), expected, "rust on {s:?}");
            assert_eq!(
                validator.is_valid(&serde_json::Value::from(s)),
                expected,
                "schema on {s:?}"
            );
        }
    }

    #[test]
    fn versions() {
        let nines = format!("{0}.{0}.{0}", "9".repeat(9));
        let ten_digits = format!("1{}.0.0", "0".repeat(9));
        let long = format!("1.0.0-{}", "a".repeat(58));
        let too_long = format!("1.0.0-{}", "a".repeat(59));
        agree::<Version>(&[
            ("0.1.0", true),
            ("1.4.0", true),
            ("0.3.0-beta.1", true),
            ("1.0.0-0", true),
            ("1.0.0-0a", true),
            ("1.0.0-x-y-z.--", true),
            (&nines, true),
            (&long, true),
            (&too_long, false),
            (&ten_digits, false),
            ("1.0", false),
            ("1.0.0.0", false),
            ("01.0.0", false),
            ("1.0.0-01", false),
            ("1.0.0-", false),
            ("1.0.0-beta..1", false),
            ("1.0.0+build", false),
            ("v1.0.0", false),
            ("1.0.0 ", false),
            ("", false),
        ]);
        let v = Version::try_from("0.3.0-beta.1").expect("valid");
        assert_eq!(v.release(), (0, 3, 0));
        assert_eq!(v.pre_release(), Some("beta.1"));
    }

    #[test]
    fn core_requirements() {
        agree::<CoreRequirement>(&[
            (">=0.1.0", true),
            (">=0.1.0, <0.2.0", true),
            (">=0.1.0, <0.1.1", true),
            (">=0.1", false),
            ("^0.1.0", false),
            (">=0.1.0,<0.2.0", false),
            (">=0.1.0, <=0.2.0", false),
            (">=0.1.0-beta", false),
            ("0.1.0", false),
        ]);
        // Ordering is checked on construction; the schema can't compare the bounds.
        for empty in [">=0.2.0, <0.2.0", ">=0.2.0, <0.1.9"] {
            let e = CoreRequirement::try_from(empty).expect_err("matches no version");
            assert!(e.to_string().contains("matches no version"), "{e}");
        }
        let req = CoreRequirement::try_from(">=0.2.0, <0.3.0").expect("valid");
        let v = |s: &str| Version::try_from(s).expect("valid");
        assert!(!req.matches(&v("0.1.9")));
        assert!(req.matches(&v("0.2.0")));
        assert!(req.matches(&v("0.2.0-dev")));
        assert!(req.matches(&v("0.2.99")));
        assert!(!req.matches(&v("0.3.0")));
    }

    #[test]
    fn paths_and_hosts() {
        agree::<PackagePath>(&[
            ("bin/esphome", true),
            ("config.schema.json", true),
            ("_build/a-b.c", true),
            ("/bin/esphome", false),
            ("bin/", false),
            ("bin//esphome", false),
            ("../esphome", false),
            ("bin/.hidden", false),
            ("bin\\esphome", false),
        ]);
        agree::<SerialPath>(&[
            ("/dev/ttyUSB0", true),
            (
                "/dev/serial/by-id/usb-ITead_Sonoff_Zigbee_3.0_USB_Dongle_Plus-if00-port0",
                true,
            ),
            ("/dev", false),
            ("/dev/", false),
            ("/dev/../etc/passwd", false),
            ("ttyUSB0", false),
        ]);
        agree::<HostPath>(&[
            ("$CONFIG", true),
            ("$DATA/cache", true),
            ("/", true),
            ("/home/pi", true),
            ("$CONFIG/", false),
            ("/home/pi/", false),
            ("$HOME", false),
            ("home/pi", false),
            ("/home/../etc", false),
            ("", false),
        ]);
        let label63 = "a".repeat(63);
        let host253 = format!("{label63}.{label63}.{label63}.{}", "a".repeat(61));
        let host254 = format!("{host253}a");
        agree::<NetworkHost>(&[
            ("api.switch-bot.com", true),
            ("*.example.com", true),
            ("1password.com", true),
            ("local.example.com", true),
            ("example.local.com", true),
            ("192.168.1.20", false),
            ("8.8.8.8", false),
            ("example.123", false),
            ("localhost", false),
            ("nas", false),
            ("esp32-desk.local", false),
            ("*.local", false),
            ("printer.home.arpa", false),
            ("home.arpa", false),
            ("vault.internal", false),
            ("router.lan", false),
            ("nas.home", false),
            (&host253, true),
            (&format!("*.{host253}"), true),
            (&host254, false),
            (&format!("*.{host254}"), false),
            (&format!("{}.com", "a".repeat(64)), false),
            ("API.example.com", false),
            ("-api.example.com", false),
            ("api-.example.com", false),
            ("api..example.com", false),
            ("*.com", false),
            ("*.", false),
            ("*", false),
            ("a.*.example.com", false),
            ("example.com.", false),
            ("https://example.com", false),
        ]);
    }

    #[test]
    fn full_access() {
        let mut p = Permissions {
            host_fs: vec![HostPath::try_from("$CONFIG/rules").expect("valid")],
            ..Permissions::default()
        };
        assert!(!p.full_access());
        p.host_fs.push(HostPath::try_from("/home").expect("valid"));
        assert!(p.full_access());
        assert!(
            Permissions {
                host_shell: true,
                ..Permissions::default()
            }
            .full_access()
        );
    }
}
