#!/usr/bin/env node
// One emulated Matter device for the dev lab, on matter.js, commissionable on the network (no
// Bluetooth) with a fixed discriminator and passcode so the Matter extension can add it with its
// manual pairing code. Kinds: light (dimmable, colour), plug, switch (in-wall), occupancy, lock,
// blind, thermostat, fan.
//
//   node device.mjs --kind plug --name "Living Room Dehumidifier" --port 5541 \
//     --discriminator 3841 --passcode 20202022 --storage /var/lib/irori-lab/matter/plug \
//     [--vendor Tapo] [--model "Smart Wi-Fi Plug"] [--serial lab-3841]
//
// Each answers commands the way matter.js's own servers do: the lock locks, the blind moves
// to where it's sent, the thermostat keeps its setpoints. The occupancy sensor flips every 20
// seconds and the thermostat's room drifts towards its setpoint, so readings arrive without
// anyone touching them. The pairing code it prints is what "Add with a pairing code" takes.

import { Environment, ServerNode, VendorId } from "@matter/main";
import { OnOffLightDevice } from "@matter/main/devices/on-off-light";
import { OnOffPlugInUnitDevice } from "@matter/main/devices/on-off-plug-in-unit";
import { MountedOnOffControlDevice } from "@matter/main/devices/mounted-on-off-control";
import { OccupancySensorDevice } from "@matter/main/devices/occupancy-sensor";
import { OccupancySensingServer } from "@matter/main/behaviors/occupancy-sensing";
import { ExtendedColorLightDevice } from "@matter/main/devices/extended-color-light";
import { DoorLockDevice } from "@matter/main/devices/door-lock";
import { WindowCoveringDevice } from "@matter/main/devices/window-covering";
import { ThermostatDevice } from "@matter/main/devices/thermostat";
import { FanDevice } from "@matter/main/devices/fan";
import { ColorControlServer } from "@matter/main/behaviors/color-control";
import { DoorLockServer } from "@matter/main/behaviors/door-lock";
import { WindowCoveringServer } from "@matter/main/behaviors/window-covering";
import { ThermostatServer } from "@matter/main/behaviors/thermostat";
import { FanControlServer } from "@matter/main/behaviors/fan-control";

function option(name, fallback) {
    const at = process.argv.indexOf(`--${name}`);
    if (at === -1 || at + 1 >= process.argv.length) {
        if (fallback === undefined) {
            console.error(`irori lab: --${name} is needed`);
            process.exit(2);
        }
        return fallback;
    }
    return process.argv[at + 1];
}

const kind = option("kind");
const name = option("name");
const port = Number(option("port"));
const discriminator = Number(option("discriminator"));
const passcode = Number(option("passcode"));
const vendor = option("vendor", "Irori Lab");
const model = option("model", kind);
const serial = option("serial", `lab-${discriminator}`);

Environment.default.vars.set("path.root", option("storage"));

const types = {
    onoff_light: OnOffLightDevice,
    light: ExtendedColorLightDevice.with(ColorControlServer.with("HueSaturation", "Xy", "ColorTemperature")),
    plug: OnOffPlugInUnitDevice,
    switch: MountedOnOffControlDevice,
    occupancy: OccupancySensorDevice.with(OccupancySensingServer.with("PassiveInfrared")),
    lock: DoorLockDevice.with(DoorLockServer.with("Unbolting")),
    blind: WindowCoveringDevice.with(WindowCoveringServer.with("Lift", "PositionAwareLift")),
    thermostat: ThermostatDevice.with(ThermostatServer.with("Heating", "Cooling", "AutoMode")),
    fan: FanDevice.with(FanControlServer.with("MultiSpeed", "Auto", "Rocking")),
};

