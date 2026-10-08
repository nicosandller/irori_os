//! What a device looks like on the plan: which glyph, which tone, and what — if anything — it
//! has to say.
//!
//! Decided by what the device's entities **can do** and what class they say they are, never by
//! what anything is called. All of it is plain data in and plain data out, so the rules are
//! tested here without a page to draw them on.

use irori_types::{
    Availability, BinarySensorClass, Capabilities, Entity, EntityId, EntityState, EventClass,
    HvacAction, HvacMode, LockStatus, MediaPlayerClass, Playback, SensorClass, SensorValue, State,
};

use crate::icons::Icon;

/// The kind of thing a device is, as a colour. One pastel each, so a plan full of round markers
/// can still be read at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Light,
    /// Switches and plugs.
    Power,
    /// TVs and speakers.
    Media,
    /// Motion, presence and mmWave sensors.
    Sense,
    /// Temperature and humidity.
    Climate,
    /// CO₂ and the rest of what's in the air.
    Air,
    /// Buttons and things that happen.
    Input,
    /// Locks.
    Secure,
    Other,
}

impl Tone {
    /// The name the stylesheet knows it by (`data-tone`).
    pub fn as_str(self) -> &'static str {
        match self {
            Tone::Light => "light",
            Tone::Power => "power",
            Tone::Media => "media",
            Tone::Sense => "sense",
            Tone::Climate => "climate",
            Tone::Air => "air",
            Tone::Input => "input",
            Tone::Secure => "secure",
            Tone::Other => "other",
        }
    }
}

/// What a device looks like on the plan right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    /// Whether the entity a click would switch is on. `None` when it can be switched but hasn't
    /// said yet — a device that has only just joined — which is still worth a click.
    pub on: Option<bool>,
    /// Whether the entity that speaks for the device is unreachable. Not whether anything on
    /// the device is: a lamp with a flaky signal sensor is still a lamp you can switch.
    pub offline: bool,
    /// The entity a click switches, if there is one.
    pub switch: Option<EntityId>,
    pub glyph: Icon,
    pub tone: Tone,
    /// What the marker opens out to say. `None` is a closed circle.
    pub reading: Option<String>,
    /// Doing something worth a second look: on, playing, sensing someone, too much CO₂.
    pub active: bool,
    /// A light that is on: the marker itself is lit.
    pub lit: bool,
    /// A motion sensor sensing someone: a ring going out from the marker.
    pub pulse: bool,
    /// The player a click plays or pauses, when it can be.
    pub media: Option<EntityId>,
    /// The button a click presses, when there is one.
    pub press: Option<EntityId>,
    /// The lock a click locks. Only ever locks: unlocking lets someone in, so it is asked for
    /// on the lock's own page, where it is asked twice.
    pub lock: Option<EntityId>,
    /// The entity whose happenings flash the marker: a remote's button, a doorbell.
    pub event: Option<EntityId>,
    /// A doorbell: its marker rings as well as flashing.
    pub rings: bool,
    /// A presence sensor that also says how far away its target is, so it can be aimed and
    /// its field drawn.
    pub radar: bool,
    /// Whether it senses someone right now.
    pub sensing: bool,
    /// How far away the nearest target is, in centimetres.
    pub distance: Option<f64>,
    /// The state in words, for the tooltip and for a screen reader.
    pub saying: String,
}

impl Look {
    fn plain(glyph: Icon, tone: Tone) -> Self {
        Self {
            on: None,
            offline: false,
            switch: None,
            glyph,
            tone,
            reading: None,
            active: false,
            lit: false,
            pulse: false,
            media: None,
            press: None,
            lock: None,
            event: None,
            rings: false,
            radar: false,
            sensing: false,
            distance: None,
            saying: String::new(),
        }
    }
}

