# irori-ui

The web UI: a [Leptos](https://leptos.dev) app compiled to WebAssembly and embedded in the
`irori` binary, so a single file still serves a working page. Today it is a few pages — a start
screen, the devices in the home, the extensions behind them, and the settings — listing
everything in the home and switching what can be switched.

It shares `irori-types` with the core, so the browser reads the same typed state the server
writes (ROADMAP tenet 13), and an entity shape can't drift between the two.

## Building it

```sh
cargo xtask ui        # trunk build → crates/irori/ui/, which the binary embeds
cargo build -p irori  # pick it up
```

This crate is **not a workspace member**: it builds for `wasm32-unknown-unknown` with `trunk`,
and keeping it out means a plain `cargo build` needs no wasm toolchain. A binary built without
running `cargo xtask ui` serves the placeholder page in `crates/irori/assets/` instead — CI
builds the UI and the release binaries embed it.

Needs `trunk` once:

```sh
cargo install trunk --locked
rustup target add wasm32-unknown-unknown
```

## Working on it

`trunk serve` rebuilds on every save and proxies the API to a core running the usual way, so
there's no binary to rebuild between edits:

```sh
cargo run -- serve                       # the core on 8480; Install Demo from /extensions
cd crates/irori-ui && trunk serve --open # the page on 8080, API proxied to 8480
```

## The pages

| | |
|---|---|
| **Floorplan** (`/floorplan`) | The home as a drawing, a floor at a time, with the devices live on it: a lamp that's on glows, and clicking one switches it. A picker on the right says which floor, and the floor below shows faintly while you draw so an upstairs can be lined up with what holds it up. **Edit** (top right) puts a toolbar over the same canvas — walls, doors, windows, rooms, devices — and becomes **Save** and **Cancel**. Whatever is picked up gets a panel for the numbers that can't be dragged: a wall's thickness, an opening's width. Points land on a 10 cm grid, or on a step of your own; the grid drawn under the plan **is** that step, with heavier lines every metre, so what you see is where a point can go. Rooms are traced with corners that prefer the walls to the grid, and a device drawn standing in a room is put in that room in Settings when the plan is saved. Undo and redo (⌘Z, ⇧⌘Z) go back a move at a time. The canvas is the whole view; scroll to zoom, drag the empty plan to move around. |
| **Welcome** | Until the home has an owner: who you are, with a password, and what time zone the home is in (putting it on the map is optional). After that the page opens on a sign-in, a name and a password both typed; it never lists who lives here. |
| **Start** (`/`) | What IroriOS is: the wordmark the terminal prints when `irori serve` runs, and how many devices, entities and extensions it is looking after. |
| **Devices** (`/devices`) | Two ways to read the same home, remembered per browser, both as compact rows in groups that fold: **Devices** is a row per device — make, model, battery, how many entities, and which area it's in — grouped by protocol, area or make; **Entities** is a row per entity — its icon (what kind of thing it is), its control, and its last 24 hours a click away: a number as a line, a state (on and off, a player's playing and paused) as a strip of how long each lasted, either as a table too — grouped by device, area, kind or protocol. **Add device** explains where devices come from — every installed extension, what it's for, and what it can provide — because nothing is typed in by hand yet. |
| **A device** (`/devices/<id>`) | One device: which extension brought it in, what that extension knows it as (the MAC address, for ESPHome), make, model, firmware, hardware, what it's reached through, and every entity it provides with its controls. Its name, description and area are yours to decide, and so is each entity's name: each is changed where it stands, with a pencil beside it. |
| **Extensions** (`/extensions`) | Official extensions from this repo (protocols, Demo, Helpers). Install copies a package into the instance and starts it; uninstall deletes the package and the devices it brought in. Every card is the same size, with the rest of what an extension says behind **Show more**; an installed one has its settings and its log beside the button, and the log button carries a mark when the log has an error in it. |
| **Settings** (`/settings`) | One table: each row is a section that says how it stands beside its name and opens in place. **System** (the instance — version, uptime, database, features — and the machine under it, with a meter each for memory, disk, processor and temperature that opens to its last day and to what is using it: the heaviest processes, and what Irori's data directory is made of), **Appearance** (whether the page animates), **Assistant** (which model answers), **Location and time zone** (where the home is, on a map with an address search and a pin to drag, and the time zone that follows from it, which is what a time of day and the sun mean in an automation), **Floors and areas** (the home's arrangement, made and rearranged by dragging), **Users** (who is allowed in: an owner runs the home, a user sees everything and controls devices; everybody has a password), **Programs** (access tokens for programs that aren't this page: what each may do, created by an owner and shown once) and **Logs** (what Irori has said since it started, with each extension's own output tagged with the extension). |

