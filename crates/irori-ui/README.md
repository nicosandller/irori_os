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
| **Start** (`/`) | What IroriOS is: the wordmark the terminal prints when `irori serve` runs, and how many devices, entities and extensions it is looking after. |
| **Devices** (`/devices`) | Two ways to read the same home, remembered per browser: **Entities** groups everything by the device it came from, with switches; **Devices** is a row per device — what brought it in, make, model, battery, how many entities, and which area it's in. **Add device** explains where devices come from — every installed extension, what it's for, and what it can provide — because nothing is typed in by hand yet. |
| **A device** (`/devices/<id>`) | One device: which extension brought it in, what that extension knows it as (the MAC address, for ESPHome), make, model, firmware, hardware, battery, what it's reached through, and every entity it provides with its controls. Its name, description and area are yours to decide. |
| **Extensions** (`/extensions`) | Official extensions from this repo (protocols, Demo, Helpers). Install copies a package into the instance and starts it; uninstall deletes the package and the devices it brought in. |
| **Settings** (`/settings`) | The instance itself (version, uptime, database, features), the home's arrangement (**Areas** and **Floors**, the same places the Rooms page used to manage), **Users** (none yet — there's nothing to sign in with), **Logs** (what Irori has said since it started, and each extension's own output tagged with the extension, behind **Show log**), and **System**: the machine running the instance — host, operating system, kernel, CPU, memory, and the disk its data sits on, asked again on demand rather than kept. |

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
- Makes the areas and floors of the home in **Settings**: name them, put them on floors, and a
  device's own page is where it's placed.
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
  **View log** on an extension's card for that extension's own output, and **Show log** in the
  **Logs** section of Settings for the core's own log, which carries each extension's lines too,
  tagged with the extension they came from.

**Not yet:** users and signing in, automations, history beyond a device's last 24 hours, and
installing the firmware update whose version the device page shows (ROADMAP M1.8, D30). The
floorplan is mouse-driven and has no furniture and no stairs between floors. Its corners are solid wherever
two walls meet at any angle, and where three or more do at right angles; a junction of three
walls one of which runs at an odd angle can still nick the outside of the corner. The page
**polls** `/api/dev/home` every 2 seconds; the WebSocket API (M1.5) will push changes instead,
and `src/api.rs` is what goes away then. The binary also serves these files **uncompressed**
(see the budget below).

## Why Leptos (ROADMAP D27)

The M0.8 spike built this same page twice, once in Leptos and once in Dioxus, both fetching
`/api/dev/states` and deserializing with `irori-types`. Measured after `trunk build --release`,
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

**Budget (ROADMAP §4.3):** under 500 KB brotli for the barebones UI. CI checks it on every pull
request; the pages together compress to about 380 KB.

That is the budget's unit, not yet what goes over the wire. `irori serve` hands these files out
**as they are**, so a browser opening the page today downloads roughly **1.3 MB** — the wasm is
most of it. Serving precompressed assets with `Accept-Encoding` negotiation is part of the plan
(ROADMAP §2.2) and hasn't been done; until it is, read the 380 KB as "this fits, with room", not
as the transfer. On a LAN the difference is a fraction of a second; over a slow link it isn't.
