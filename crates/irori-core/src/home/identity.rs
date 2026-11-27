//! Recognising one device that two extensions both reach (`docs/specs/protocols.md` §6.1).
//!
//! A device's `unique_id` belongs to its extension, so the same plug reached through a bridge
//! to another hub and through its own extension would be two devices. An extension that can
//! reach what others reach says what hardware a device is (`DeviceDescription::connections`),
//! and the core compares that with what every other extension already says about its own
//! devices: a `unique_id` that is a MAC or IEEE address, a serial number.
//!
//! Nothing here exists until a device arrives with connections: a home whose extensions never
//! send any never builds the index, and describing or removing a device does no extra work.

use std::collections::{BTreeSet, HashMap};

use irori_types::{ConnectionKind, DeviceDescription, DeviceId, ProtocolId, normalized_address};

/// One identifier, typed, so a serial number never matches an address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Key {
    Mac(String),
    Ieee(String),
    Serial(String),
}

/// What the index knows about one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    protocol: ProtocolId,
    /// Whether its extension named its connections: the copy, when it matches one that didn't.
    carrier: bool,
    /// Who made it, in one spelling. A serial number is only unique within one maker.
    maker: Option<String>,
    /// Whether a person has it in the home: between two that name connections, the one to keep.
    in_home: bool,
    keys: BTreeSet<Key>,
}

impl Entry {
    pub fn of(protocol: &ProtocolId, description: &DeviceDescription, in_home: bool) -> Self {
        Self {
            protocol: protocol.clone(),
            carrier: !description.connections.is_empty(),
            maker: description
                .manufacturer
                .as_deref()
                .map(|maker| maker.trim().to_lowercase())
                .filter(|maker| !maker.is_empty()),
            in_home,
            keys: keys_of(description),
        }
    }

    /// The same device, now in the home or not.
    pub fn in_home(mut self, in_home: bool) -> Self {
        self.in_home = in_home;
        self
    }

    /// Whether a serial number they share can mean they're one device: the same maker, or one
    /// of them doesn't say.
    fn same_maker(&self, other: &Self) -> bool {
        match (&self.maker, &other.maker) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        }
    }
}

/// The identifiers a device is known by: its `unique_id` when it is an address, its serial
/// number, and its connections.
///
/// A `unique_id` is an address when, written as one spelling, it is 12 hex digits (a MAC:
/// ESPHome's, TP-Link's, SwitchBot's) or 16 (an IEEE address: Zigbee's, which Zigbee2MQTT
/// writes `zigbee2mqtt_0x…` and plain HA discovery `0x…`). Anything else, like Matter's
/// fabric and node or a Cast or ONVIF uuid, can only be matched by serial number.
pub(crate) fn keys_of(description: &DeviceDescription) -> BTreeSet<Key> {
    let mut keys = BTreeSet::new();
    let raw = description.unique_id.as_str();
    let address = normalized_address(raw.strip_prefix("zigbee2mqtt_").unwrap_or(raw));
    if address.chars().all(|c| c.is_ascii_hexdigit()) {
        match address.len() {
            12 => {
                keys.insert(Key::Mac(address));
            }
            16 => {
                keys.insert(Key::Ieee(address));
            }
            _ => {}
        }
    }
    if let Some(serial) = description.serial_number.as_deref().map(str::trim)
        && says_which(serial)
    {
        keys.insert(Key::Serial(serial.to_owned()));
    }
    for connection in &description.connections {
        let value = connection.value().to_owned();
        keys.insert(match connection.kind {
            ConnectionKind::Mac => Key::Mac(value),
            ConnectionKind::Ieee => Key::Ieee(value),
            ConnectionKind::Serial if says_which(&value) => Key::Serial(value),
            ConnectionKind::Serial => continue,
        });
    }
    keys
}

/// Whether a serial number can tell one device from another. A placeholder (`unknown`,
/// `TEST_SN`, `N/A`), a single character repeated (`0000`), a run of counting digits
/// (`123456789`) or one so short two makers could both use it can't: two extensions that each
/// have one device saying `unknown` would otherwise hide one of them. The ambiguity rule only
/// catches a placeholder once one extension uses it twice.
fn says_which(serial: &str) -> bool {
    const PLACEHOLDERS: [&str; 10] = [
        "unknown",
        "none",
        "null",
        "na",
        "default",
        "serial",
        "serialnumber",
        "notavailable",
        "test",
        "testsn",
    ];
    let plain: Vec<u8> = serial
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .collect();
    let repeated = plain.windows(2).all(|pair| pair[0] == pair[1]);
    let counting = plain.iter().all(u8::is_ascii_digit)
        && plain
            .windows(2)
            .all(|pair| pair[1] == pair[0] + 1 || (pair[0] == b'9' && pair[1] == b'0'));
    plain.len() >= 4
        && !repeated
        && !counting
        && !PLACEHOLDERS
            .iter()
            .any(|placeholder| placeholder.as_bytes() == plain)
}