/// A device's look, from its entities and what they last said. The first of these that fits
/// wins: a light, a player, a switch, a lock, a thermostat, a radar, a motion sensor, a
/// doorbell or a button, a thermometer, an air monitor, anything else with a reading.
///
/// Chosen by what an entity **can do**, not by what it has said. An entity that has reported
/// nothing yet still has a light's or a switch's capabilities, and a marker that refused to
/// switch it until it had spoken would be a dead control on a device that works.
///
/// An entity filed under configuration or diagnostics doesn't decide what the device *is* — a
/// blind's Calibrate button doesn't make it a button — but it can still lend a reading to the
/// one that does, as a plug's power sensor does.
pub fn look_for(entities: &[&Entity], states: &[EntityState]) -> Look {
    let reported = |id: &EntityId| states.iter().find(|state| &state.entity_id == id);
    let said = |id: &EntityId| reported(id).and_then(|state| state.state.as_ref());
    let unreachable = |id: &EntityId| {
        reported(id).is_some_and(|state| state.availability == Availability::Unavailable)
    };
    let number = |id: &EntityId| match said(id) {
        Some(State::Sensor(sensor)) => match sensor.value {
            SensorValue::Number(value) => Some(value),
            SensorValue::Text(_) => None,
        },
        _ => None,
    };
    let main = || {
        entities
            .iter()
            .copied()
            .filter(|entity| entity.entity_category.is_none())
    };
    // A sensor of one class, with its unit: from anywhere on the device.
    let sensor = |class: SensorClass| {
        entities
            .iter()
            .find_map(|entity| match &entity.capabilities {
                Capabilities::Sensor(caps) if caps.device_class == Some(class) => {
                    Some((&entity.id, caps.unit.as_deref()))
                }
                _ => None,
            })
    };
    let offline_words = |look: &mut Look| {
        if look.offline {
            look.saying = "Not answering".into();
        }
    };

    // 1. A light.
    if let Some(light) = main().find(|entity| matches!(entity.capabilities, Capabilities::Light(_)))
    {
        let on = match said(&light.id) {
            Some(State::Light(light)) => Some(light.on),
            _ => None,
        };
        let mut look = Look {
            on,
            offline: unreachable(&light.id),
            switch: Some(light.id.clone()),
            lit: on == Some(true),
            active: on == Some(true),
            saying: switch_words(on),
            ..Look::plain(Icon::Bulb, Tone::Light)
        };
        offline_words(&mut look);
        return look;
    }

    // 2. A media player.
    if let Some((player, caps)) = main().find_map(|entity| match &entity.capabilities {
        Capabilities::MediaPlayer(caps) => Some((entity, caps)),
        _ => None,
    }) {
        let glyph = match caps.device_class {
            Some(MediaPlayerClass::Tv) => Icon::Tv,
            _ => Icon::Speaker,
        };
        let playing = match said(&player.id) {
            Some(State::MediaPlayer(playing)) => Some(playing),
            _ => None,
        };
        let playback = playing.map(|playing| playing.state);
        let offline = unreachable(&player.id);
        // A player that is off has to be turned on before it can play; that is its own button
        // on its own page, not what a tap on the plan means.
        let asleep = matches!(playback, Some(Playback::Off | Playback::Standby));
        let mut look = Look {
            offline,
            media: (!asleep).then(|| player.id.clone()),
            ..Look::plain(glyph, Tone::Media)
        };
        match playback {
            Some(Playback::Playing | Playback::Buffering) => {
                let what = playing
                    .and_then(|playing| playing.app.clone().or_else(|| playing.title.clone()))
                    .unwrap_or_else(|| "Playing".into());
                look.reading = Some(format!("▶ {what}"));
                look.active = true;
                look.saying = "Playing — click to pause".into();
            }
            Some(Playback::Paused) => {
                look.reading = Some("Paused".into());
                look.saying = "Paused — click to play".into();
            }
            Some(Playback::Idle) => look.saying = "Idle — click to play".into(),
            Some(Playback::Off) => look.saying = "Off".into(),
            Some(Playback::Standby) => look.saying = "Standby".into(),
            None => {}
        }
        offline_words(&mut look);
        return look;
    }

    // 3. A switch or a plug.
    if let Some(switch) =
        main().find(|entity| matches!(entity.capabilities, Capabilities::Switch(_)))
    {
        let on = match said(&switch.id) {
            Some(State::Switch(switch)) => Some(switch.on),
            _ => None,
        };
        let watts = sensor(SensorClass::Power)
            .and_then(|(id, unit)| Some((number(id)?, unit.unwrap_or("W"))))
            .filter(|_| on == Some(true))
            .map(|(watts, unit)| format!("{watts:.0} {unit}"));
        let mut look = Look {
            on,
            offline: unreachable(&switch.id),
            switch: Some(switch.id.clone()),
            active: on == Some(true),
            saying: match &watts {
                Some(watts) => format!("On, {watts} — click to turn off"),
                None => switch_words(on),
            },
            reading: watts,
            ..Look::plain(Icon::Plug, Tone::Power)
        };
        offline_words(&mut look);
        return look;
    }

    // A lock. Shut and locked is the ordinary state and says nothing; anything else is worth
    // knowing from across the room, so it opens to say so.
    if let Some(lock) = main().find(|entity| matches!(entity.capabilities, Capabilities::Lock(_))) {
        let status = match said(&lock.id) {
            Some(State::Lock(lock)) => Some(lock.state),
            _ => None,
        };
        let (reading, saying, locked) = match status {
            Some(LockStatus::Locked) => (None, "Locked", true),
            Some(LockStatus::Locking) => (Some("Locking…"), "Locking", true),
            Some(LockStatus::Unlocked) => (Some("Unlocked"), "Unlocked — click to lock", false),
            Some(LockStatus::Unlocking) => (Some("Unlocking…"), "Unlocking", false),
            Some(LockStatus::Jammed) => (Some("Jammed"), "Jammed", false),
            Some(LockStatus::Open) => (Some("Open"), "Open", false),
            Some(LockStatus::Opening) => (Some("Opening…"), "Opening", false),
            None => (None, "", true),
        };
        let mut look = Look {
            offline: unreachable(&lock.id),
            reading: reading.map(Into::into),
            active: !locked,
            lock: (status == Some(LockStatus::Unlocked)).then(|| lock.id.clone()),
            saying: saying.into(),
            ..Look::plain(
                if locked { Icon::Lock } else { Icon::Unlocked },
                Tone::Secure,
            )
        };
        offline_words(&mut look);
        return look;
    }

    // A thermostat. Always says how warm it is, and what it's aiming for while it's on; it is
    // set from its own page, where there is room for a dial.
    if let Some(thermostat) =
        main().find(|entity| matches!(entity.capabilities, Capabilities::Climate(_)))
    {
        let climate = match said(&thermostat.id) {
            Some(State::Climate(climate)) => Some(climate),
            _ => None,
        };
        let now = climate.and_then(|climate| climate.current_temperature);
        let on = climate.is_some_and(|climate| climate.hvac_mode != HvacMode::Off);
        // What it's set to: one temperature, or the band it keeps between.
        let aim = climate.filter(|_| on).and_then(|climate| {
            match (
                climate.target_temperature,
                climate.target_temp_low,
                climate.target_temp_high,
            ) {
                (Some(target), _, _) => Some(format!("{}°", trimmed(target))),
                (None, Some(low), Some(high)) => {
                    Some(format!("{}–{}°", trimmed(low), trimmed(high)))
                }
                _ => None,
            }
        });
        let reading = match (now, &aim) {
            (Some(now), Some(aim)) => Some(format!("{now:.1}° → {aim}")),
            (Some(now), None) => Some(format!("{now:.1}°")),
            (None, Some(aim)) => Some(format!("→ {aim}")),
            (None, None) => None,
        };
        let doing = climate.and_then(|climate| climate.hvac_action);
        let to = aim
            .as_deref()
            .map(|aim| format!(" to {aim}"))
            .unwrap_or_default();
        let saying = match (on, doing) {
            (false, _) if climate.is_some() => "Off".to_owned(),
            (_, Some(HvacAction::Heating | HvacAction::Preheating)) => format!("Heating{to}"),
            (_, Some(HvacAction::Cooling)) => format!("Cooling{to}"),
            (_, Some(HvacAction::Drying)) => "Drying".to_owned(),
            (_, Some(HvacAction::Fan)) => "Fan running".to_owned(),
            (_, Some(HvacAction::Defrosting)) => "Defrosting".to_owned(),
            (true, _) if aim.is_some() => format!("Set{to}"),
            (true, _) => "On".to_owned(),
            (false, _) => String::new(),
        };
        let mut look = Look {
            offline: unreachable(&thermostat.id),
            reading,
            // Working right now, not merely switched on: a thermostat is on all winter.
            active: on
                && matches!(
                    doing,
                    Some(
                        HvacAction::Heating
                            | HvacAction::Preheating
                            | HvacAction::Cooling
                            | HvacAction::Drying
                            | HvacAction::Fan
                            | HvacAction::Defrosting
                    )
                ),
            saying,
            ..Look::plain(Icon::Thermostat, Tone::Climate)
        };
        offline_words(&mut look);
        return look;
    }

    // 4 and 5. Something that senses whether anyone is there.
    if let Some(presence) = main().find(|entity| {
        matches!(
            &entity.capabilities,
            Capabilities::BinarySensor(sensor)
                if matches!(
                    sensor.device_class,
                    Some(
                        BinarySensorClass::Motion
                            | BinarySensorClass::Occupancy
                            | BinarySensorClass::Presence
                    )
                )
        )
    }) {
        let sensing = matches!(said(&presence.id), Some(State::BinarySensor(sensor)) if sensor.on);
        let offline = unreachable(&presence.id);
        // With a distance to its target it is a radar: it points somewhere, and says how far.
        if let Some((id, unit)) = sensor(SensorClass::Distance) {
            let metres = number(id)
                .map(|value| in_metres(value, unit))
                .filter(|_| sensing);
            let reading = metres.map(|metres| format!("{metres:.1} m"));
            let mut look = Look {
                offline,
                active: sensing,
                radar: true,
                sensing,
                distance: metres.map(|metres| metres * 100.0),
                saying: match (&reading, sensing) {
                    (Some(reading), _) => format!("Someone {reading} away"),
                    (None, true) => "Someone's here".into(),
                    (None, false) => "Nobody there".into(),
                },
                reading,
                ..Look::plain(Icon::Person, Tone::Sense)
            };
            offline_words(&mut look);
            return look;
        }
        let mut look = Look {
            offline,
            active: sensing,
            pulse: sensing,
            sensing,
            saying: if sensing {
                "Someone's here"
            } else {
                "Nobody there"
            }
            .into(),
            ..Look::plain(Icon::Motion, Tone::Sense)
        };
        offline_words(&mut look);
        return look;
    }

    // 6. A button, or something that happens.
    let button = main().find(|entity| matches!(entity.capabilities, Capabilities::Button(_)));
    let event = main().find(|entity| matches!(entity.capabilities, Capabilities::Event(_)));
    if button.is_some() || event.is_some() {
        // A doorbell is an event that says it is one. It has nothing to press from here —
        // ringing somebody's bell from the plan isn't a thing — so it only ever tells.
        let bell = event.is_some_and(|event| {
            matches!(
                &event.capabilities,
                Capabilities::Event(caps) if caps.device_class == Some(EventClass::Doorbell)
            )
        });
        let offline = button
            .or(event)
            .is_some_and(|entity| unreachable(&entity.id));
        let mut look = Look {
            offline,
            press: button.filter(|_| !bell).map(|button| button.id.clone()),
            event: event.map(|event| event.id.clone()),
            saying: if bell {
                "Doorbell".into()
            } else if button.is_some() {
                "Click to press".into()
            } else {
                String::new()
            },
            rings: bell,
            ..Look::plain(if bell { Icon::Bell } else { Icon::Button }, Tone::Input)
        };
        offline_words(&mut look);
        return look;
    }

    // 7. Temperature, humidity, or both.
    let temperature = main_sensor(entities, SensorClass::Temperature);
    let humidity = main_sensor(entities, SensorClass::Humidity);
    if temperature.is_some() || humidity.is_some() {
        let degrees = temperature.and_then(|(id, unit)| {
            let value = number(id)?;
            Some(match unit {
                // Degrees of what goes without saying in the home it's measured in.
                Some(unit) if !unit.starts_with('°') => format!("{value:.1} {unit}"),
                _ => format!("{value:.1}°"),
            })
        });
        let damp = humidity
            .and_then(|(id, unit)| Some(format!("{:.0}{}", number(id)?, unit.unwrap_or("%"))));
        let reading = match (degrees, damp) {
            (Some(degrees), Some(damp)) => Some(format!("{degrees} · {damp}")),
            (one, other) => one.or(other),
        };
        let first = temperature.or(humidity).map(|(id, _)| id);
        let mut look = Look {
            offline: first.is_some_and(unreachable),
            saying: reading.clone().unwrap_or_default(),
            reading,
            ..Look::plain(Icon::Temperature, Tone::Climate)
        };
        offline_words(&mut look);
        return look;
    }

    // 8. What's in the air.
    if let Some((id, unit)) = main_sensor(entities, SensorClass::Co2) {
        let ppm = number(id);
        let reading = ppm.map(|ppm| format!("{ppm:.0} {}", unit.unwrap_or("ppm")));
        let mut look = Look {
            offline: unreachable(id),
            active: ppm.is_some_and(|ppm| ppm >= STUFFY),
            saying: reading.clone().unwrap_or_default(),
            reading,
            ..Look::plain(Icon::Air, Tone::Air)
        };
        offline_words(&mut look);
        return look;
    }

    // 9. Anything else with a reading.
    if let Some((gauge, caps)) = main().find_map(|entity| match &entity.capabilities {
        Capabilities::Sensor(caps) => Some((entity, caps)),
        _ => None,
    }) {
        let reading = match said(&gauge.id) {
            Some(State::Sensor(sensor)) => Some(match &sensor.value {
                SensorValue::Number(value) => with_unit(*value, caps.unit.as_deref()),
                SensorValue::Text(text) => text.clone(),
            }),
            _ => None,
        };
        let mut look = Look {
            offline: unreachable(&gauge.id),
            saying: reading.clone().unwrap_or_default(),
            reading,
            ..Look::plain(Icon::Dot, Tone::Other)
        };
        offline_words(&mut look);
        return look;
    }

    Look::plain(Icon::Dot, Tone::Other)
}