Routing is client-side (`leptos_router`), so the binary serves the app for any path that isn't a
file, and the app decides what to show.

## What it does

- Lists every entity, grouped by the device it belongs to, sorted by name.
- Switches lights and switches. The switch shows what the device **reports**, not what was
  clicked: it moves when the change comes back. A click asks for a state ("on"), not a flip, so
  clicking a row that's out of date can't undo the click before it.
- Shows sensor readings with their units, and binary sensors in the words of their device class
  (a motion sensor says Motion or Still, a door says Open or Closed).
- Marks unreachable entities offline, keeping their last known value, and refuses to switch them.
- Says why a command was refused, under the row it belongs to.
- Filters by entity name, entity id, device name, area or make.
- Makes the areas and floors of the home in **Settings**: name them where they'll appear, drag
  an area onto another floor and a device into an area. A device's own page places it too.
- Draws the home in **Floorplan**, a floor at a time: walls in runs that snap to the corners
  already there (right-click or Escape ends a run), doors and windows cut into those walls, the
  rooms of the home traced out as shapes, and devices put where they are. Corners weld, so
  dragging one keeps the room closed, and walls run on into each other far enough that a corner
  is solid rather than notched. Floors and rooms are the ones Settings already knows about — the
  plan gives them a shape rather than defining them, though a floor can be added from here as
  well as from Settings. Everything is in whole centimetres and lands in
  `config/floorplan.toml` when Save is pressed — the editor works on a copy until then, so
  Cancel is simply never sending it, and undo goes back through that copy a move at a time.
  Saving also writes `devices.toml` for any device drawn standing in a room, and says how many
  it moved; a device deliberately in no room is left alone.
- Lists the extensions behind it all, with their status and any reports they lost.
- Shows what Irori and its extensions have said, in one window that keeps itself up to date:
  the log button on an extension's card for that extension's own output, in a window, and the
  **Logs** row of Settings for the core's own log, in place, which carries each extension's
  lines too, tagged with the extension they came from.

**Not yet:** permissions finer than owner and user, history beyond a device's last 24 hours, and
installing the firmware update whose version the device page shows (ROADMAP M1.8, D30). The
floorplan is mouse-driven and has no furniture and no stairs between floors. Its corners are solid wherever
two walls meet at any angle, and where three or more do at right angles; a junction of three
walls one of which runs at an odd angle can still nick the outside of the corner. The page
hears the home on `/api/ws` and asks `/api/health` about once every 30 seconds. Log windows
keep their own two-second refresh. The binary also serves these files **uncompressed**
(see the budget below).

## Motion

The guidelines — principles, tokens, the rules, and what moves where — are in
[docs/motion_design.md](../../docs/motion_design.md). In short: small and quick, mostly CSS. The durations and easings are tokens on `:root` in
`index.html` (`--dur-fast` 120 ms, `--dur-base` 180 ms, `--ease-out`, and `--ease-spring` for
the few things a finger pushes). Use them rather than new numbers, so turning motion off turns
off everything.

- **Buttons** ease into their hover colours and give a little under a press.
- **The toggle's knob** springs across, stretches while held, and breathes while a command is
  waiting on the device (`.pending`). Offline stays plain and dim: nothing is coming.
- **Sliders** are drawn by the page: brightness fills in ember up to the thumb, colour
  temperature shows the colours themselves, and the thumb swells under the pointer. The label
  follows the thumb while it's dragged, and letting go still sends one command.