/// Every device's identifiers, in the home or only found, and which devices are copies.
#[derive(Debug, Default)]
pub(crate) struct Identities {
    entries: HashMap<DeviceId, Entry>,
    by_key: HashMap<Key, BTreeSet<DeviceId>>,
    /// What each device was last worked out to be a copy of, so a change can be told apart.
    duplicates: HashMap<DeviceId, DeviceId>,
    /// In what order each device was first seen, for keeping the one that was there first.
    arrived: HashMap<DeviceId, u64>,
    next_arrival: u64,
}

impl Identities {
    /// Records what a device is now (`None`: it's gone). Returns the devices whose standing may
    /// have changed, or `None` when nothing about it did.
    pub fn set(&mut self, id: &DeviceId, entry: Option<Entry>) -> Option<BTreeSet<DeviceId>> {
        if self.entries.get(id) == entry.as_ref() {
            return None;
        }
        // Everything sharing an identifier with it, matched or not: a second device of one
        // extension can make an identifier ambiguous and undo another pair's match.
        let mut touched: BTreeSet<DeviceId> = self.sharing(id);
        touched.insert(id.clone());
        if let Some(old) = self.entries.remove(id) {
            for key in old.keys {
                if let Some(ids) = self.by_key.get_mut(&key) {
                    ids.remove(id);
                    if ids.is_empty() {
                        self.by_key.remove(&key);
                    }
                }
            }
        }
        if let Some(entry) = entry {
            if !self.arrived.contains_key(id) {
                self.arrived.insert(id.clone(), self.next_arrival);
                self.next_arrival += 1;
            }
            for key in &entry.keys {
                self.by_key
                    .entry(key.clone())
                    .or_default()
                    .insert(id.clone());
            }
            self.entries.insert(id.clone(), entry);
        } else {
            self.arrived.remove(id);
        }
        touched.extend(self.sharing(id));
        // A copy's standing decides its twin's `also_through`, and the twin's other copies'.
        let near: Vec<DeviceId> = touched.iter().cloned().collect();
        for other in near {
            touched.extend(self.matches(&other));
        }
        Some(touched)
    }

    /// Every device that shares any identifier with this one.
    fn sharing(&self, id: &DeviceId) -> BTreeSet<DeviceId> {
        let Some(entry) = self.entries.get(id) else {
            return BTreeSet::new();
        };
        entry
            .keys
            .iter()
            .filter_map(|key| self.by_key.get(key))
            .flatten()
            .filter(|other| *other != id)
            .cloned()
            .collect()
    }

    /// Whether one extension gives this identifier to more than one of its devices, like a
    /// placeholder serial number (`TEST_SN`) a maker never changed. Such an identifier says
    /// nothing about which device is which, so it matches nothing.
    fn ambiguous(&self, key: &Key) -> bool {
        let mut protocols = BTreeSet::new();
        self.by_key.get(key).is_some_and(|ids| {
            ids.iter()
                .filter_map(|id| self.entries.get(id))
                .any(|entry| !protocols.insert(&entry.protocol))
        })
    }

    /// The devices of other extensions that share an identifier with this one, in id order.
    fn matches(&self, id: &DeviceId) -> BTreeSet<DeviceId> {
        let Some(entry) = self.entries.get(id) else {
            return BTreeSet::new();
        };
        entry
            .keys
            .iter()
            .filter(|key| !self.ambiguous(key))
            .filter_map(|key| Some((key, self.by_key.get(key)?)))
            .flat_map(|(key, ids)| ids.iter().map(move |other| (key, other)))
            .filter(|(key, other)| {
                *other != id
                    && self.entries.get(*other).is_some_and(|them| {
                        them.protocol != entry.protocol
                            && (!matches!(key, Key::Serial(_)) || entry.same_maker(them))
                    })
            })
            .map(|(_, other)| other.clone())
            .collect()
    }

