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
| **Start** (`/`) | What IroriOS is: the wordmark the terminal prints when `irori serve` runs, and how many devices, entities and extensions it is looking after. |
| **Devices** (`/devices`) | Two ways to read the same home, remembered per browser: **Entities** groups everything by the device it came from, with switches; **Devices** is a row per device — what brought it in, make, model, battery, how many entities, and which area it's in. **Add device** explains where devices come from — every installed integration, what it's for, and what it can provide — because nothing is typed in by hand yet. |
| **A device** (`/devices/<id>`) | One device: which integration brought it in, what that integration knows it as (the MAC address, for ESPHome), make, model, firmware, hardware, battery, what it's reached through, and every entity it provides with its controls. Its name, description and area are yours to decide. |
| **Extensions** (`/extensions`) | Official extensions from this repo (protocols, Demo, Helpers). Install copies a package into the instance and starts it; uninstall deletes the package and the devices it brought in. |
| **Settings** (`/settings`) | The instance itself (version, uptime, database, features), the home's arrangement (**Areas** and **Floors**, the same places the Rooms page used to manage), **Users** (none yet — there's nothing to sign in with), and **System**: the machine running the instance — host, operating system, kernel, CPU, memory, and the disk its data sits on, asked again on demand rather than kept. |

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
- Lists the extensions behind it all, with their status and any reports they lost.

**Not yet:** users and signing in, automations, history beyond a device's last 24 hours, and
installing the firmware update whose version the device page shows (ROADMAP M1.8, D30). The page
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
request; the pages together compress to about 230 KB.

That is the budget's unit, not yet what goes over the wire. `irori serve` hands these files out
**as they are**, so a browser opening the page today downloads roughly **770 KB** — the wasm is
most of it. Serving precompressed assets with `Accept-Encoding` negotiation is part of the plan
(ROADMAP §2.2) and hasn't been done; until it is, read the 230 KB as "this fits, with room", not
as the transfer. On a LAN the difference is a fraction of a second; over a slow link it isn't.
