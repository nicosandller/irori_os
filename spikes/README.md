# M0.8 spike: Leptos vs Dioxus

Irori's UI runs in the browser and shares the core's Rust types (ROADMAP tenet 13), so it's
compiled to WebAssembly. The open question was which framework to build it with (ROADMAP open
question 3). This is the experiment that answered it.

**Result: Leptos** (ROADMAP D27). Both work; Leptos downloads about half as much.

## What was built

The same small Devices page, twice: [`ui-leptos`](ui-leptos) and [`ui-dioxus`](ui-dioxus). Each
one:

- fetches `/api/dev/states` from a running Irori and lists every entity;
- re-fetches every 5 seconds, so live changes appear (the demo's temperature moves);
- filters the list as you type;
- deserializes with `irori-types`, the core's own types, compiled to wasm.

87 lines each, not counting the shared page shell. Both were checked in a browser against a
running `irori serve` with the demo extension: same list, same live updates, same filtering.

## Measured

Release builds through `trunk` (which runs `wasm-bindgen`), then `wasm-opt -Oz`, then
`brotli -q 11`. The compressed sizes are what a browser downloads.

| | Leptos 0.8.20 | Dioxus 0.7.10 |
|---|---|---|
| **Download (wasm + JS, brotli)** | **119 KB** | **207 KB** |
| wasm, brotli | 113 KB | 197 KB |
| wasm, after `wasm-opt -Oz` | 355 KB | 611 KB |
| wasm, raw | 384 KB | 660 KB |
| Release build, from clean deps | 61 s | 55 s |
| Crates in the dependency tree | 203 | 363 |

Budget (ROADMAP §4.3): under 500 KB brotli for the barebones UI. Both pass; Leptos leaves far
more room for the real UI, which will be bigger than this page.

## What the numbers don't say

- **Both were pleasant to write**, and the two files read almost the same. Neither had
  a learning cliff for a page this simple.
- **Dioxus needed a mount point** (`<div id="main">`) or it logs an error and falls back to the
  body. A five-second fix, but the kind of thing Leptos didn't ask for.
- **Dioxus aims wider** (web, desktop, mobile from one codebase), which explains some of the
  size. Irori's UI is a web page served by the binary, so that reach isn't worth paying for.
- **Leptos is closer to the server-rendering story** we may want later for first paint on a Pi.
- Versions move fast in both. Re-run this before committing to a major upgrade: `trunk build
  --release` in each folder, then compare `dist/*.wasm` after `wasm-opt` and `brotli`.

## Running them

```sh
cargo install trunk --locked        # once
cargo run -- serve                  # Irori, with the demo devices, on 8480
cd spikes/ui-leptos                 # or ui-dioxus
trunk serve --release --port 8081 --proxy-backend=http://127.0.0.1:8480/api/
```

Then open http://127.0.0.1:8081.

## What happens to this folder

It stays until the real UI lands in `crates/irori-ui`, as evidence for the decision and a place
to re-measure. CI doesn't build it, so it can go stale; that's the trade for not spending a
couple of minutes of CI on every pull request for code that isn't shipped. The spikes are excluded from the workspace: they have their own dependency trees
and are built with `trunk`, not by `cargo build` at the root.
