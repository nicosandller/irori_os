# irori-protocol-homeassistant

Try Irori on the home your Home Assistant already runs. This extension brings Home Assistant's
devices, entities, rooms and floors into Irori while Home Assistant keeps running them: it reads
Home Assistant over its WebSocket API and follows its changes. It passes on the commands you give in
Irori **by default**; turn off "Let Irori control devices" for a read-only try. Nothing about your
Home Assistant's setup changes.

> **Use an `https://` address if you can.** Over `http://`, your Home Assistant administrator's
> access token travels unencrypted on your local network, where another device on it could read
> it. The extension's settings page says so next to the address.

## What it does

- **Signs in** to Home Assistant's WebSocket API (`/api/websocket`) with a long-lived access
  token of an administrator, at its `https://` (`wss://`) or `http://` (`ws://`) address. Over
  `https://` the certificate is checked before the token is sent; over `http://` the token
  travels unencrypted on the local network.
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
- **Removes what no longer comes over**: removed or disabled in Home Assistant, or left out in
  this extension's settings, also while Irori wasn't running. It keeps the ids it brought over in
  its own store for that. An entity Home Assistant lists but hasn't loaded yet stays.
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

The network map draws them too: Home Assistant between Irori and its devices, each device hanging
off the one Home Assistant says it's reached through (a Hue bridge, Zigbee2MQTT), over Zigbee,
Bluetooth or the home's network when Home Assistant knows its address there. Online services it
lists as devices (the weather, the sun) are drawn over the internet.

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

- **At most 2,000 entities.** Past that, those already in the home are kept and the rest wait
  (new ones in the order of their registry ids), and the extension's page says how many more
  qualified and how to narrow it.
- **A sensor reports at most once a second**, the newest value winning.
- **About 150 reports a second in all**, in batches of 15 every 100 ms, the longest waiting first.
  What answers a command goes first.
- **Nothing that didn't change.** A change to an attribute that doesn't change the value Irori
  holds isn't reported. Attributes aren't sent: Irori's typed value has what it uses.
- **One message for availability**, however many entities it covers.

`dev/ha-load` checks this holds against a fake Home Assistant of 2,000 and 5,000 entities.

## Never changes Home Assistant's setup

What this extension can send is a closed list, checked again before anything leaves: signing in,
`get_config`, the four registries' `list`, `get_states`, `subscribe_entities`,
`subscribe_events`, `unsubscribe_events`, `camera/stream` (which only starts a stream to read),
and `call_service`. It never touches Home Assistant's registries, areas or automations, and it
can't unpair anything. A test fails if anything else can be sent.

`call_service` does act on your home, and it is **on by default**: a command someone gives in
Irori goes to that entity, with a service of its own kind. So Irori can switch, unlock, disarm and
send commands, and Automations flows can do the same. For a read-only try, turn off "Let Irori
control devices": every command is then answered "read-only" and no service call is sent.

## Settings

```toml
[homeassistant]
# Home Assistant's address, as you open it in a browser on this network. With http://, the
# administrator's token travels unencrypted on your local network.
url = "https://homeassistant.local:8123"
# In Home Assistant, as an administrator: your profile → Security → Long-lived access tokens →
# Create token. A secret: kept in secrets.toml and never shown again.
token = "…"
# Pass Irori's commands on to Home Assistant: on by default. Off: a read-only try.
commands = true
# Optional, for a certificate no public authority issued: its SHA-256 fingerprint, or your
# certificate or CA certificate as PEM. `none` goes back to public authorities only.
certificate = ""
# Optional: this https:// address is a public name your network's DNS points at Home Assistant.
resolves_locally = false
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

A public name is taken only over `https://` with `resolves_locally` on (split DNS), and Irori then
connects only if the name resolves to a local address.

Over `https://`, a certificate is trusted if a public authority issued it for that name. For Home
Assistant's self-signed certificate, enter its SHA-256 fingerprint, or the certificate itself, in
`certificate`: exactly that certificate is then trusted, whatever name it carries. A CA certificate
there also trusts what it issued for the name in the address. A certificate that doesn't pass is
never sent the token, and the extension's page says why.

The token belongs to the address it was entered for. Saving a different address clears it, so it
is never sent to the new host, and the settings page asks for it again; the extension waits in
"needs setup" until it's given. Saving a new token along with the new address keeps the new one.

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
