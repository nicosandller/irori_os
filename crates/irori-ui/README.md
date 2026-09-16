# irori-ui

The web UI: a [Leptos](https://leptos.dev) app compiled to WebAssembly and embedded in the
`irori` binary, so a single file still serves a working page. Today it's one page — Devices —
listing everything in the home and switching what can be switched.

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
cargo run -- serve                       # the core, with the demo devices, on 8480
cd crates/irori-ui && trunk serve --open # the page on 8080, API proxied to 8480
```

## What it does

- Lists every entity, grouped by the device it belongs to, sorted by name.
- Switches lights and switches. The switch shows what the device **reports**, not what was
  clicked: it moves when the change comes back. A click asks for a state ("on"), not a flip, so
  clicking a row that's out of date can't undo the click before it.
- Shows sensor readings with their units, and binary sensors in the words of their device class
  (a motion sensor says Motion or Still, a door says Open or Closed).
- Marks unreachable entities offline, keeping their last known value, and refuses to switch them.
- Says why a command was refused, under the row it belongs to.
- Filters by entity name, entity id, or device name.
- Lists the extensions behind it all, with their status and any reports they lost.

**Not yet:** brightness and color for lights (the command API takes them; the page doesn't send
them), areas and floors, any page other than Devices, and history. The page **polls**
`/api/dev/home` every 2 seconds; the WebSocket API (M1.5) will push changes instead, and
`src/api.rs` is what goes away then.

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

**Budget (ROADMAP §4.3):** under 500 KB brotli for the barebones UI. CI checks it on every pull
request. The Devices page is around 160 KB.
