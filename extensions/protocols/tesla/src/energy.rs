//! An energy site (a Powerwall, solar panels or both), as Irori reads it: its power flows now
//! (`live_status`), its settings (`site_info`), and today's energy (`calendar_history`).
//!
//! Today's energy is asked for without dates, as Home Assistant does: Tesla answers the site's
//! own day, in its own time zone, in buckets that add up to the day so far. So Irori never does
//! time-zone sums, and a day's totals start again at the site's midnight.

use std::collections::BTreeMap;

use irori_types::{BinarySensorClass, NumberMode, SensorClass, State, StateClass};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::Site;
use crate::entity::{self, Entity};

const LIVE: &[&str] = &[
    "solar_power",
    "battery_power",
    "load_power",
    "grid_power",
    "percentage_charged",
    "energy_left",
    "grid_status",
    "storm_mode_active",
];

const INFO: &[&str] = &[
    "backup_reserve_percent",
    "default_real_mode",
    "version",
    "battery_count",
    "components.grid",
    "components.storm_mode_capable",
    "user_settings.storm_mode_enabled",
];

/// Today's totals, as `calendar_history` names them, in Wh.
const TODAY: &[&str] = &[
    "total_solar_generation",
    "total_home_usage",
    "total_battery_charge",
    "total_battery_discharge",
    "grid_energy_imported",
    "total_grid_energy_exported",
];

const MODES: [&str; 3] = ["self_consumption", "autonomous", "backup"];

/// Everything known about a site. Kept in the store under `site/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteReading {
    pub live: BTreeMap<String, Value>,
    pub info: BTreeMap<String, Value>,
    /// Today's totals so far, in Wh.
    pub today: BTreeMap<String, f64>,
    /// The site's day those totals are for, as Tesla dates it.
    pub day: Option<String>,
}

pub fn stored(site: &str) -> String {
    format!("site/{site}")
}

/// `field` or `part.field` out of an answer.
fn pick(data: &Value, field: &str) -> Option<Value> {
    let value = match field.split_once('.') {
        Some((part, name)) => data.get(part)?.get(name)?,
        None => data.get(field)?,
    };
    (!value.is_null() && !value.is_object() && !value.is_array()).then(|| value.clone())
}

fn take(into: &mut BTreeMap<String, Value>, data: &Value, fields: &[&str]) -> bool {
    let before = into.clone();
    for field in fields {
        match pick(data, field) {
            Some(value) => {
                into.insert((*field).to_owned(), value);
            }
            None => {
                into.remove(*field);
            }
        }
    }
    *into != before
}

impl SiteReading {
    pub fn take_live(&mut self, data: &Value) -> bool {
        take(&mut self.live, data, LIVE)
    }

    pub fn take_info(&mut self, data: &Value) -> bool {
        take(&mut self.info, data, INFO)
    }

    /// Adds up the day's buckets.
    pub fn take_history(&mut self, data: &Value) -> bool {
        let buckets = data
            .get("time_series")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut today: BTreeMap<String, f64> = BTreeMap::new();
        for bucket in &buckets {
            for field in TODAY {
                if let Some(wh) = bucket.get(*field).and_then(Value::as_f64) {
                    *today.entry((*field).to_owned()).or_default() += wh;
                }
            }
        }
        let day = buckets
            .first()
            .and_then(|bucket| bucket.get("timestamp"))
            .and_then(Value::as_str)
            .and_then(|stamp| stamp.get(..10))
            .map(str::to_owned);
        let changed = today != self.today || day != self.day;
        self.today = today;
        self.day = day;
        changed
    }

    fn live_number(&self, field: &str) -> Option<f64> {
        self.live.get(field)?.as_f64()
    }

    fn info_flag(&self, field: &str) -> Option<bool> {
        self.info.get(field)?.as_bool()
    }

    fn kwh_today(&self, field: &str) -> Option<f64> {
        self.today.get(field).map(|wh| (wh / 10.0).round() / 100.0)
    }