/// The CO₂ level, in parts per million, at which a room wants a window opened.
const STUFFY: f64 = 1000.0;

/// A sensor of one class that speaks for the device, with its unit.
fn main_sensor<'a>(
    entities: &[&'a Entity],
    class: SensorClass,
) -> Option<(&'a EntityId, Option<&'a str>)> {
    entities
        .iter()
        .filter(|entity| entity.entity_category.is_none())
        .find_map(|entity| match &entity.capabilities {
            Capabilities::Sensor(caps) if caps.device_class == Some(class) => {
                Some((&entity.id, caps.unit.as_deref()))
            }
            _ => None,
        })
}

fn switch_words(on: Option<bool>) -> String {
    match on {
        Some(true) => "On — click to turn off",
        Some(false) => "Off — click to turn on",
        None => "Click to turn on",
    }
    .into()
}

/// A distance in the metres the plan thinks in, whatever the sensor reports it in.
fn in_metres(value: f64, unit: Option<&str>) -> f64 {
    match unit {
        Some("cm") => value / 100.0,
        Some("mm") => value / 1000.0,
        Some("ft") => value * 0.3048,
        Some("in") => value * 0.0254,
        _ => value,
    }
}

/// A number to one decimal place, and to none when that decimal would be a nought.
fn trimmed(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

/// A number the way a marker says one: a decimal only when there is one, and its unit after it.
fn with_unit(value: f64, unit: Option<&str>) -> String {
    let figure = trimmed(value);
    match unit {
        None => figure,
        Some(unit) if unit == "%" || unit.starts_with('°') => format!("{figure}{unit}"),
        Some(unit) => format!("{figure} {unit}"),
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{
        BinarySensorCapabilities, BinarySensorState, ButtonCapabilities, Context, EntityCategory,
        EventCapabilities, LightCapabilities, LightState, MediaPlayerCapabilities,
        MediaPlayerState, Origin, SensorCapabilities, SensorState, SensorValueType,
        SwitchCapabilities, SwitchState, Timestamp,
    };

    use super::*;

    fn entity(id: &str, capabilities: Capabilities) -> Entity {
        Entity {
            id: id.parse().expect("a valid entity id"),
            protocol: "demo".parse().expect("a valid protocol id"),
            unique_id: id.replace('.', "-").parse().expect("a valid unique id"),
            name: "Thing".parse().expect("a valid name"),
            device_id: Some("thing".parse().expect("a valid device id")),
            area_id: None,
            capabilities,
            entity_category: None,
        }
    }

    fn says(id: &str, state: State) -> EntityState {
        let at: Timestamp = "2026-10-08T12:00:00Z".parse().expect("a valid timestamp");
        EntityState {
            entity_id: id.parse().expect("a valid entity id"),
            availability: Availability::Available,
            state: Some(state),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9"
                    .parse()
                    .expect("a valid context id"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    fn gauge(id: &str, class: SensorClass, unit: &str) -> Entity {
        entity(
            id,
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Number,
                device_class: Some(class),
                unit: Some(unit.into()),
                state_class: None,
                options: Vec::new(),
            }),
        )
    }

    fn reads(id: &str, value: f64) -> EntityState {
        says(
            id,
            State::Sensor(SensorState {
                value: SensorValue::Number(value),
            }),
        )
    }

    fn flag(id: &str, class: BinarySensorClass) -> Entity {
        entity(
            id,
            Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: Some(class),
            }),
        )
    }

    fn flagged(id: &str, on: bool) -> EntityState {
        says(id, State::BinarySensor(BinarySensorState { on }))
    }

    fn light() -> Entity {
        let capabilities: LightCapabilities =
            serde_json::from_value(serde_json::json!({})).expect("a light that only switches");
        entity("light.lamp", Capabilities::Light(capabilities))
    }

    fn lit(on: bool) -> EntityState {
        let state: LightState =
            serde_json::from_value(serde_json::json!({"on": on})).expect("a light's state");
        says("light.lamp", State::Light(state))
    }

    fn player(class: MediaPlayerClass) -> Entity {
        let capabilities: MediaPlayerCapabilities =
            serde_json::from_value(serde_json::json!({"device_class": class}))
                .expect("a player with nothing optional");
        entity("media_player.tv", Capabilities::MediaPlayer(capabilities))
    }

    fn playing(state: Playback, app: Option<&str>, title: Option<&str>) -> EntityState {
        let state: MediaPlayerState =
            serde_json::from_value(serde_json::json!({"state": state, "app": app, "title": title}))
                .expect("a player's state");
        says("media_player.tv", State::MediaPlayer(state))
    }

    fn plug() -> Entity {
        entity(
            "switch.plug",
            Capabilities::Switch(SwitchCapabilities::default()),
        )
    }

    fn switched(on: bool) -> EntityState {
        says("switch.plug", State::Switch(SwitchState { on }))
    }

    /// A light never opens out: on, the whole marker is lit instead.
    #[test]
    fn a_light_is_lit_or_not_and_never_says_more() {
        let lamp = light();
        let on = look_for(&[&lamp], &[lit(true)]);
        assert_eq!((on.tone, on.glyph), (Tone::Light, Icon::Bulb));
        assert!(on.lit && on.active && on.reading.is_none());
        assert_eq!(on.on, Some(true));
        assert_eq!(on.switch, Some(lamp.id.clone()));

        let off = look_for(&[&lamp], &[lit(false)]);
        assert!(!off.lit && !off.active && off.reading.is_none());

        let silent = look_for(&[&lamp], &[]);
        assert_eq!(silent.on, None, "it hasn't said");
        assert!(silent.switch.is_some(), "but it can still be switched");
    }

    #[test]
    fn a_plug_says_its_watts_only_while_it_is_on() {
        let (plug, power) = (plug(), gauge("sensor.plug_power", SensorClass::Power, "W"));
        let on = look_for(
            &[&plug, &power],
            &[switched(true), reads("sensor.plug_power", 41.6)],
        );
        assert_eq!((on.tone, on.glyph), (Tone::Power, Icon::Plug));
        assert_eq!(on.reading.as_deref(), Some("42 W"));
        assert!(on.active);

        let off = look_for(
            &[&plug, &power],
            &[switched(false), reads("sensor.plug_power", 0.4)],
        );
        assert!(off.reading.is_none() && !off.active);

        let bare = look_for(&[&plug], &[switched(true)]);
        assert!(
            bare.reading.is_none(),
            "nothing to say without a power sensor"
        );
        assert!(bare.active, "but it is on");
    }

    #[test]
    fn a_player_opens_while_it_plays_or_is_paused_and_closes_when_idle() {
        let tv = player(MediaPlayerClass::Tv);
        let on = look_for(
            &[&tv],
            &[playing(Playback::Playing, Some("Netflix"), Some("A film"))],
        );
        assert_eq!((on.tone, on.glyph), (Tone::Media, Icon::Tv));
        assert_eq!(on.reading.as_deref(), Some("▶ Netflix"));
        assert!(on.active);
        assert_eq!(on.media, Some(tv.id.clone()));

        let titled = look_for(&[&tv], &[playing(Playback::Playing, None, Some("A film"))]);
        assert_eq!(titled.reading.as_deref(), Some("▶ A film"));
        let anonymous = look_for(&[&tv], &[playing(Playback::Playing, None, None)]);
        assert_eq!(anonymous.reading.as_deref(), Some("▶ Playing"));

        let paused = look_for(&[&tv], &[playing(Playback::Paused, Some("Netflix"), None)]);
        assert_eq!(paused.reading.as_deref(), Some("Paused"));
        assert!(!paused.active);

        let idle = look_for(&[&tv], &[playing(Playback::Idle, None, None)]);
        assert!(idle.reading.is_none() && idle.media.is_some());
        let off = look_for(&[&tv], &[playing(Playback::Off, None, None)]);
        assert!(off.reading.is_none());
        assert!(off.media.is_none(), "a click doesn't turn a TV on");

        let speaker = player(MediaPlayerClass::Speaker);
        assert_eq!(look_for(&[&speaker], &[]).glyph, Icon::Speaker);
    }

    /// A radar is a presence sensor that also says how far away its target is.
    #[test]
    fn a_radar_says_how_far_away_someone_is() {
        let (presence, distance) = (
            flag("binary_sensor.radar", BinarySensorClass::Occupancy),
            gauge("sensor.radar_distance", SensorClass::Distance, "m"),
        );
        let sensing = look_for(
            &[&presence, &distance],
            &[
                flagged("binary_sensor.radar", true),
                reads("sensor.radar_distance", 1.84),
            ],
        );
        assert_eq!((sensing.tone, sensing.glyph), (Tone::Sense, Icon::Person));
        assert_eq!(sensing.reading.as_deref(), Some("1.8 m"));
        assert!(sensing.radar && sensing.sensing && sensing.active && !sensing.pulse);
        assert_eq!(sensing.distance, Some(184.0));

        let empty = look_for(
            &[&presence, &distance],
            &[
                flagged("binary_sensor.radar", false),
                reads("sensor.radar_distance", 1.84),
            ],
        );
        assert!(empty.radar && empty.reading.is_none() && empty.distance.is_none());

        let centimetres = gauge("sensor.radar_distance", SensorClass::Distance, "cm");
        let near = look_for(
            &[&presence, &centimetres],
            &[
                flagged("binary_sensor.radar", true),
                reads("sensor.radar_distance", 75.0),
            ],
        );
        assert_eq!(near.reading.as_deref(), Some("0.8 m"));
    }

    #[test]
    fn a_motion_sensor_pulses_and_never_opens() {
        // Motion comes before the thermometer it happens to carry.
        let (motion, warmth) = (
            flag("binary_sensor.hall", BinarySensorClass::Motion),
            gauge("sensor.hall_temperature", SensorClass::Temperature, "°C"),
        );
        let moving = look_for(
            &[&motion, &warmth],
            &[
                flagged("binary_sensor.hall", true),
                reads("sensor.hall_temperature", 21.0),
            ],
        );
        assert_eq!((moving.tone, moving.glyph), (Tone::Sense, Icon::Motion));
        assert!(moving.pulse && moving.reading.is_none() && !moving.radar);
        let still = look_for(&[&motion], &[flagged("binary_sensor.hall", false)]);
        assert!(!still.pulse && still.reading.is_none());

        // A door contact is a binary sensor too, and is nobody's motion sensor.
        let door = flag("binary_sensor.door", BinarySensorClass::Door);
        assert_eq!(look_for(&[&door], &[]).tone, Tone::Other);
    }

    #[test]
    fn a_button_never_opens_and_can_be_pressed() {
        let button = entity(
            "button.remote",
            Capabilities::Button(ButtonCapabilities::default()),
        );
        let look = look_for(&[&button], &[]);
        assert_eq!((look.tone, look.glyph), (Tone::Input, Icon::Button));
        assert!(look.reading.is_none());
        assert_eq!(look.press, Some(button.id.clone()));

        let bell = entity(
            "event.doorbell",
            Capabilities::Event(EventCapabilities {
                event_types: vec!["ring".into()],
                device_class: None,
            }),
        );
        let look = look_for(&[&bell], &[]);
        assert_eq!(look.tone, Tone::Input);
        assert!(look.reading.is_none() && look.press.is_none());
        assert_eq!(look.event, Some(bell.id.clone()));
    }

    /// A doorbell is a bell, rings, and can't be rung from the plan.
    #[test]
    fn a_doorbell_is_a_bell_that_only_tells() {
        let bell = entity(
            "event.doorbell",
            Capabilities::Event(EventCapabilities {
                event_types: vec!["ring".into()],
                device_class: Some(EventClass::Doorbell),
            }),
        );
        let look = look_for(&[&bell], &[]);
        assert_eq!((look.tone, look.glyph), (Tone::Input, Icon::Bell));
        assert!(look.rings && look.reading.is_none() && look.press.is_none());
        assert_eq!(look.event, Some(bell.id.clone()));
    }

    /// Locked is the ordinary state and says nothing. Anything else opens to say so, and the
    /// only thing a click ever does is lock.
    #[test]
    fn a_lock_says_when_it_is_not_locked_and_a_click_only_locks() {
        let lock = entity(
            "lock.front_door",
            Capabilities::Lock(irori_types::LockCapabilities::default()),
        );
        let stands = |status: &str| {
            let state: irori_types::LockState =
                serde_json::from_value(serde_json::json!({"state": status}))
                    .expect("a lock's state");
            look_for(&[&lock], &[says("lock.front_door", State::Lock(state))])
        };
        let locked = stands("locked");
        assert_eq!((locked.tone, locked.glyph), (Tone::Secure, Icon::Lock));
        assert!(locked.reading.is_none() && !locked.active);
        assert!(locked.lock.is_none(), "a click never unlocks");

        let unlocked = stands("unlocked");
        assert_eq!(unlocked.glyph, Icon::Unlocked);
        assert_eq!(unlocked.reading.as_deref(), Some("Unlocked"));
        assert!(unlocked.active);
        assert_eq!(unlocked.lock, Some(lock.id.clone()));

        let jammed = stands("jammed");
        assert_eq!(jammed.reading.as_deref(), Some("Jammed"));
        assert!(jammed.active && jammed.lock.is_none());
        assert_eq!(stands("locking").glyph, Icon::Lock);
        assert!(
            look_for(&[&lock], &[]).reading.is_none(),
            "nothing said yet"
        );
    }

    /// A thermostat always says how warm it is, says what it's aiming for while it's on, and is
    /// worth a second look only while it is actually heating or cooling.
    #[test]
    fn a_thermostat_says_how_warm_it_is_and_what_it_is_aiming_for() {
        let capabilities: irori_types::ClimateCapabilities = serde_json::from_value(
            serde_json::json!({"hvac_modes": ["off", "heat"], "min_temp": 5, "max_temp": 30, "temp_step": 0.5}),
        )
        .expect("a thermostat that heats");
        let thermostat = entity("climate.hall", Capabilities::Climate(capabilities));
        let stands = |state: serde_json::Value| {
            let state: irori_types::ClimateState =
                serde_json::from_value(state).expect("a thermostat's state");
            look_for(
                &[&thermostat],
                &[says("climate.hall", State::Climate(state))],
            )
        };

        let heating = stands(serde_json::json!({
            "hvac_mode": "heat", "hvac_action": "heating",
            "current_temperature": 19.42, "target_temperature": 22.0
        }));
        assert_eq!(
            (heating.tone, heating.glyph),
            (Tone::Climate, Icon::Thermostat)
        );
        assert_eq!(heating.reading.as_deref(), Some("19.4° → 22°"));
        assert!(heating.active);
        assert_eq!(heating.saying, "Heating to 22°");
        assert!(heating.switch.is_none(), "it is set from its own page");

        let resting = stands(serde_json::json!({
            "hvac_mode": "heat", "hvac_action": "idle",
            "current_temperature": 22.0, "target_temperature": 21.5
        }));
        assert_eq!(resting.reading.as_deref(), Some("22.0° → 21.5°"));
        assert!(!resting.active, "on, but not working");
        assert_eq!(resting.saying, "Set to 21.5°");

        let off = stands(serde_json::json!({
            "hvac_mode": "off", "current_temperature": 18.0, "target_temperature": 22.0
        }));
        assert_eq!(
            off.reading.as_deref(),
            Some("18.0°"),
            "no aim while it's off"
        );
        assert_eq!(off.saying, "Off");
        assert!(
            look_for(&[&thermostat], &[]).reading.is_none(),
            "nothing yet"
        );
    }

    /// A blind's Calibrate button is a setting of the blind, not what the blind is.
    #[test]
    fn a_setting_does_not_decide_what_a_device_is() {
        let mut calibrate = entity(
            "button.blind_calibrate",
            Capabilities::Button(ButtonCapabilities::default()),
        );
        calibrate.entity_category = Some(EntityCategory::Config);
        let look = look_for(&[&calibrate], &[]);
        assert_eq!(look.tone, Tone::Other);
        assert!(look.press.is_none());
    }

    #[test]
    fn a_thermometer_always_says_what_it_reads() {
        let (warmth, damp) = (
            gauge("sensor.air_temperature", SensorClass::Temperature, "°C"),
            gauge("sensor.air_humidity", SensorClass::Humidity, "%"),
        );
        let both = look_for(
            &[&warmth, &damp],
            &[
                reads("sensor.air_temperature", 21.42),
                reads("sensor.air_humidity", 48.3),
            ],
        );
        assert_eq!((both.tone, both.glyph), (Tone::Climate, Icon::Temperature));
        assert_eq!(both.reading.as_deref(), Some("21.4° · 48%"));

        let only_warmth = look_for(&[&warmth], &[reads("sensor.air_temperature", 19.0)]);
        assert_eq!(only_warmth.reading.as_deref(), Some("19.0°"));
        let only_damp = look_for(&[&damp], &[reads("sensor.air_humidity", 55.0)]);
        assert_eq!(only_damp.reading.as_deref(), Some("55%"));
        assert!(look_for(&[&warmth], &[]).reading.is_none(), "nothing yet");
    }

    #[test]
    fn stale_air_is_worth_a_second_look() {
        let air = gauge("sensor.air_co2", SensorClass::Co2, "ppm");
        let fresh = look_for(&[&air], &[reads("sensor.air_co2", 640.0)]);
        assert_eq!((fresh.tone, fresh.glyph), (Tone::Air, Icon::Air));
        assert_eq!(fresh.reading.as_deref(), Some("640 ppm"));
        assert!(!fresh.active);
        let stuffy = look_for(&[&air], &[reads("sensor.air_co2", 1000.0)]);
        assert!(stuffy.active);
    }

    #[test]
    fn anything_else_with_a_reading_says_it() {
        let lux = gauge("sensor.lux", SensorClass::Illuminance, "lx");
        let look = look_for(&[&lux], &[reads("sensor.lux", 320.0)]);
        assert_eq!((look.tone, look.glyph), (Tone::Other, Icon::Dot));
        assert_eq!(look.reading.as_deref(), Some("320 lx"));
        assert_eq!(look_for(&[], &[]).tone, Tone::Other);
        assert_eq!(with_unit(12.34, Some("%")), "12.3%");
        assert_eq!(with_unit(7.0, None), "7");
    }

    /// The order is the rule: a lamp that also measures its own power is a light.
    #[test]
    fn the_first_kind_that_fits_wins() {
        let (lamp, plug, power) = (
            light(),
            plug(),
            gauge("sensor.plug_power", SensorClass::Power, "W"),
        );
        assert_eq!(look_for(&[&power, &plug, &lamp], &[]).tone, Tone::Light);
        assert_eq!(look_for(&[&power, &plug], &[]).tone, Tone::Power);
    }

    #[test]
    fn a_device_that_is_not_answering_says_so() {
        let lamp = light();
        let mut gone = lit(true);
        gone.availability = Availability::Unavailable;
        let look = look_for(&[&lamp], &[gone]);
        assert!(look.offline);
        assert_eq!(look.saying, "Not answering");
    }
}
