# irori-protocol-homeassistant

Try Irori on the home your Home Assistant already runs. This extension brings Home Assistant's
devices, entities, rooms and floors into Irori while Home Assistant keeps running them: it reads
Home Assistant over its WebSocket API, follows its changes, and passes on the commands you give
in Irori. Nothing about your Home Assistant changes.

## What it does

- **Signs in** to Home Assistant's WebSocket API (`/api/websocket`) with a long-lived access
  token of an administrator. `https://` addresses connect over TLS (`wss://`), and the
  certificate must be one a public authority issued for that very name: a self-signed one, or a
  Let's Encrypt one for a public name reached by a local address, is refused. Use `http://` and
  the local address then; the token never leaves the local network either way.
- **Reads** the floor, area, device and entity registries and every entity's state, and follows
  them: entities' changes through `subscribe_entities` (only the entities it brought over), and
  the four registries' `*_registry_updated` events. A registry change is read again two seconds
  later, once a burst of them has settled.
- **Brings over devices** keyed by Home Assistant's device id, with their name, maker, model,
  versions and serial number. A device's area becomes its suggested room. Its MAC, Bluetooth and
  Zigbee addresses and its serial number are its `connections`, so a device another extension
  also reaches (an ESPHome board, a Zigbee bulb) is recognised and not listed twice.
- **Brings over entities** keyed by their entity registry entry's id, which stays the same when
  an entity is renamed. Each keeps its name and its entity id after the `.`.
- **Proposes Home Assistant's floors and rooms**, by name, for the owner to bring over in one
  step from the extension's page. Devices land in their rooms once those exist.
- **Lists what it doesn't bring over** on the extension's page: entities of kinds Irori has but
  this extension doesn't carry yet (media players, vacuums, updates…), with that reason, and
  kinds Irori doesn't have (weather, calendars…).
- **Passes commands on** as `call_service` to the entity, and answers once Home Assistant has
  carried it out, within 8 seconds. The change it causes is reported as caused by that command.
- **Reconnects on its own** when Home Assistant restarts or the network drops: after a second,
  then doubling to a minute. Meanwhile everything it brought over is unavailable, and commands
  are answered "not connected".

## What comes over

By default, entities that:

- are on a Home Assistant device;
- are of a kind this extension carries: `light`, `switch`, `sensor`, `binary_sensor`, `button`,
  `number`, `select`, `cover`, `lock`, `fan`, `event`, `valve`, `siren` and `climate`;
- aren't disabled or hidden, and aren't one of a device's settings or diagnostics
  (`entity_category`).

The settings widen or narrow that:

- `include_integrations` / `include_areas` also bring over everything else of those
  integrations or in those areas: entities on no device, hidden ones, settings and diagnostics.
  Disabled entities never come over.
- `exclude_integrations` / `exclude_areas` leave those out, whatever else says.

Areas are matched by name, integrations by their domain (`hue`, `zha`, `mqtt`…), both ignoring
case.

## Going easy on the core

Every extension shares the core's event broadcast, its history queue and the home's write lock.
A big Home Assistant must not crowd out a home's own devices, so this extension holds itself to:

- **At most 2,000 entities.** Past that the same 2,000 are always taken (in the order of their
  registry ids), and the extension's page says how many more qualified and how to narrow it.
- **A sensor reports at most once a second**, the newest value winning.
- **About 200 reports a second in all**, in batches of 20 every 100 ms, the longest waiting first.
  What answers a command goes first.
- **Nothing that didn't change.** A change to an attribute that doesn't change the value Irori
  holds isn't reported. Attributes aren't sent: Irori's typed value has what it uses.
- **One message for availability**, however many entities it covers.

`dev/ha-load` checks this holds against a fake Home Assistant of 2,000 and 5,000 entities.

## Never writes to Home Assistant

What this extension can send is a closed list, checked again before anything leaves: signing in,
`get_config`, the four registries' `list`, `get_states`, `subscribe_entities`,
`subscribe_events`, `unsubscribe_events`, and `call_service`. `call_service` is only sent in
answer to a command someone gave in Irori, to that entity, with a service of its own kind. It
never touches Home Assistant's registries, areas or automations, and it can't unpair anything.
A test fails if anything else can be sent.

## Settings

```toml
[homeassistant]
# Home Assistant's local address, as you open it in a browser on this network.
url = "http://homeassistant.local:8123"
# In Home Assistant, as an administrator: your profile → Security → Long-lived access tokens →
# Create token. A secret: kept in secrets.toml and never shown again.
token = "…"
# Optional: widen or narrow what comes over (see above).
include_integrations = []
exclude_integrations = []
include_areas = []
exclude_areas = []
```

Until `url` and `token` are given, the extension waits in "needs setup". The address has to be on
the local network: a private, loopback or link-local IP address, `localhost`, or a name under
`.local`, `.home.arpa`, `.internal`, `.lan` or `.home`. A Home Assistant Cloud (Nabu Casa)
address is refused.

A token whose user isn't an administrator can sign in but can't read the registries. The
extension then says so on its page and asks for another token there.

## Not yet

- Media players, vacuums, humidifiers, water heaters, updates, alarm panels, lawn mowers, texts,
  dates and times, device trackers, cameras and scenes: Irori has these kinds, but this extension
  doesn't carry them yet. They're listed as unmodeled.
- History from Home Assistant's recorder (#167), a welcome step for trying Irori this way (#166),
  showing which side runs what (#169), handing a device over from Home Assistant to a native
  extension (#168), and packaging for Home Assistant OS (#170, #171).
- A device removed in Home Assistant while this extension isn't running stays in Irori until it's
  removed there.