    pub fn version(&self) -> Option<String> {
        self.info
            .get("version")?
            .as_str()?
            .split_whitespace()
            .next()
            .map(str::to_owned)
    }
}

/// The site's entities: a battery's when it has one, solar's when it has panels, the grid's
/// unless the site says it's off the grid.
pub fn entities(site: &Site, reading: &SiteReading) -> Vec<Entity> {
    use SensorClass as C;
    use StateClass::{Measurement, TotalIncreasing};
    let power = |key, name| entity::measured(key, name, Some(C::Power), "W", Measurement);
    let today = |key, name| entity::measured(key, name, Some(C::Energy), "kWh", TotalIncreasing);
    let mut list = vec![
        power("home_power", "Home power"),
        today("home_today", "Home energy today"),
    ];
    if site.solar {
        list.push(power("solar_power", "Solar power"));
        list.push(today("solar_today", "Solar energy today"));
    }
    if site.battery {
        list.push(entity::measured(
            "battery",
            "Battery",
            Some(C::Battery),
            "%",
            Measurement,
        ));
        list.push(power("battery_power", "Battery power"));
        list.push(entity::measured(
            "energy_left",
            "Energy left",
            Some(C::EnergyStorage),
            "kWh",
            Measurement,
        ));
        list.push(today("battery_charged_today", "Battery charged today"));
        list.push(today(
            "battery_discharged_today",
            "Battery discharged today",
        ));
        list.push(
            entity::number(
                "backup_reserve",
                "Backup reserve",
                (0.0, 100.0, 1.0),
                "%",
                NumberMode::Slider,
            )
            .config(),
        );
        list.push(entity::select("operation_mode", "Operation mode", &MODES).config());
        if reading.info_flag("components.storm_mode_capable") != Some(false) {
            list.push(entity::switch("storm_mode", "Storm watch").config());
        }
    }
    if reading.info_flag("components.grid") != Some(false) {
        list.push(power("grid_power", "Grid power"));
        list.push(entity::binary("grid", "Grid", BinarySensorClass::Power));
        list.push(today("grid_imported_today", "Grid energy imported today"));
        list.push(today("grid_exported_today", "Grid energy exported today"));
    }
    list
}

