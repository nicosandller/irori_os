# irori-protocol-switchbot

SwitchBot devices through SwitchBot's cloud API (v1.1) and over Bluetooth: meters and hubs,
contact, motion, presence and leak sensors, locks, the Bot, plugs and relays, curtains, roller
shades, blind tilts, the garage door opener, humidifiers and robot vacuums. With an account,
readings and commands go through SwitchBot's servers, which is also how devices behind a hub are
reached. The token and secret key stay on this machine. Devices near this machine are also read
from what they broadcast over Bluetooth, which needs no account and spends none of its calls.

## What it does

- **Lists the account's devices** at start and every 6 hours. A device is keyed by its
  `deviceId`, which is its MAC address (upper case, no colons), the same id it has over
  Bluetooth. A device reached through a hub says so (`via_device`), and the hub is described
  first. A device deleted in the SwitchBot app is removed at the next listing.
- **Lists what it can't use yet** as found but unusable: device types without a mapping (bulbs,
  fans, cameras…) and infrared remotes.
- **Reads each device's status**, one call at a time. Every new device is read once right after
  the listing. After that, calls are spread over the day so Irori's share lasts until midnight
  UTC, when SwitchBot's count starts again. Leak and contact sensors, locks and the garage door
  opener are read twice as often as the rest. A hub with no sensors of its own is never read.
- **Merges what SwitchBot says** into one reading per device, so a field missing from one answer
  keeps its last value. The device list and readings are kept between runs: at start they're
  described and reported as replayed straight away, even on a day SwitchBot refuses every call.
- **Carries out commands** and answers once SwitchBot has taken them. It shows no state it hasn't
  read: the device is read again 6 seconds later, and that reading is reported on behalf of the
  call. SwitchBot's "something went wrong" (190) is tried once more.
- **Marks a device unavailable** when SwitchBot says it (161) or its hub (171) is offline. A hub
  offline is marked unavailable too, and the extension's health names it.

## Bluetooth

SwitchBot devices broadcast their readings a few times a second, unencrypted, locks included.
The extension listens and reads them: a device is one Irori device whichever way it's reached,
because its Bluetooth broadcast carries the same MAC address the cloud uses as its id.

- **A device heard in the last 10 minutes isn't read through the cloud**, so the account's calls
  go to what's out of range.
- **With an account**, only the account's devices are taken; a neighbour's meter heard through
  the wall is ignored. A device removed from the account but still heard stays, over Bluetooth.
- **Without an account**, every SwitchBot device heard is found and named after its type and the
  end of its address ("Contact Sensor 5E1F"). One that falls quiet for 10 minutes is unavailable.
  After a day unheard the extension forgets it, so a neighbour's device passing through doesn't
  stay; one you added stays in the home, unavailable until it's heard again.
- **Commands go through the cloud** for now: without an account, devices are read but not
  controlled.
- **No adapter, or Bluetooth off**: with an account, the extension carries on through the cloud
  and logs why; without one, its health says Bluetooth isn't working and why. It tries again
  every minute.

**Adverts aren't signed.** Anyone nearby with a Bluetooth radio could broadcast an advert that
looks like one of your devices: a lock saying it's locked, a leak sensor saying it's dry. That
also keeps the cloud from being asked about that device for 10 minutes. Home Assistant's
SwitchBot Bluetooth integration reads them the same way. Commands aren't affected: they're
signed, and go through the cloud. Turn `bluetooth` off to read only from the cloud.

It uses [btleplug](https://github.com/deviceplug/btleplug): CoreBluetooth on macOS, BlueZ over
D-Bus on Linux. On Linux it needs `bluetoothd` running and the system D-Bus reachable (in Docker,
mount `/run/dbus`); no extra capabilities. The Linux build links libdbus statically, which is
used under the Academic Free License 2.1. On macOS the first scan may ask for Bluetooth access
for the app running Irori.

## Settings

```toml
[switchbot]
# In the SwitchBot app: Profile → Preferences → About, tap App Version ten times, then
# Developer Options. Both are secrets: kept in secrets.toml and never shown again.
token = "…"
secret = "…"
# Irori's share of the account's 10 000 calls a day (100–10 000). Default 5 000.
daily_calls = 5000
# Read nearby devices over Bluetooth. Default on.
bluetooth = true
# Which Bluetooth adapter, counting from 0, when the machine has more than one. Default 0.
bluetooth_adapter = 0
```

Give both the token and the secret key, or neither: without them, devices are found over
Bluetooth only.

## Sharing the account

SwitchBot allows 10 000 calls a day for the whole account, and Home Assistant or any other app
using the same token spends from them too. `daily_calls` is Irori's share; 10% of it is kept back
for commands.

SwitchBot answers "Unauthorized" both for a wrong token and for a day whose calls are used up. A
token that worked earlier the same UTC day and is then refused is taken as the day being used up:
the extension says so, stops calling, and starts again after midnight UTC. Only a token that
hasn't worked today is reported as wrong. A wrong token is tried again after an hour.

## Not yet

- Commands over Bluetooth, starting with the devices that don't need a key (Bot, curtains,
  shades, humidifier), then locks and relays with their key.
- SwitchBot's webhook, for changes as they happen.
- Infrared remotes, as buttons.