    /// Which of two devices that both name connections is kept, lowest first: the one in the
    /// home, then the one seen first, then by id. A total order, so two never point at each
    /// other, and installing another bridge can't push aside what a person already chose.
    fn standing<'a>(&self, id: &'a DeviceId) -> (bool, u64, &'a DeviceId) {
        (
            !self.entries.get(id).is_some_and(|entry| entry.in_home),
            self.arrived.get(id).copied().unwrap_or(u64::MAX),
            id,
        )
    }

    /// Which device this one is a copy of. Only a device that names its connections is ever a
    /// copy: of a matching device that doesn't, the first in id order; else, when every match
    /// names connections too, of the one with the best `standing`, if that's better than its
    /// own.
    pub fn duplicate_of(&self, id: &DeviceId) -> Option<DeviceId> {
        if !self.entries.get(id)?.carrier {
            return None;
        }
        let matches = self.matches(id);
        matches
            .iter()
            .find(|other| !self.entries[*other].carrier)
            .or_else(|| {
                matches
                    .iter()
                    .filter(|other| self.standing(other) < self.standing(id))
                    .min_by_key(|other| self.standing(other))
            })
            .cloned()
    }

    /// The extensions whose copies of this device are its duplicates.
    pub fn also_through(&self, id: &DeviceId) -> Vec<ProtocolId> {
        let mut protocols: Vec<ProtocolId> = self
            .matches(id)
            .into_iter()
            .filter(|other| self.duplicate_of(other).as_ref() == Some(id))
            .map(|other| self.entries[&other].protocol.clone())
            .collect();
        protocols.sort();
        protocols.dedup();
        protocols
    }

    /// Whether this device is a copy, as last worked out.
    pub fn is_duplicate(&self, id: &DeviceId) -> bool {
        self.duplicates.contains_key(id)
    }

    /// Records what this device was worked out to be a copy of; whether that changed.
    pub fn note_duplicate(&mut self, id: &DeviceId, of: Option<DeviceId>) -> bool {
        let changed = self.duplicates.get(id) != of.as_ref();
        match of {
            Some(of) => {
                self.duplicates.insert(id.clone(), of);
            }
            None => {
                self.duplicates.remove(id);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{Connection, Name, UniqueId};

    fn described(unique_id: &str, serial: Option<&str>) -> DeviceDescription {
        DeviceDescription {
            unique_id: UniqueId::try_from(unique_id).expect("valid"),
            name: Name::try_from("Plug").expect("valid"),
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            serial_number: serial.map(str::to_owned),
            suggested_area: None,
            via_device_unique_id: None,
            connections: Vec::new(),
        }
    }

    #[test]
    fn every_native_handle_that_is_an_address_is_read_as_one() {
        let mac = |s: &str| Key::Mac(s.to_owned());
        let ieee = |s: &str| Key::Ieee(s.to_owned());
        for (unique_id, key) in [
            ("AA:BB:CC:DD:EE:FF", Some(mac("aabbccddeeff"))), // ESPHome, TP-Link
            ("AABBCCDDEE04", Some(mac("aabbccddee04"))),      // SwitchBot
            ("0x0017880104E45520", Some(ieee("0017880104e45520"))), // HA discovery
            (
                "zigbee2mqtt_0x00158d0001a2b3c4",
                Some(ieee("00158d0001a2b3c4")),
            ), // Zigbee2MQTT
            ("0123456789ABCDEF-00000000000000AB", None),      // Matter: fabric and node
            ("8a9b2c4d-1e2f-4a3b-9c8d-7e6f5a4b3c2d", None),   // Cast, ONVIF
            ("01-202310", None),                              // a SwitchBot remote
            ("zigbee2mqtt_bridge_0x00124b0022aabbcc", None),  // the coordinator itself
        ] {
            let keys = keys_of(&described(unique_id, None));
            assert_eq!(keys.into_iter().next(), key, "{unique_id}");
        }
    }

    #[test]
    fn a_serial_is_only_ever_a_serial() {
        let keys = keys_of(&described("8a9b2c4d", Some("  AABBCCDDEEFF ")));
        assert_eq!(
            keys,
            BTreeSet::from([Key::Serial("AABBCCDDEEFF".to_owned())])
        );
    }

    #[test]
    fn a_placeholder_serial_is_no_key() {
        for serial in [
            "unknown",
            "N/A",
            "0",
            "0000-0000",
            "123",
            "Default",
            "TEST_SN",
            "test-sn",
            "123456789",
            "1234567890",
            "FFFF-FFFF",
        ] {
            assert!(
                keys_of(&described("8a9b2c4d", Some(serial))).is_empty(),
                "{serial}"
            );
            let mut description = described("ha-device-1", None);
            description.connections =
                vec![Connection::new(ConnectionKind::Serial, serial).expect("valid")];
            assert!(keys_of(&description).is_empty(), "{serial}");
        }
        assert!(!keys_of(&described("8a9b2c4d", Some("SN-42"))).is_empty());
    }

    #[test]
    fn connections_are_keys_of_their_own_kind() {
        let mut description = described("ha-device-1", None);
        description.connections = vec![
            Connection::new(ConnectionKind::Mac, "AA-BB-CC-DD-EE-FF").expect("valid"),
            Connection::new(ConnectionKind::Serial, "SN-1001").expect("valid"),
        ];
        assert_eq!(
            keys_of(&description),
            BTreeSet::from([
                Key::Mac("aabbccddeeff".to_owned()),
                Key::Serial("SN-1001".to_owned())
            ])
        );
    }
}