pub fn state(key: &str, reading: &SiteReading) -> Option<State> {
    let live = |field: &str| entity::reading(reading.live_number(field));
    let today = |field: &str| entity::reading(reading.kwh_today(field));
    match key {
        "home_power" => live("load_power"),
        "solar_power" => live("solar_power"),
        "battery_power" => live("battery_power"),
        "grid_power" => live("grid_power"),
        "battery" => entity::reading(
            reading
                .live_number("percentage_charged")
                .map(|percent| (percent * 10.0).round() / 10.0),
        ),
        "energy_left" => entity::reading(
            reading
                .live_number("energy_left")
                .map(|wh| (wh / 10.0).round() / 100.0),
        ),
        "grid" => entity::on(
            reading
                .live
                .get("grid_status")
                .and_then(Value::as_str)
                .map(|status| status == "Active"),
        ),
        "home_today" => today("total_home_usage"),
        "solar_today" => today("total_solar_generation"),
        "battery_charged_today" => today("total_battery_charge"),
        "battery_discharged_today" => today("total_battery_discharge"),
        "grid_imported_today" => today("grid_energy_imported"),
        "grid_exported_today" => today("total_grid_energy_exported"),
        "backup_reserve" => entity::set_to(
            reading
                .info
                .get("backup_reserve_percent")
                .and_then(Value::as_f64),
        ),
        "operation_mode" => entity::chosen(
            reading
                .info
                .get("default_real_mode")
                .and_then(Value::as_str)
                .filter(|mode| MODES.contains(mode)),
        ),
        "storm_mode" => entity::switched(reading.info_flag("user_settings.storm_mode_enabled")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    pub fn answer(file: &str) -> Value {
        let text = match file {
            "live_status" => include_str!("../testdata/live_status.json"),
            "site_info" => include_str!("../testdata/site_info.json"),
            _ => include_str!("../testdata/energy_history.json"),
        };
        let data: Value = serde_json::from_str(text).expect("json");
        data["response"].clone()
    }

    fn site() -> Site {
        Site {
            id: "2252000000000001".into(),
            name: "Home".into(),
            battery: true,
            solar: true,
        }
    }

    fn value(reading: &SiteReading, key: &str) -> Option<Value> {
        state(key, reading).and_then(|state| serde_json::to_value(state).ok())
    }

    #[test]
    fn a_site_reads_its_flows_settings_and_day() {
        let mut reading = SiteReading::default();
        assert!(reading.take_live(&answer("live_status")));
        assert!(reading.take_info(&answer("site_info")));
        assert!(reading.take_history(&answer("energy_history")));
        assert_eq!(
            value(&reading, "solar_power"),
            Some(json!({"kind": "sensor", "value": 1185.0}))
        );
        assert_eq!(
            value(&reading, "battery"),
            Some(json!({"kind": "sensor", "value": 95.5}))
        );
        assert_eq!(
            value(&reading, "energy_left"),
            Some(json!({"kind": "sensor", "value": 38.9}))
        );
        assert_eq!(
            value(&reading, "grid"),
            Some(json!({"kind": "binary_sensor", "on": true}))
        );
        assert_eq!(
            value(&reading, "solar_today"),
            Some(json!({"kind": "sensor", "value": 211.88}))
        );
        assert_eq!(
            value(&reading, "operation_mode"),
            Some(json!({"kind": "select", "option": "self_consumption"}))
        );
        assert_eq!(
            value(&reading, "storm_mode"),
            Some(json!({"kind": "switch", "on": true}))
        );
        assert_eq!(reading.day.as_deref(), Some("2026-11-10"));
        assert_eq!(reading.version().as_deref(), Some("25.10.1"));
        assert!(
            !reading.take_history(&answer("energy_history")),
            "nothing new"
        );
    }

    #[test]
    fn a_solar_only_site_has_no_battery() {
        let solar = Site {
            battery: false,
            ..site()
        };
        let keys: Vec<&str> = entities(&solar, &SiteReading::default())
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert!(keys.contains(&"solar_today"));
        assert!(!keys.contains(&"backup_reserve"));
        assert!(keys.contains(&"grid"));
    }

    #[test]
    fn every_entity_and_state_is_one_the_core_takes() {
        let manifest = irori_protocol::parse_manifest(include_str!("../irori-extension.toml"))
            .expect("a valid manifest");
        let kinds = &manifest.contributes.protocol[0].entity_kinds;
        let mut reading = SiteReading::default();
        reading.take_live(&answer("live_status"));
        reading.take_info(&answer("site_info"));
        reading.take_history(&answer("energy_history"));
        let mut keys = std::collections::BTreeSet::new();
        for entity in entities(&site(), &reading) {
            assert!(keys.insert(entity.key), "{} twice", entity.key);
            let description = irori_types::EntityDescription {
                unique_id: entity::id("2252000000000001", entity.key).expect("an id"),
                name: irori_types::Name::try_from(entity.name).ok(),
                device_unique_id: None,
                suggested_object_id: None,
                capabilities: entity.capabilities.clone(),
                entity_category: entity.category,
            };
            let wire = serde_json::to_value(&description).expect("serialises");
            let back: irori_types::EntityDescription =
                serde_json::from_value(wire).expect("the core takes it");
            assert!(kinds.contains(&back.capabilities.kind()));
            let state =
                state(entity.key, &reading).unwrap_or_else(|| panic!("{} has a state", entity.key));
            let wire = serde_json::to_value(&state).expect("serialises");
            let _: State = serde_json::from_value(wire).expect("a valid state");
        }
    }
}
