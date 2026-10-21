#!/usr/bin/env node
// One emulated Matter device for the dev lab, on matter.js: a light, a plug, an in-wall switch,
// or an occupancy sensor, commissionable on the network (no Bluetooth) with a fixed discriminator and passcode
// so the Matter extension can add it with its manual pairing code.
//
//   node device.mjs --kind plug --name "Living Room Dehumidifier" --port 5541 \
//     --discriminator 3841 --passcode 20202022 --storage /var/lib/irori-lab/matter/plug \
//     [--vendor Tapo] [--model "Smart Wi-Fi Plug"] [--serial lab-3841]
//
// The occupancy sensor flips every 20 seconds, so a reading arrives without anyone in the room.
// The pairing code it prints is what "Add with a pairing code" takes.

import { Environment, ServerNode, VendorId } from "@matter/main";
import { OnOffLightDevice } from "@matter/main/devices/on-off-light";
import { OnOffPlugInUnitDevice } from "@matter/main/devices/on-off-plug-in-unit";
import { MountedOnOffControlDevice } from "@matter/main/devices/mounted-on-off-control";
import { OccupancySensorDevice } from "@matter/main/devices/occupancy-sensor";
import { OccupancySensingServer } from "@matter/main/behaviors/occupancy-sensing";

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
    light: OnOffLightDevice,
    plug: OnOffPlugInUnitDevice,
    switch: MountedOnOffControlDevice,
    occupancy: OccupancySensorDevice.with(OccupancySensingServer.with("PassiveInfrared")),
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

const endpoint = await node.add(
    type,
    kind === "occupancy"
        ? {
              id: kind,
              occupancySensing: {
                  occupancy: { occupied: false },
                  occupancySensorType: 0,
                  occupancySensorTypeBitmap: { pir: true },
              },
          }
        : { id: kind },
);

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

const { manualPairingCode } = node.state.commissioning.pairingCodes;
console.log(`irori lab: Matter ${kind} "${name}" on port ${port}, pairing code ${manualPairingCode}`);

await node.run();
