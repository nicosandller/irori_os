# irori-protocol-homeassistant

Try Irori on the home your Home Assistant already runs. This extension brings Home Assistant's
devices, entities, rooms and floors into Irori while Home Assistant keeps running them: it reads
Home Assistant over its WebSocket API, follows its changes, and passes on the commands you give
in Irori. Nothing about your Home Assistant changes.

## What it does

- **Signs in** to Home Assistant's WebSocket API (`/api/websocket`) with a long-lived access
  token of an administrator, at its local `http://` address (`ws://`). Not `https://`: a
  certificate is only trusted when a public authority issued it for the name used, and none is
  ever issued for a local name or a private address. The token never leaves the local network.
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
- **Lists what it doesn't bring over** on the extension's page: an entity Irori couldn't take
  (a water heater whose modes Irori's don't include), with the reason, and kinds Irori doesn't
  have (Home Assistant's voice pipeline: `tts`, `stt`, `conversation`…).
- **Shows a camera's picture, or an image's,** when a page asks for it, from Home Assistant's
  camera or image proxy (`/api/camera_proxy/<entity_id>`, `/api/image_proxy/<entity_id>`, over
  plain `http://`, signed in with the same token). Their own access token, in their attributes,
  is never passed on.
- **Streams a camera live** when Home Assistant streams it: `camera/stream` gives an HLS
  playlist of fragmented MP4, the container Irori's live view plays, so its init segment and then
  each new segment are passed on as they are, starting at the newest, until nobody watches.
- **Passes commands on** as `call_service` to the entity, and answers once Home Assistant has
  carried it out, within 8 seconds. The change it causes is reported as caused by that command.
- **Reconnects on its own** when Home Assistant restarts or the network drops: after a second,
  then doubling to a minute. Meanwhile everything it brought over is unavailable, and commands
  are answered "not connected".

## What comes over

By default, entities that:

- are on a Home Assistant device, are one of Home Assistant's helpers (`input_boolean`,
  `input_number`, `input_select`, `input_text`, `input_button`, `input_datetime`, `timer`,
  `schedule`), or are a weather service, a calendar or a to-do list, which often have no device;
- are of a kind Irori has: `light`, `switch`, `sensor`, `binary_sensor`, `button`, `number`,
  `select`, `cover`, `lock`, `fan`, `event`, `valve`, `siren`, `climate`, `media_player`, `camera`,
  `device_tracker`, `update`, `alarm_control_panel`, `vacuum`, `water_heater`, `humidifier`,
  `lawn_mower`, `scene`, `text`, `date`, `time`, `datetime`, `image`, `notify`, `remote`,
  `weather`, `calendar`, `todo`, `timer` and `schedule`. A helper comes over as the kind it is: an
  `input_boolean` is a switch, an `input_datetime` a date, a time or both. The weather comes in
  Irori's units (°C, hPa, km/h, km) whatever Home Assistant shows it in;
- aren't disabled or hidden;
- aren't part of Home Assistant itself: Core, the Supervisor, the operating system, its apps
  (add-ons), its backups, and the cards, integrations and themes HACS installed, which Home
  Assistant lists as devices. Naming their integration (`hassio`, `backup`, `hacs`) in
  `include_integrations` brings them over.

A device's settings and diagnostics (`entity_category`) come over too, marked as such, so a
device has everything it has on Home Assistant's device page.

The settings widen or narrow that:

- `include_integrations` / `include_areas` also bring over everything else of those
  integrations or in those areas: entities on no device and hidden ones. Disabled entities never
  come over.
- `exclude_integrations` / `exclude_areas` leave those out, whatever else says.

Areas are matched by name, integrations by their domain (`hue`, `zha`, `mqtt`…), both ignoring
case.

## Going easy on the core

Every extension shares the core's event broadcast, its history queue and the home's write lock.
A big Home Assistant must not crowd out a home's own devices, so this extension holds itself to:

- **At most 2,000 entities.** Past that the same 2,000 are always taken (in the order of their
  registry ids), and the extension's page says how many more qualified and how to narrow it.
- **A sensor reports at most once a second**, the newest value winning.
- **About 150 reports a second in all**, in batches of 15 every 100 ms, the longest waiting first.
  What answers a command goes first.
- **Nothing that didn't change.** A change to an attribute that doesn't change the value Irori
  holds isn't reported. Attributes aren't sent: Irori's typed value has what it uses.
- **One message for availability**, however many entities it covers.

`dev/ha-load` checks this holds against a fake Home Assistant of 2,000 and 5,000 entities.

## Never writes to Home Assistant

What this extension can send is a closed list, checked again before anything leaves: signing in,
`get_config`, the four registries' `list`, `get_states`, `subscribe_entities`,
`subscribe_events`, `unsubscribe_events`, `camera/stream` (which only starts a stream to read),
and `call_service`. `call_service` is only sent in
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

- Weather forecasts, a calendar's other events and adding to it, and a to-do list's items
  (reading, ticking off, removing): each kind has its present state and its main action for now.
- Home Assistant's voice pipeline (`tts`, `stt`, `conversation`, `ai_task`, `wake_word`,
  `assist_satellite`): it is Home Assistant's own, not something in the home, so Irori has no kind
  for it. Listed as unmodeled.
- A water heater whose modes aren't ones Irori's water heaters have is listed, with its modes.
- Checking for an update again (`homeassistant.update_entity` isn't the update's own service).
- History from Home Assistant's recorder (#167), a welcome step for trying Irori this way (#166),
  showing which side runs what (#169), handing a device over from Home Assistant to a native
  extension (#168), and packaging for Home Assistant OS (#170, #171).
- A device removed in Home Assistant while this extension isn't running stays in Irori until it's
  removed there.
