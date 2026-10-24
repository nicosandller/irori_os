//! Golden adverts from pySwitchbot's own tests (`tests/test_adv_parser.py`), with the values
//! it reads from them. Generated; one test per advert.

use super::*;

fn advert(mfr: &[(u16, &str)], svc: &[(&str, &str)]) -> Advert {
    let bytes = |hex: &str| {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect::<Vec<u8>>()
    };
    Advert {
        address: Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
        manufacturer: mfr.iter().map(|(id, hex)| (*id, bytes(hex))).collect(),
        service: svc
            .iter()
            .map(|(uuid, hex)| ((*uuid).to_owned(), bytes(hex)))
            .collect(),
        rssi: Some(-60),
    }
}

#[test]
fn parse_advertisement_data_curtain() {
    let advert = advert(
        &[(2409, "e7ab46ac8f927c0f001104")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "63c058001104")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(88.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_passive() {
    let advert = advert(&[(2409, "e7ab46ac8f927c0f001104")], &[]);
    let reading = parse(&advert, Some("Curtain")).expect("read").reading;
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_passive_12_bytes() {
    let advert = advert(&[(2409, "e7ab46ac8f927c0f00110400")], &[]);
    let reading = parse(&advert, Some("Curtain")).expect("read").reading;
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_position_zero() {
    let advert = advert(
        &[(89, "c1c7277d55ab")],
        &[("00000d00-0000-1000-8000-00805f9b34fb", "63d0ce641104")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(78.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(0.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_firmware_six_position_100() {
    let advert = advert(
        &[(89, "f5989408a0e7"), (2409, "f5989408a0e79b0f002204")],
        &[
            ("00000d00-0000-1000-8000-00805f9b34fb", "63d048001204"),
            ("0000fd3d-0000-1000-8000-00805f9b34fb", "63c047002204"),
        ],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(71.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_firmware_six_position_100_other_rssi() {
    let advert = advert(
        &[(89, "f5989408a0e7"), (2409, "f5989408a0e7a50f632204")],
        &[
            ("00000d00-0000-1000-8000-00805f9b34fb", "63d048001204"),
            ("0000fd3d-0000-1000-8000-00805f9b34fb", "63c047632204"),
        ],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(71.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(1.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_fully_closed() {
    let advert = advert(
        &[(2409, "c1c7277d55ab220f641104")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "63c053641104")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(83.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(0.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_fully_open() {
    let advert = advert(
        &[(2409, "c1c7277d55ab250f001104")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "63c053001104")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(83.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_curtain3() {
    let advert = advert(
        &[(2409, "aabbccddeefff7070011040049")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7bc049001104")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(73.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_curtain3_passive() {
    let advert = advert(&[(2409, "aabbccddeefff7070011040049")], &[]);
    let reading = parse(&advert, Some("Curtain3")).expect("read").reading;
    assert_eq!(reading.battery, Some(73.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn parse_advertisement_data_contact() {
    let advert = advert(
        &[(2409, "e7ab46ac8f927c0f001104")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "64406405007500f812")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(100.0), "battery");
    assert_eq!(reading.contact_open, Some(true), "contact_open");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.bright, Some(true), "bright");
}

#[test]
fn new_bot_firmware() {
    let advert = advert(
        &[(89, "d82eadcd0d85")],
        &[("00000d00-0000-1000-8000-00805f9b34fb", "4810e1")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(97.0), "battery");
}

#[test]
fn parse_advertisement_data_curtain_firmware_six_fully_closed() {
    let advert = advert(
        &[(89, "ccf4c4f9ac6c"), (2409, "ccf4c4f9ac6ceb0f641204")],
        &[
            ("00000d00-0000-1000-8000-00805f9b34fb", "63d059641104"),
            ("0000fd3d-0000-1000-8000-00805f9b34fb", "63c064641204"),
        ],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(100.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(0.0), "position");
}

#[test]
fn parse_advertisement_data_curtain_firmware_six_fully_open() {
    let advert = advert(
        &[(89, "ccf4c4f9ac6c"), (2409, "ccf4c4f9ac6ce20f001204")],
        &[
            ("00000d00-0000-1000-8000-00805f9b34fb", "63d059641104"),
            ("0000fd3d-0000-1000-8000-00805f9b34fb", "63c064001204"),
        ],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(100.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(100.0), "position");
}

#[test]
fn contact_sensor_mfr() {
    let advert = advert(
        &[(2409, "cb39cdc43d46412c0046018fc4")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "6400da040046018fc4")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(90.0), "battery");
    assert_eq!(reading.contact_open, Some(true), "contact_open");
    assert_eq!(reading.motion, Some(false), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn contact_sensor_srv() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "6400da040046018fc4")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(90.0), "battery");
    assert_eq!(reading.contact_open, Some(true), "contact_open");
    assert_eq!(reading.motion, Some(false), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn contact_sensor_open() {
    let advert = advert(
        &[(2409, "cb39cdc43d46849c0017005144")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "6440da020017005144")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(90.0), "battery");
    assert_eq!(reading.contact_open, Some(true), "contact_open");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn contact_sensor_closed() {
    let advert = advert(
        &[(2409, "cb39cdc43d46898c002b001984")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "6440da00002b001984")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(90.0), "battery");
    assert_eq!(reading.contact_open, Some(false), "contact_open");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn switchbot_passive() {
    let advert = advert(&[], &[]);
    assert!(parse(&advert, Some("Bot")).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn bulb_active() {
    let advert = advert(
        &[(2409, "84f703b4cb7a03e4210000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "750064")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn wosensor_passive_and_active() {
    let advert = advert(
        &[(2409, "d7c17d5deb43de03069835")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "5400e4069835")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(24.6), "temperature");
    assert_eq!(reading.humidity, Some(53.0), "humidity");
    assert_eq!(reading.battery, Some(100.0), "battery");
}

#[test]
fn wosensor_active() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "5400e4069835")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(24.6), "temperature");
    assert_eq!(reading.humidity, Some(53.0), "humidity");
    assert_eq!(reading.battery, Some(100.0), "battery");
}

#[test]
fn wosensor_passive_only() {
    let advert = advert(&[(2409, "d7c17d5deb43de03069835")], &[]);
    let reading = parse(&advert, Some("Meter")).expect("read").reading;
    assert_eq!(reading.temperature, Some(24.6), "temperature");
    assert_eq!(reading.humidity, Some(53.0), "humidity");
}

#[test]
fn wosensor_active_zero_data() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "540000000000")],
    );
    assert!(parse(&advert, None).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn wosensor_active_positive_battery_and_zero_data() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "540050000000")],
    );
    assert!(parse(&advert, None).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn wohub2_passive_and_active() {
    let advert = advert(
        &[(2409, "aabbccddeeff00ff66541af182079a3200")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7600")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(26.7), "temperature");
    assert_eq!(reading.humidity, Some(50.0), "humidity");
    assert_eq!(reading.light_level, Some(2.0), "light_level");
}

#[test]
fn woiosensor_passive_and_active() {
    let advert = advert(
        &[(2409, "aabbccddeeffe00f06983500")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7700e4")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(24.6), "temperature");
    assert_eq!(reading.humidity, Some(53.0), "humidity");
    assert_eq!(reading.battery, Some(100.0), "battery");
}

#[test]
fn woiosensor_passive_only() {
    let advert = advert(&[(2409, "aabbccddeeffe00f06983500")], &[]);
    let reading = parse(&advert, Some("WoIOSensor")).expect("read").reading;
    assert_eq!(reading.temperature, Some(24.6), "temperature");
    assert_eq!(reading.humidity, Some(53.0), "humidity");
}

#[test]
fn motion_sensor_clear() {
    let advert = advert(
        &[(2409, "c0219ae8bc496a1c0066")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7300e2006601")],
    );
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.battery, Some(98.0), "battery");
    assert_eq!(reading.motion, Some(false), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn motion_sensor_clear_passive() {
    let advert = advert(&[(2409, "c0219ae8bc496a1c0066")], &[]);
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.motion, Some(false), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn motion_sensor_motion() {
    let advert = advert(
        &[(2409, "c0219ae8bc49695c0038")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7340e2003801")],
    );
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.battery, Some(98.0), "battery");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn motion_sensor_motion_passive() {
    let advert = advert(&[(2409, "c0219ae8bc49695c0038")], &[]);
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.bright, Some(false), "bright");
}

#[test]
fn motion_sensor_is_light_passive() {
    let advert = advert(
        &[(2409, "c0219ae8bc49732c0467")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7300e2046702")],
    );
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.battery, Some(98.0), "battery");
    assert_eq!(reading.motion, Some(false), "motion");
    assert_eq!(reading.bright, Some(true), "bright");
}

#[test]
fn motion_sensor_is_light_active() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7300e2046702")],
    );
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.battery, Some(98.0), "battery");
    assert_eq!(reading.motion, Some(false), "motion");
    assert_eq!(reading.bright, Some(true), "bright");
}

#[test]
fn motion_with_light_detected() {
    let advert = advert(
        &[(2409, "c0219ae8bc49766c002c")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7340e2002c02")],
    );
    let reading = parse(&advert, Some("Motion Sensor")).expect("read").reading;
    assert_eq!(reading.battery, Some(98.0), "battery");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.bright, Some(true), "bright");
}

#[test]
fn meter_pro_active() {
    let advert = advert(
        &[(2409, "b0e9fe52dd84066408972c0005")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "340064")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(23.8), "temperature");
    assert_eq!(reading.humidity, Some(44.0), "humidity");
    assert_eq!(reading.battery, Some(100.0), "battery");
}

#[test]
fn meter_pro_passive() {
    let advert = advert(&[(2409, "b0e9fe52dd84066408972c0005")], &[]);
    let reading = parse(&advert, Some("MeterPro")).expect("read").reading;
    assert_eq!(reading.temperature, Some(23.8), "temperature");
    assert_eq!(reading.humidity, Some(44.0), "humidity");
}

#[test]
fn meter_pro_c_active() {
    let advert = advert(
        &[(2409, "b0e9fe543215b7e4079ba4003702d500")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "350064")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(27.7), "temperature");
    assert_eq!(reading.humidity, Some(36.0), "humidity");
    assert_eq!(reading.battery, Some(100.0), "battery");
    assert_eq!(reading.co2, Some(725.0), "co2");
}

#[test]
fn meter_pro_c_passive() {
    let advert = advert(&[(2409, "b0e9fe543215b7e4079ba4003702d500")], &[]);
    let reading = parse(&advert, Some("MeterPro(CO2)")).expect("read").reading;
    assert_eq!(reading.temperature, Some(27.7), "temperature");
    assert_eq!(reading.humidity, Some(36.0), "humidity");
    assert_eq!(reading.co2, Some(725.0), "co2");
}

#[test]
fn parse_advertisement_data_keypad() {
    let advert = advert(
        &[(2409, "eb1302e6230f8f6400000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "790064")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn leak_active() {
    let advert = advert(
        &[(2409, "c43430374c7a184e98675e94513c0500000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "26004e")],
    );
    let reading = parse(&advert, Some("Water Detector"))
        .expect("read")
        .reading;
    assert_eq!(reading.battery, Some(78.0), "battery");
    assert_eq!(reading.leak, Some(false), "leak");
}

#[test]
fn leak_passive() {
    let advert = advert(&[(2409, "c43430374c7a184e98675e94513c0500000000")], &[]);
    let reading = parse(&advert, Some("Water Detector"))
        .expect("read")
        .reading;
    assert_eq!(reading.battery, Some(78.0), "battery");
    assert_eq!(reading.leak, Some(false), "leak");
}

#[test]
fn leak_no_leak_detected() {
    let advert = advert(
        &[(2409, "c43430374c7a4a64986761c40a3c0500000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "260064")],
    );
    let reading = parse(&advert, Some("Water Detector"))
        .expect("read")
        .reading;
    assert_eq!(reading.battery, Some(100.0), "battery");
    assert_eq!(reading.leak, Some(false), "leak");
}

#[test]
fn leak_leak_detected() {
    let advert = advert(
        &[(2409, "c43430374c7a4764f96761c4083c0500000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "260064")],
    );
    let reading = parse(&advert, Some("Water Detector"))
        .expect("read")
        .reading;
    assert_eq!(reading.battery, Some(100.0), "battery");
    assert_eq!(reading.leak, Some(true), "leak");
}

#[test]
fn leak_low_battery() {
    let advert = advert(
        &[(2409, "c43430374c7a020998000000003c0500000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "260064")],
    );
    let reading = parse(&advert, Some("Water Detector"))
        .expect("read")
        .reading;
    assert_eq!(reading.battery, Some(9.0), "battery");
    assert_eq!(reading.leak, Some(false), "leak");
}

#[test]
fn leak_real_data_from_ha() {
    let advert = advert(
        &[(
            2409,
            "5c78643634303744315c783032565c7839305c7830305c7830305c7830305c7830305c7831655c7830355c7830305c7830305c7830305c783030",
        )],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "265c78303056")],
    );
    let reading = parse(&advert, Some("Water Detector"))
        .expect("read")
        .reading;
    assert_eq!(reading.battery, Some(68.0), "battery");
    assert_eq!(reading.leak, Some(true), "leak");
}

#[test]
fn remote_active() {
    let advert = advert(
        &[(89, "aabbccddeeff")],
        &[("00000d00-0000-1000-8000-00805f9b34fb", "62205600")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn remote_passive() {
    let advert = advert(&[(89, "aabbccddeeff")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn universal_remote_active() {
    let advert = advert(
        &[(2409, "aabbccddeeff00500000000000000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2700")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn parse_advertisement_data_hubmini_matter() {
    let advert = advert(
        &[(2409, "e6a1cd1f5b650000000000001401983500")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2500")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn parse_advertisement_data_roller_shade() {
    let advert = advert(
        &[(2409, "b0e9fe54901b2c089f11042700")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2c00279f1104")],
    );
    let reading = parse(&advert, Some("Roller Shade")).expect("read").reading;
    assert_eq!(reading.battery, Some(39.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(69.0), "position");
}

#[test]
fn hubmini_matter_passive() {
    let advert = advert(&[(2409, "e6a1cd1f5b650000000000001401983500")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn roller_shade_passive() {
    let advert = advert(&[(2409, "b0e9fe54901b2c089f11042700")], &[]);
    let reading = parse(&advert, Some("Roller Shade")).expect("read").reading;
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(69.0), "position");
}

#[test]
fn circulator_fan_active() {
    let advert = advert(
        &[(2409, "b0e9fe5859a87e4c5239")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7e0052")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn circulator_fan_passive() {
    let advert = advert(&[(2409, "b0e9fe5859a87e4c5239")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn circulator_fan_pro_active() {
    let advert = advert(
        &[(2409, "b0e9fefdc0b19ad9980b000000000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "0000590011b340")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn circulator_fan_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7e0052")]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k20_active() {
    let advert = advert(
        &[(2409, "b0e9fe01f38f270111530010640f")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2e0064")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k20_passive() {
    let advert = advert(&[(2409, "b0e9fe01f38f270111530010640f")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k20_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2e0064")]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_pro_active() {
    let advert = advert(
        &[(2409, "b0e9fe508d8d022064")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2800")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_pro_passive() {
    let advert = advert(&[(2409, "b0e9fe508d8d022064")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_pro_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2800")]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_active() {
    let advert = advert(
        &[(2409, "ca3806a95ff1022064")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7d00")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_passive() {
    let advert = advert(&[(2409, "ca3806a95ff1022064")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7d00")]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_pro_combo_active() {
    let advert = advert(
        &[(2409, "b0e9fe01f41d0b0101b103113801")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "330000")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_pro_combo_passive() {
    let advert = advert(&[(2409, "b0e9fe01f41d0b0101b103113801")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn k10_pro_combo_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "330000")]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn s10_active() {
    let advert = advert(
        &[(2409, "b0e9fe00087c0a01110500104d02")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7a0000")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn s10_passive() {
    let advert = advert(&[(2409, "b0e9fe00087c0a01110500104d02")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn s10_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "7a0000")]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn air_purifier_passive() {
    let advert = advert(&[(2409, "f09e9e966ad6a18188e40001950000")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn air_purifier_with_empty_data() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "2b0000150400")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn hub3_active() {
    let advert = advert(
        &[(2409, "b0e9fe6e5e2900ff6826d6648303993480")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "0000640010b940")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.temperature, Some(25.3), "temperature");
    assert_eq!(reading.humidity, Some(52.0), "humidity");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.light_level, Some(3.0), "light_level");
}

#[test]
fn hub3_passive() {
    let advert = advert(&[(2409, "b0e9fe6e5e2900ff6826d6648303993480")], &[]);
    let reading = parse(&advert, Some("Hub 3")).expect("read").reading;
    assert_eq!(reading.temperature, Some(25.3), "temperature");
    assert_eq!(reading.humidity, Some(52.0), "humidity");
    assert_eq!(reading.motion, Some(true), "motion");
    assert_eq!(reading.light_level, Some(3.0), "light_level");
}

#[test]
fn hub3_with_empty_data() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "0000640010b940")],
    );
    assert!(parse(&advert, None).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn blind_tilt_active() {
    let advert = advert(
        &[(2409, "fc285c366c7f0b2700a184")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "780048")],
    );
    let reading = parse(&advert, None).expect("read").reading;
    assert_eq!(reading.battery, Some(72.0), "battery");
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(0.0), "position");
}

#[test]
fn blind_tilt_passive() {
    let advert = advert(&[(2409, "fc285c366c7f0b2700a184")], &[]);
    let reading = parse(&advert, Some("Blind Tilt")).expect("read").reading;
    assert_eq!(reading.moving, Some(false), "moving");
    assert_eq!(reading.calibrated, Some(true), "calibrated");
    assert_eq!(reading.position, Some(0.0), "position");
}

#[test]
fn blind_tilt_with_empty_data() {
    let advert = advert(&[], &[("0000fd3d-0000-1000-8000-00805f9b34fb", "780048")]);
    assert!(parse(&advert, None).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn humidifer_with_empty_data() {
    let advert = advert(
        &[],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "230000151c00")],
    );
    assert!(parse(&advert, None).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn adv_with_empty_data() {
    let advert = advert(&[], &[]);
    assert!(parse(&advert, None).is_none_or(|heard| heard.reading.battery.is_none()));
}

#[test]
fn weather_station_active() {
    let advert = advert(
        &[(2409, "aabbccddeeff0150069a230000000000")],
        &[("0000fd3d-0000-1000-8000-00805f9b34fb", "000050001053b0")],
    );
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

#[test]
fn weather_station_passive() {
    let advert = advert(&[(2409, "aabbccddeeff0150069a230000000000")], &[]);
    assert_eq!(parse(&advert, None), None, "not a model read here");
}

/// Adverts heard on a Mac from real devices (their addresses changed): macOS gives no address,
/// so the MAC comes from the advert itself.
fn heard_on_a_mac(mfr: &str, svc: &str) -> Advert {
    Advert {
        address: Some([0; 6]),
        ..advert(&[(2409, mfr)], &[(SERVICE, svc)])
    }
}

#[test]
fn a_meter_pro_co2_heard_on_a_mac() {
    let heard = parse(
        &heard_on_a_mac("a1b2c3d4e5f63f4c09963c001a044d20", "35004c"),
        None,
    )
    .expect("read");
    assert_eq!(heard.mac, "A1B2C3D4E5F6");
    assert_eq!(heard.device_type, "MeterPro(CO2)");
    assert_eq!(heard.reading.temperature, Some(22.9));
    assert_eq!(heard.reading.humidity, Some(60.0));
    assert_eq!(heard.reading.co2, Some(1101.0));
    assert_eq!(heard.reading.battery, Some(76.0));
}

#[test]
fn a_lock_pro_heard_on_a_mac() {
    let heard = parse(&heard_on_a_mac("a1b2c3d4e5f71d8208640000", "248064"), None).expect("read");
    assert_eq!(heard.device_type, "Smart Lock Pro");
    assert_eq!(heard.reading.bolt, Some(Bolt::Locked));
    assert_eq!(heard.reading.door_open, Some(false));
    assert_eq!(heard.reading.battery, Some(100.0));
    assert_eq!(heard.reading.calibrated, Some(true));
}

#[test]
fn a_keypad_heard_on_a_mac_is_not_read() {
    assert_eq!(
        parse(&heard_on_a_mac("a1b2c3d4e5f8f16300000000", "790063"), None),
        None
    );
}

#[test]
fn without_an_address_or_one_in_the_advert_there_is_no_device() {
    let advert = Advert {
        address: Some([0; 6]),
        ..advert(&[], &[(SERVICE, "54006418ae3d")])
    };
    assert_eq!(advert.mac(), None);
    assert_eq!(parse(&advert, None), None);
}

#[test]
fn locks_read_the_way_home_assistant_reads_them() {
    assert_eq!(bolt(0), Some(Bolt::Locked));
    assert_eq!(bolt(1), Some(Bolt::Unlocked));
    assert_eq!(bolt(2), Some(Bolt::Locking));
    assert_eq!(bolt(3), Some(Bolt::Unlocking));
    assert_eq!(bolt(4), Some(Bolt::Jammed));
    assert_eq!(bolt(5), Some(Bolt::Jammed));
    assert_eq!(bolt(6), Some(Bolt::Unlocked), "latch locked, EU");
    assert_eq!(bolt(7), Some(Bolt::Locked), "half locked");
}