- **The sidebar** marks the page you're on with a highlight that glides from one entry to the
  next, rather than the choice jumping; an icon leans toward its page under the pointer, and the
  Settings cog turns a notch. The same glide (`src/glide.rs`) marks the tool in hand and the floor
  shown on the Floorplan. On a phone, where these lists are rows, the chosen item draws its own
  highlight instead.
- **List to device:** opening a device from the list slides the page aside and the device's
  name travels from its row up into the heading (the View Transition API, `src/transition.rs`);
  going back reverses it. Every other page change moves the way the sidebar goes, and a Start
  tile grows into the page it opens.
- **Live numbers** count to their new value as readings arrive (`src/count.rs`), and a
  device's chart grows with them. **Toggles swipe**, a history **pulls down**
  (`src/gesture.rs`), and on the **Floorplan** lit lights pool light in their rooms, motion
  sensors ripple, and an aimed radar's field fades in with an arc gliding to where its target is
  (`src/floorplan/ambience.rs`); changing floor, the plan sinks or rises.
- **A device's history:** on a device's page a sensor's reading is itself the way into its last
  24 hours (a switch or light has a chevron beside it); the drawer rolls down to its content. A
  number's day is a chart (`src/chart.rs`) that draws itself in, with a crosshair and the reading
  at any moment under the pointer or the arrow keys, and the table a click away.
- **The Floorplan:** the tools, the device and room pickers and the inspector slide in when they
  appear; a device marker leans in under the pointer, warms to its tone, and grows into its
  reading and back out of it; a motion sensor's marker pulses while it senses someone and a
  button's flashes once when it's pressed (a doorbell's bell swings too); a door or window with
  a contact sensor swings open and shut on its hinge; what's picked, and the line being drawn, march like any drawing tool's
  selection. Its sliders (a wall's thickness, an opening's width, the snap step) are drawn like
  the lights'.
- **Ambient:** the Live dot breathes while the core answers and goes still when it doesn't, the
  ember on Start flickers, the Start tiles come in one after another, and a banner drops in
  when something goes wrong.

**Nothing animates as it appears in the device list.** The list is rebuilt whenever a reading
changes, so an entrance animation there would replay every couple of seconds. Motion goes on
state changes (a transition on `aria-pressed`, say) and on things that appear once per visit.
The same goes for the Floorplan's markers and drawing, which redraw on every reading and every
pan: they transition, and nothing on them plays as it appears.

**Turning it off:** Settings → Appearance → Motion, remembered per browser (`irori.motion`), sets
`data-motion="off"` on `.shell`. The system's *reduce motion* setting does the same whatever
the switch says.

## Why Leptos (ROADMAP D27)

The M0.8 spike built this same page twice, once in Leptos and once in Dioxus, both fetching
`/api/states` and deserializing with `irori-types`. Measured after `trunk build --release`,
`wasm-opt -Oz` and `brotli -q 11`:

| | Leptos 0.8.20 | Dioxus 0.7.10 |
|---|---|---|
| **Download (wasm + JS, brotli)** | **119 KB** | **207 KB** |
| wasm, after `wasm-opt -Oz` | 355 KB | 611 KB |
| Release build, from clean deps | 61 s | 55 s |
| Crates in the dependency tree | 203 | 363 |

Both were pleasant to write and both fit the budget, but Dioxus's extra size buys desktop and
mobile reach that a page served by the core doesn't need. The spikes are in the history of the
`m0.8-ui-framework` branch; to re-measure, build both with `trunk build --release` and compare
`dist/*.wasm` after `wasm-opt` and `brotli`.

## The size budget, and what a browser really downloads

**Budget:** under 5 MB brotli. CI checks it on every pull request; the pages together compress
to about 520 KB. It was 500 KB until 2026-09-28, when the motion work (charts, page
transitions) reached it and it was raised to leave room for the UI to be richer rather than
smaller.

That is the budget's unit, not yet what goes over the wire. `irori serve` hands these files out
**as they are**, so a browser opening the page today downloads roughly **1.3 MB** — the wasm is
most of it. Serving precompressed assets with `Accept-Encoding` negotiation is part of the plan
(ROADMAP §2.2) and hasn't been done; until it is, read the compressed number as the budget's
unit, not as the transfer. On a LAN the difference is a fraction of a second; over a slow link it isn't.