// What each kind starts as, besides its endpoint id.
const initial = {
    occupancy: {
        occupancySensing: {
            occupancy: { occupied: false },
            occupancySensorType: 0,
            occupancySensorTypeBitmap: { pir: true },
        },
    },
    light: {
        colorControl: {
            colorMode: 2,
            enhancedColorMode: 2,
            colorTemperatureMireds: 370,
            colorTempPhysicalMinMireds: 153,
            colorTempPhysicalMaxMireds: 500,
            coupleColorTempToLevelMinMireds: 153,
            startUpColorTemperatureMireds: 370,
        },
    },
    lock: {
        doorLock: { lockState: 1, lockType: 0, actuatorEnabled: true, operatingMode: 0 },
    },
    blind: {
        windowCovering: {
            type: 0,
            endProductType: 0,
            currentPositionLiftPercent100ths: 0,
            targetPositionLiftPercent100ths: 0,
        },
    },
    thermostat: {
        thermostat: {
            localTemperature: 1900,
            occupiedHeatingSetpoint: 2100,
            occupiedCoolingSetpoint: 2500,
            minSetpointDeadBand: 2,
            systemMode: 4,
            controlSequenceOfOperation: 4,
        },
    },
    fan: {
        fanControl: {
            fanMode: 0,
            fanModeSequence: 2,
            percentSetting: 0,
            percentCurrent: 0,
            speedMax: 10,
            speedSetting: 0,
            speedCurrent: 0,
            rockSupport: { rockLeftRight: true },
            rockSetting: { rockLeftRight: false },
        },
    },
};
const type = types[kind];
if (type === undefined) {
    console.error(`irori lab: --kind is one of ${Object.keys(types).join(", ")}`);
    process.exit(2);
}

const node = await ServerNode.create({
    id: serial,
    network: { port },
    commissioning: { passcode, discriminator },
    productDescription: { name, deviceType: type.deviceType },
    basicInformation: {
        vendorName: vendor,
        vendorId: VendorId(0xfff1),
        nodeLabel: name,
        productName: model,
        productLabel: model,
        productId: 0x8000,
        serialNumber: serial,
        // Not the serial number: the spec says the two differ.
        uniqueId: `irori-${serial}`,
    },
});

const endpoint = await node.add(type, { id: kind, ...initial[kind] });

if (kind === "occupancy") {
    let occupied = false;
    // Unref'd: once matter.js has closed the device on SIGTERM, this mustn't keep Node alive.
    setInterval(() => {
        occupied = !occupied;
        endpoint.set({ occupancySensing: { occupancy: { occupied } } }).catch(error => {
            console.error(`irori lab: couldn't change occupancy: ${error}`);
        });
    }, 20_000).unref();
}

if (kind === "thermostat") {
    // The room drifts half a degree a minute towards what the thermostat is set to.
    setInterval(() => {
        const { localTemperature, occupiedHeatingSetpoint, occupiedCoolingSetpoint, systemMode } =
            endpoint.state.thermostat;
        const target = systemMode === 3 ? occupiedCoolingSetpoint : occupiedHeatingSetpoint;
        if (systemMode === 0 || localTemperature === null || localTemperature === target) {
            return;
        }
        const step = Math.sign(target - localTemperature) * Math.min(50, Math.abs(target - localTemperature));
        endpoint.set({ thermostat: { localTemperature: localTemperature + step } }).catch(error => {
            console.error(`irori lab: couldn't change the temperature: ${error}`);
        });
    }, 60_000).unref();
}

if (kind === "fan") {
    // As the spec has a fan do it: its speed follows what it's set to, a speed starts a fan that
    // was off (FanMode High), 0 % stops it, and turned off it stops. Each only changes what
    // differs, so the two handlers don't set each other off.
    const OFF = 0;
    const HIGH = 3;
    const AUTO = 5;
    const fan = () => endpoint.state.fanControl;
    const set = change =>
        endpoint.set({ fanControl: change }).catch(error => console.error(`irori lab: the fan: ${error}`));
    endpoint.events.fanControl.percentSetting$Changed.on(percent => {
        const change = { percentCurrent: percent ?? 0 };
        if (percent === 0 && fan().fanMode !== OFF) change.fanMode = OFF;
        if (percent > 0 && fan().fanMode === OFF) change.fanMode = HIGH;
        set(change);
    });
    endpoint.events.fanControl.fanMode$Changed.on(mode => {
        if (mode === OFF && fan().percentSetting !== 0) set({ percentSetting: 0, percentCurrent: 0 });
        if (mode !== OFF && mode !== AUTO && fan().percentSetting === 0) set({ percentSetting: 100, percentCurrent: 100 });
    });
}

const { manualPairingCode } = node.state.commissioning.pairingCodes;
console.log(`irori lab: Matter ${kind} "${name}" on port ${port}, pairing code ${manualPairingCode}`);

await node.run();
