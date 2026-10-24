# irori-protocol-switchbot

SwitchBot devices through SwitchBot's cloud API (v1.1): meters and hubs, contact, motion,
presence and leak sensors, locks, the Bot, plugs and relays, curtains, roller shades, blind tilts,
the garage door opener, humidifiers and robot vacuums. Readings and commands go through
SwitchBot's servers, which is also how devices behind a hub are reached. The token and secret key
stay on this machine.

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
  keeps its last value. Readings are kept between runs and reported as replayed at start.
- **Carries out commands** and answers once SwitchBot has taken them. It shows no state it hasn't
  read: the device is read again 6 seconds later, and that reading is reported on behalf of the
  call. SwitchBot's "something went wrong" (190) is tried once more.
- **Marks a device unavailable** when SwitchBot says it (161) or its hub (171) is offline.

## Settings

```toml
[switchbot]
# In the SwitchBot app: Profile → Preferences → About, tap App Version ten times, then
# Developer Options. Both are secrets: kept in secrets.toml and never shown again.
token = "…"
secret = "…"
# Irori's share of the account's 10 000 calls a day (100–10 000). Default 5 000.
daily_calls = 5000
```

Give both the token and the secret key, or neither.

## Sharing the account

SwitchBot allows 10 000 calls a day for the whole account, and Home Assistant or any other app
using the same token spends from them too. `daily_calls` is Irori's share; 10% of it is kept back
for commands.

SwitchBot answers "Unauthorized" both for a wrong token and for a day whose calls are used up. A
token that worked earlier the same UTC day and is then refused is taken as the day being used up:
the extension says so, stops calling, and starts again after midnight UTC. Only a token that
hasn't worked today is reported as wrong. A wrong token is tried again after an hour.

## Not yet

- Bluetooth, for reading nearby devices without spending calls.
- SwitchBot's webhook, for changes as they happen.
- Infrared remotes, as buttons.
