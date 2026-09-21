# Irori — Roadmap

Companion document: [INSPIRATION.md](INSPIRATION.md) (vision, tenets, target user).
Last revised: 2026-09-21. Based on the original `irori-project-plan.md`, revised after review, then
reorganized from sequential phases into areas (D42) and audited line-by-line against the actual
codebase on the same date.

---

## 0. How to use this document

- Work below is grouped into **areas**, not sequential phases: an area's own milestones are usually
  sequential, but areas don't imply an order relative to each other (D42) — priorities here get
  reworked often, and the document shouldn't need restructuring every time they do. The one place
  order is stated is **§0a Working order**, a short, disposable list — reorder that, not this document.
- Every milestone still ends with a **demo** — something you can show running. If it can't be demoed, it isn't done.
- Estimates assume a solo side project (~8–12 h/week). They're gut-feel sizing to catch scope creep, not schedule commitments, and matter less now that order isn't fixed.
- **Status marks used throughout:** ✅ done · 🔶 partial (some shipped, some didn't) · ⚠️ this note is stale, or the plan underneath it changed, and the text hasn't caught up · ➕ shipped, but never had a line here · ⏳ not started.
- When a decision changes, update §2 (Decision log) first, then the affected areas.

---

## 0a. Working order

Not a phase list — a short, frequently-rewritten note on what's actually next, kept separate from the
areas below so re-prioritizing means editing this, not moving sections around.

- **Now (2026-09-21):** nothing in flight — PR #16 (Floorplan) just merged to `main`.
- **What unblocks the most, if picked next** *(a suggestion, not a commitment — reorder freely)*:
  M1.2 (MQTT — the second protocol extension, the real test of D15's "core has no protocol code"), M1.5
  (API/auth — unblocks the CLI command tree, external extensions, and lets `--allow-unauthenticated-lan`
  go away per D12), and the Floorplan-as-extension move (D43).
- **Deliberately parked, not blocked on anything:** the AI layer (all opt-in, D13), the HA backend
  adapter, multi-user — real work, just not where the value is yet.

---

## 1. Decision log

| # | Decision | Rationale / notes |
|---|---|---|
| D0 | **Core priorities, in order: robust → fast and lightweight → modular → nerd friendly** | Owner's direction. Every Phase 0–1 trade-off is judged against these. The extra features (Phase 2+) are built on the core, never shortcuts inside it. |
| D1 | **Core first**, then the extra features | Owner's call. Risk: the product idea is validated late. Mitigation: **shadow mode** dogfooding (§4.2) and publishing specs and dev logs early (§9). |
| D2 | **Automation visualizer/debugger is the first extra feature**, then AI rule authoring, then AI dashboards | Needs only Phase 1 traces and no LLM, so it's the shortest path to a showable difference. Owner had no preference; easy to reorder. |
| D3 | **AI providers are pluggable**: BYO cloud key (Anthropic, OpenAI-compatible) or a local model (Ollama) | Local-first community; hosted tier can be a provider later. |
| D4 | **AI dashboards produce generated HTML** | Owner's call. Risks: security (model-written JS) and maintainability. Mitigations are mandatory, not optional: sandboxed iframe, capability-scoped bridge SDK, versioning (§6.3). |
| D5 | **Frontend is Rust/WASM** | Shared types crate between server and UI; no TS codegen. Leptos (CSR) recommended; confirm with a Phase 0 spike vs Dioxus. |
| D6 | **v1 persona: tinkerers with MQTT gear** | Matches the MQTT-first device story; enables no-migration trials alongside HA. |
| D7 | **License: Apache-2.0** | Chosen before the repo went public, so no relicensing-with-consent problem exists. Permissive like Home Assistant: anyone can use, modify and build on it, commercially included (royalty-free by design). Monetization is outside the license: the hosted tier, remote access, backups, paid extensions and support (§9). |
| D8 | **The first-party sequential engine is a closed typed schema + small expression language, no templates.** Other engines may use other shapes. | Validation against the real home is this engine's product. The OS does not own "what a rule is." |
| D9 | **Traces are emitted by an automation engine, stored separately from state history** | The visualizer needs "condition X read Y and evaluated false", which state history can't reconstruct. The core stores traces; engines write them. |
| D10 | **The first-party sequential engine is deterministic: injected clock + state source** | Enables backtesting/replay. Other engines (LLM, flow-based) are not required to be deterministic. |
| D11 | **The MQTT extension speaks HA MQTT Discovery** | Zigbee2MQTT, Tasmota, ESPHome-over-MQTT already publish it → zero-config device onboarding. |
| D12 | **Auth exists from the first network-facing build** | Single owner + access tokens in Phase 1; multi-user in Phase 3. A LAN-exposed unauthenticated home controller is not acceptable, even in alpha. Until auth lands (M1.5), `irori serve` refuses non-loopback binds unless `--allow-unauthenticated-lan` is passed; the flag is removed with auth. |
| D13 | **AI features live in a separate crate that depends only on the public API client**, available behind an opt-in cargo feature (off in the barebones build, see D17) or as a separate process | Keeps "not part of core" enforced by the compiler; the core never pays for AI it doesn't use. |
| D14 | **HA-familiar domain/service vocabulary** (`light.turn_on`, `binary_sensor`, …) with typed state | LLMs already know it; eases a future HA backend adapter and HA importer. |
| D15 | **Protocols (MQTT, Zigbee, Matter, Z-Wave, …) are extensions behind one interface; the core has no protocol code.** MQTT is the first one, not part of the core | Replaces the original plan's "MQTT bundled in core". Building MQTT against the interface proves the interface is good enough for Zigbee/Matter/Z-Wave later. |
| D16 | **Two extension tiers, one contract:** *built-in* (Rust crates compiled in via cargo features, run in-process through the `Protocol` trait) and *external* (any language, separate process, same contract over the WS API) | Built-in = fastest, single binary, first-party only. External = crash-isolated, language-agnostic, how nerds and third parties extend Irori. External extensions work in Phase 1 (run from a local path); the install-from-registry flow comes in Phase 3. |
| D17 | **Barebones default build:** core + CLI + minimal UI (Devices, Extensions, Settings). No extensions are compiled in: official extensions are installable packages, not cargo features. **No automation engine** is compiled in; AI is **not** in the default build | "Robust at its smallest". Automations are installed as an extension, like a protocol. `--no-default-features` remains the test that matters. ⚠️ *Note added 2026-09-21: the shipped default build also has a Start screen and a Floorplan page, neither on this list. Floorplan is planned to move into an extension (D43), which would restore this list's accuracy; Start still wouldn't be covered.* |
| D18 | **Nerd friendly as a requirement:** plain-text config (`irori.toml`, `extensions/*.toml`) as the source of truth; an installed engine's own files (this engine: JSON rules) live with that engine; CLI can do everything the UI can, with `--json`; structured logs; `/metrics`; shell completions | Makes the system scriptable, diffable, and git-friendly. SQLite holds runtime data, not the config users author. |
| D19 | **Performance budgets enforced in CI** (§4.3) | Otherwise "lightning fast" drifts. Benchmarks run on every PR; budget regressions fail the build. 🔶 *Note added 2026-09-21: only the UI download-size budget is actually gated in `ci.yml` today; the other seven budgets in §4.3 (binary size, RSS, cold start, latency, throughput) have no benchmark yet.* |
| D20 | **Registry and state are separate; availability is its own field; unknown is `null`** | Refines the M0.2 draft (one `Entity` with `Unavailable`/`Unknown` as state values). Keeps state updates small, lets rules be checked against capabilities alone, and keeps the last known value through an outage. See [docs/specs/entities.md](docs/specs/entities.md) §9. |
| D21 | **Extensions are the single unit of modularity.** An extension is one installable package with a manifest that declares what it **contributes**. Contribution kinds (categories): **protocol**, **automation** (an engine that subscribes, calls services, emits traces), **dashboard**, **card**, **app**. More kinds can be added later (theme, LLM provider, exporter) without breaking existing extensions | Owner's direction: automations are not the OS. One install, update, and permission system; a separate typed runtime contract per kind. There is no separate concept of "an integration" — an extension either contributes a `protocol` or it doesn't (D44). |
| D22 | **Contribution kinds ship in phases:** protocol in Phase 1; dashboard and card in Phase 2c (they share the sandbox and `irori.js` bridge from D4); app in Phase 3. The manifest reserves the `contributes` table from day one | Keeps Phase 1 barebones while guaranteeing later kinds slot in without a manifest format break. |
| D23 | **Extension permissions are declared and approved:** API scopes, network hosts, serial/USB devices, host filesystem paths, host shell. Shown at install; any increase on update needs re-approval. `host_shell` and broad `host_fs` are marked *full access to this machine*, owner-only | A terminal or file explorer app is effectively root on the box; the model must say so plainly instead of hiding it inside a generic "plugin". |
| D24 | **A protocol contribution declares an `iot_class`** (`local_push`, `local_polling`, `cloud_push`, `cloud_polling`), shown as a badge in the UI and CLI | Protocols and vendor cloud APIs share one contract; local-first users still need to see at a glance what depends on the internet. |
| D25 | **v1: an extension contributes at most one protocol, and its `ProtocolId` equals its extension id** | Leaves the M0.2 entity model and existing code (`Device.protocol`) unchanged. Can be relaxed later with namespaced ids if one extension ever needs several protocol contributions. |
| D26 | **Home-testing path before automations.** After the extension spec (M0.6): a trimmed M1.1 (registry, state, events, extension host, demo extension), then the Leptos vs Dioxus spike (M0.8) with a first Devices page, then the **ESPHome native API extension** (moved up from Phase 3, §8.3). The remaining specs (M0.3 rules, M0.4 traces, M0.5 API, M0.7 config) resume after. MQTT/Zigbee2MQTT and native Zigbee stay undecided | Owner's direction: see real devices (ESP32 test boards) on a real Devices page early, and learn from a real home before designing automations. ESPHome's native API needs no broker and runs alongside the existing Home Assistant + Zigbee2MQTT setup without touching it. |
| D27 | **The UI is built with Leptos** (0.8), not Dioxus | M0.8 spike: the same page in both, measured. Leptos downloads 119 KB brotli against Dioxus's 207 KB, with 203 crates against 363. Both were equally pleasant to write and both fit the §4.3 budget, but the barebones UI will grow past this page, and Dioxus's extra size buys desktop and mobile reach Irori doesn't need. See [crates/irori-ui/README.md](crates/irori-ui/README.md). |
| D28 | **ESPHome's protocol comes from the `esphome-client` crate**, pinned to one API version, rather than hand-rolled | The architecture already puts protocol libraries inside extensions (`rumqttc` for MQTT, §2.2), and this one is the client half: mDNS discovery, the protobuf messages, and the Noise transport encryption will need. Hand-rolling the Noise handshake and a protobuf subset would cost weeks for no behaviour. The crate is young (0.2.1), so the risk is deliberate and bounded: it is ~2k readable lines under MIT, only `irori-protocol-esphome` depends on it, and its version pin means an ESPHome release can't change what Irori compiles against. If it is abandoned, vendoring or replacing it touches one crate. |
| D29 | **Plaintext ESPHome devices are adopted automatically, and that is a known trust limit.** *Encrypted devices, which close it, are supported (D35)* | Plain ESPHome has no device authentication, so anything on the LAN that announces `_esphomelib._tcp` is believed: a hostile host could present fake entities, or impersonate a device id. The fix is encryption, which authenticates both ends with a pre-shared key: an encrypted device is only believed if it holds its key. That works now — keys live in `secrets.toml` and a device that wants one waits until it has it — so the limit applies to devices left on plaintext, which is a choice made in their firmware. It stays stated in the README, warned about in the log at every plaintext connection, and bounded by Irori refusing to listen beyond loopback without `--allow-unauthenticated-lan`. Choosing *which* devices to adopt is possible by asking before adding them (D39). |
| D30 | **A device capability is an entity kind; a vendor's own tooling is a contributed app.** Firmware update becomes an `update` entity kind in the model (M1.8), not an ESPHome-specific screen | The protocols already model it as a capability — ESPHome, Z-Wave and Matter all report an available version and take an install command, and Home Assistant's `update` domain works the same way. Modelled once, every extension that has it gets the same UI, rules can act on it, and the CLI gets it for free. What genuinely is extension-specific — ESPHome's YAML editor, compiling and flashing, a vendor's cloud account settings — is a separate program, and the manifest already reserves the contribution kinds for that (`app`, proxied under `/apps/<id>/`, Phase 3; `dashboard`/`card` in sandboxed iframes, Phase 2c). So Irori doesn't need a special back door for extension UI: it needs the contribution kinds it already planned. |
| D31 | **A decision a person makes is attached to something that never moves.** *Revised by D36* | Keying settings on a name-derived id would mean renaming a device could detach the very setting that renamed it. First answer: key on `<protocol>/<unique_id>`. D36 went further and made the device id itself permanent, so devices are now keyed by their id; entities still key on `<protocol>/<unique_id>`, because an entity id is built to be readable in rules. |
| D32 | **Irori never creates a room from a device's suggestion** | ESPHome (and others) report an `area:` the owner wrote in the firmware, which is real information. But config that appears because something showed up on the network is config nobody remembers agreeing to, and the whole point of a plain-text config dir is that it says what a person decided. So a suggestion is kept on the device and resolved at read time: it takes effect only against a room that already exists, and it is re-checked whenever rooms change — making a room called "Kitchen" quietly collects every device that was asking for one. Nothing is written to `devices.toml` unless a person chooses a room themselves. |
| D33 | **The parts of a page a person can edit are derived separately from the parts that show live readings** | Found by testing against a real presence sensor: it reports every second, the page rebuilt on every report, and a rebuilt page threw away a half-typed name and the cursor with it — renaming the devices that most need renaming was impossible. The fix is a memo over everything that isn't a reading (`Shape` in `crates/irori-ui/src/device.rs`), so redraws follow real changes rather than the chattiest sensor in the house. Worth stating as a rule because it will apply again to every editable page, and a unit test can pin it without a browser: two homes differing only in state must produce the same shape. |
| D34 | **Settings reach an extension only by restarting it** | When an extension's table in `secrets.toml` changes, the host stops the extension (5 s grace) and starts it again with the new table, with no backoff because it didn't fail. The alternative — each extension watching for new settings and applying them while running — puts a second, rarely-exercised code path in every extension, which is where "works after a restart but not before" bugs live. The cost is a few seconds of reconnecting every device of that extension when one key is added, which for settings that change a handful of times in a device's life is the right trade. Invalid settings leave the extension failed until they change, rather than retrying something that can't work, and an extension that can tell one bad value from the rest should keep the rest working: a malformed ESPHome key only keeps its own device waiting. |
| D35 | **What an extension can't use yet is shown as "waiting", and a secret can only be given where one is asked for** | An encrypted ESPHome device found on the network isn't a device in the registry — nothing is known about it but its name and MAC — so an extension reports it on a separate list (`docs/specs/protocols.md` §6.6), each item saying what it needs and, for a secret, the path in its `secrets.toml` table where it goes. The UI renders one generic form for any extension. The API writes a secret only at a path an extension is currently asking for, because until there's sign-in (D12) anything on the network can reach it, and "write anything into anyone's settings" is a much bigger hole than "answer a question an extension is asking". A secret is never read back, never logged, and `secrets.toml` is written `0600` with a `.gitignore` beside it. Found while testing: `esphome-client` 0.2.1 discards the "unexpected encryption" error, so a device that wants a key but doesn't announce `api_encryption` looks unreachable rather than locked; all current firmware announces it. |
| D36 | **One id, one name, one description per device, everywhere** | The Home Assistant pattern this avoids: a Zigbee device named one thing in Zigbee2MQTT and another in HA, an ESPHome board with a firmware name, a display name and an entity id that says a third thing — and no way to tell which is "the" name. In Irori a device's **id** is made once from its protocol and permanent handle (`esphome_00_11_22_33_44_55`) and never from a name, so it's the same in the page address, `devices.toml`, the API and rules, and no rename or restart moves it. Its **name** is a person's if they've given one, else what the protocol reported, and only one is ever shown: the firmware's name is where a device's name starts, not a second name kept beside it. Its **description** is only ever a person's. Entity ids build on the device id (`sensor.esphome_00_11_22_33_44_55_temperature`), so a renamed device can't leave entity ids saying its old name. The cost is ids that are unambiguous rather than pretty; names are what people read, and rules editors will show them. Where a protocol can store a name on the device or bridge (Zigbee2MQTT's `friendly_name`), its extension should write Irori's name there — never read it back as a second opinion. |
| D37 | **Ignoring a device keeps it out of the home without forgetting it** | "Adopt everything the network announces" (D29) needs an opposite. An ignored device (`ignored = true` in `devices.toml`) is removed from the registry: not listed, not switchable, nothing recorded, and nothing it says counts as a rejected report. The core keeps what its extension last described and reported, so letting it back in restores it as it is now, straight away, without restarting the extension. It's a core decision rather than each extension's, so every extension gets it; the extension may still talk to the device, which is the honest limit. Asking before adopting new devices at all is the stronger version (D39). |
| D38 | **Irori's own settings live in `irori.toml`, which nothing Irori serves can write** | Bind address, data directory, log level and which extensions are turned off, with a flag or environment variable still winning. Turning an extension off applies while Irori runs, through the same host machinery as new settings (D34); where Irori listens can't change under a running server, so an edit to `[server]` says it needs a restart instead of silently doing nothing. `allow_unauthenticated_lan` is in this file, so while there's no sign-in (D12) no endpoint may write it — otherwise anything that can reach Irori could open it to the network. |
| D39 | **Irori can ask before adding a device it finds** | `[devices] new = "ask"` in `irori.toml` holds each newly found device back — the same way an ignored one is held (D37), so what its extension says is kept — until a person adds or ignores it. The default stays `"add"`: most homes want the ESPHome board they just flashed to appear, and a setting that makes a new install look empty would be a bad first impression. It's the answer to D29's "choosing which devices to adopt" and to a neighbour's devices showing up on a shared network. The decision is written to `devices.toml` (`added = true`), so it survives restarts and is visible in the file. Turning asking on marks everything already in the home as added: a setting that emptied the home the moment it was switched on would be read as data loss. |
| D40 | **Helpers are an extension, and their definitions are its settings** | A toggle ("guests are over") is a switch no device reports, which rules will read and flip. Rather than a special kind of entity in the core, helpers are a built-in extension using only the protocol contract: its settings (`extensions/helpers.toml`) define the toggles, its private storage keeps the value each was left at, and it reports them like any switch — so the core stays smaller, and every helper is a proof that the contract is enough even for something that isn't a real device protocol. The page writes the definitions through endpoints that only edit that extension's file. A helper's one name is the one in its definition; renaming its entity rewrites that rather than adding a second name in `entities.toml` (D36). Numbers, text and timers follow once rules exist to use them. |
| D41 | **The first-party sequential engine's expressions are CEL**, behind `num` / `on` / `available`. The engine is a library (`crates/irori-rules`), **not linked into the core**, and will ship as a downloadable extension. | Spike: `cel` 0.14.5, `default-features = false`. Hallway condition ~30–40 µs/eval on a Mac debug build. Spec: [docs/specs/rules.md](docs/specs/rules.md). |
| D42 | **This roadmap is organized into areas, not sequential phases.** The only place a working order is stated is §0a, a short list meant to be rewritten often | Priorities here get reworked constantly (floorplan jumped the queue twice), and a phase-numbered document made every reorder look like a rewrite of the plan itself rather than an update to one list. Milestone ids (`M0.x`, `M1.x`) are kept as stable references even though the "0"/"1" no longer means "phase" — renumbering them would break every cross-reference in this document for no benefit. |
| D43 | **Floorplan is planned to become a first-party core extension**, not code hardwired into `irori-ui` | It shipped fast as part of the UI crate to get it in front of real use (PR #16), but it's a full page with its own nav entry and config-file writes — the same shape Helpers has (D40) and the shape future built-in features should have, per D21's "extensions are the single unit of modularity." None of today's contribution kinds (`protocol`, `dashboard`/`card` — sandboxed iframes, Phase 2c — or `app` — proxied external process, Phase 3) cleanly fit "a first-party page with direct registry and config-dir access." Mechanism is open — see §11 Q15. |
| D44 | **There is no "integration" concept. There are only extensions, grouped into categories.** The one category defined so far is **`protocol`** (an extension that brings in devices — MQTT, ESPHome, and, in the demo/helpers case, something that behaves like one without a wire protocol behind it). What was `Integration` in code is now the `Protocol` trait (`crates/irori-protocol`, formerly `irori-integration`); `IntegrationId` is `ProtocolId`; `[[contributes.integration]]` is `[[contributes.protocol]]`; the official extension crates are `irori-protocol-<name>` (formerly `irori-int-<name>`); `docs/specs/integrations.md` is `docs/specs/protocols.md` | Owner's direction: "extension" is the only unit (D21); "integration" was a second word for the same thing, left over from the original HA-inspired draft. Renamed the code and this document; **the deeper spec prose in `docs/specs/{protocols,extensions,config,entities,rules}.md` still says "integration" throughout and needs its own pass** — those documents already use "protocol" for the literal wire protocol (MQTT, Zigbee, ESPHome's native API), so a blind find-and-replace there collides the two meanings (e.g. "Protocols are protocols behind one interface"); each needs reading, not sed. Tracked, not yet done. |

### Review notes on the original plan (kept for context)

The original plan's direction holds. The main changes are (including the follow-up on core priorities, D0 and D15–D19):
- MQTT moved out of the core into the first protocol extension; the extension interface is now designed in Phase 0 instead of Phase 3.
- Barebones default build, CLI parity, plain-text config, and CI-enforced performance budgets.
- One extension system (D21–D25) covering protocols, dashboards, cards, and apps, with declared permissions. It replaces the separate "plugins" idea and gives HACS-style needs a first-class home.
- Defined what "structured automations" really requires: an expression language, run modes, waits, traces, versioning.
- Replaced HA's stringly-typed entity model with typed state plus a device/area registry.
- Solved the MQTT broker gap and chose HA MQTT Discovery for onboarding.
- Added auth.
- Separated traces from history.
- Added safety mitigations for generated HTML dashboards.
- Found that multi-user permissions and HACS were listed as problems but never scheduled.

---

## 2. Architecture

```
┌──────────────────────────── irori (single binary) ─────────────────────────────┐
│                                                                                │
│  BUILT-IN EXTENSIONS (cargo features)          CORE (no protocol code)         │
│  ┌──────────────────────────┐                  ┌────────────────────────────┐  │
│  │ mqtt protocol  [default] │── Protocol ──►│ extension host             │  │
│  │  MQTT + HA discovery,    │     trait        │  manifests, permissions,   │  │
│  │  opt. broker             │                  │  lifecycle, health, config │  │
│  ├──────────────────────────┤                  ├────────────────────────────┤  │
│  │ demo protocol  [default] │─────────────────►│ registry · state · events  │  │
│  │  virtual devices         │                  │ context propagation        │  │
│  ├──────────────────────────┤                  ├────────────────────────────┤  │
│  │ zigbee / matter / …      │ (later, opt-in)  │ irori-rules (deterministic)│  │
│  └──────────────────────────┘                  │ irori-recorder (SQLite)    │  │
│                                                └─────────────┬──────────────┘  │
│                                                ┌─────────────▼──────────────┐  │
│  config dir (source of truth, git-friendly)    │ irori-api  WS + HTTP, auth │  │
│  irori.toml · rules/*.json · extensions/*      │ /metrics · UI · app proxy  │  │
│                                                └─────────────┬──────────────┘  │
│  irori-assist (AI) — opt-in feature, NOT in barebones build  │                 │
└──────────────────────────────────────────────────────────────┼─────────────────┘
                                                               │ public contracts
          ┌─────────────────────────┬──────────────────────────┼─────────────────────────┐
          ▼                         ▼                          ▼                         ▼
   Browser: irori-ui         irori CLI                 EXTERNAL EXTENSIONS         Scripts
   Devices · Automations     (full UI parity,          separate processes, any     (irori-client
   Extensions · Settings     --json)                   language:                   or raw WS)
   ┌───────────────────┐                               · protocols (ESPHome,
   │ sandboxed iframes │ ◄── irori.js bridge ──        │  Z-Wave, Tesla, SwitchBot…)
   │ dashboards, cards │                               · apps (terminal, file
   │ app pages         │ ◄── authenticated proxy ──────│  explorer…) (Phase 3)
   └───────────────────┘
```

**The one idea to hold onto:** everything beyond the core is an **extension**, and an extension is a manifest plus contributions (D21). Each contribution kind has one contract that doesn't care where the code runs:

| Kind | Contract | Runs as | Phase |
|---|---|---|---|
| Protocol | `Protocol` trait in-process, or the same operations as WS messages | Built-in (compiled in) or external process | 1 |
| Dashboard | HTML bundle using `irori.js` + declared bindings | Sandboxed iframe (no process) | 2c |
| Card | Web bundle using `irori.js`, embeddable in dashboards | Sandboxed iframe (no process) | 2c |
| App | HTTP UI served by the extension, proxied by the core under `/apps/<id>/` | External process | 3 |

The MQTT extension is the proof that the protocol contract works; the Phase 2c dashboard runtime is the proof for dashboards and cards.

### 2.1 Workspace layout

```
irori_os/
  Cargo.toml                 # workspace
  crates/
    irori-types/             # entities, typed state, registry, rules, traces, API messages,
                             # extension manifests + permissions (the UI renders them too)
                             # serde + schemars; compiles to native AND wasm32
    irori-core/              # registry, state store, event bus, context, extension host
    irori-protocol/          # THE protocol SDK: Protocol trait, manifest, config schema,
                             # entity/service registration types, external-process protocol
    irori-rules/             # rules engine: pure, deterministic, Clock + StateView traits
    irori-recorder/          # rusqlite (bundled), dedicated writer thread, retention
    irori-config/            # load/validate/watch the plain-text config dir (hot reload)
    irori-api/               # axum WS/HTTP, auth, /metrics, static UI serving
    irori-client/            # typed Rust client for the public API (CLI, external extensions, assist)
    irori-ui/                # Leptos CSR app (built to wasm by `cargo xtask ui`, embedded
                             # via rust-embed; outside the workspace, its own dependency tree)
    irori/                   # binary: CLI + wiring + embedded assets; cargo features pick protocols
  extensions/                # first-party extensions of every contribution kind, one dir each
    protocols/mqtt/          # rumqttc, HA discovery → registry, command publishing, optional broker
    protocols/esphome/       # ESPHome's native API: mDNS discovery, entities, state, commands
    demo/                    # virtual lights/sensors/switches; the reference protocol extension to copy
    helpers/                 # toggles Irori keeps itself (D40) — proves the protocol contract works
                             # for something that isn't a real device protocol
                             # ⚠️ Superseded, 2026-09-21: an earlier draft split a top-level
                             # `integrations/` (a word this document no longer uses, D44) from a
                             # Phase-2c+ `extensions/`; the actual layout puts every first-party
                             # extension — protocol contributions included — under one `extensions/`
                             # tree, by category. Dashboards, cards and apps will land here too once
                             # they exist. Floorplan is expected to join this tree too (D43).
  extras/
    irori-assist/            # AI: LLM providers, rule authoring, explainer, dashboards (opt-in)
  examples/
    external-protocol-py/    # ~100-line external protocol extension in Python over WS (proves "any language")
                             # ⏳ not started — M0.6 shipped without it; still the open gap in that spec
  schemas/                   # generated JSON Schemas (checked in, CI verifies fresh)
  docs/specs/                # entity model, rule schema, trace format, API protocol, auth, protocols
  fixtures/                  # recorded Z2M/Tasmota discovery payloads, sample homes
  benches/                   # performance budget benchmarks (§4.3)
  install/                   # install.sh, systemd unit template
```

**Dependency rules (enforce in CI):**
- `irori-core` and `irori-rules` depend on **no** protocol extension (`irori-protocol-*`), no AI crate, and no protocol library (no `rumqttc` in the core's dependency tree). The protocol SDK (`irori-protocol`) is allowed, and required: the core's extension host loads protocol extensions through its `Protocol` trait. The SDK itself must stay protocol-free.
- Protocol extensions depend only on `irori-protocol` and `irori-types`.
- `irori-assist`, external extensions written in Rust, and other tools depend only on `irori-types` and `irori-client` (plus `irori-protocol` for external protocol extensions).

**Cargo features on the `irori` binary:** `default = ["ui"]` (D17); opt-in: `assist` (D13). Official extensions are packages, never cargo features. `--no-default-features` must still build, start, and serve the API. That's the "robust at its smallest" test.

### 2.2 Tech stack

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust (stable) | |
| Async runtime | `tokio` | |
| HTTP / WebSocket | `axum` | |
| MQTT client | `rumqttc` | |
| ESPHome client | `esphome-client` | Native API: mDNS discovery, protobuf, Noise. Pinned to one API version; only `irori-protocol-esphome` depends on it |
| Embedded broker (optional) | `rumqttd` | Evaluate maturity in Phase 0; fall back to "requires external broker" |
| Persistence | `rusqlite` with `bundled` | Sync API on a dedicated writer thread; simpler than `sqlx` and musl-friendly. WAL mode. |
| Serialization / schema | `serde` + `schemars` | JSON Schema generated from Rust types |
| Expression language | **CEL** via `cel` 0.14.5 (D41) | First-party sequential engine only. `default-features = false`. Other engines pick their own language. |
| Time / time zones | `jiff` | DST-correct scheduling; DST bugs are classic automation failures |
| Sun position | `sunrise` (or similar) | sunrise/sunset/elevation triggers |
| Password hashing | `argon2` | |
| Frontend | Leptos (CSR) → wasm, `trunk` build | Confirmed against Dioxus by measurement (D27) |
| Graph layout (visualizer) | Hand-rolled layered layout → SVG | Rule graphs are near-trees; fall back to JS interop (e.g. elkjs) if needed |
| Asset embedding | `rust-embed` | Serve precompressed (brotli) assets |
| Cross-compilation | `cargo-zigbuild`, musl targets | x86_64, aarch64 required; armv7 best-effort |
| LLM access | Thin provider trait over HTTP (`reqwest`) | Anthropic, OpenAI-compatible, Ollama |
| CLI | `clap` (+ `clap_complete`) | Shell completions; `--json` on every read command |
| Config | `toml` + `serde`, `notify` for hot reload | Plain-text config dir is the source of truth (D18) |
| Logs / metrics | `tracing` (JSON or pretty output), Prometheus text at `/metrics` | |
| Allocator (evaluate) | system vs `mimalloc` | Measure against budgets; musl's default allocator is slow |
| Benchmarks | `criterion` + a load-generator binary | Budgets in §4.3 |
| Matter (future protocol) | `rs-matter` | |

---

## 3. Core platform & specs

Goal: the decisions that are expensive to change later are written down and prototyped. **Little product code, lots of leverage.** (Formerly "Phase 0" — kept as an area rather than a first phase; see D42.)

> **What actually happened, in order (D26 — history, not a plan for what comes next):** M0.1 ✅ → M0.2 ✅ → M0.6 ✅ → M1.1 (trimmed) ✅ → M0.8 UI spike ✅ + Devices page ✅ → ESPHome extension ✅ → M0.7 config dir ✅ → M0.3 spec + validation 🔶 (done as a library, per D41 it ships as an extension, not linked into the core) → M0.4, M0.5 still ⏳.

### M0.1 Workspace and toolchain ✅

> Done except the demo on a physical Raspberry Pi (CI covers it under QEMU).

- Cargo workspace with the crates above as empty shells; `fmt`, `clippy -D warnings`, `test` in CI.
- Hello-world binary that embeds a static page and opens a bundled SQLite DB, **cross-compiled to aarch64-musl and run on a Raspberry Pi** (or QEMU).
- Dependency-rule check (§2.1) in CI (e.g. `cargo-deny` bans or a small script over `cargo metadata`).
- **Demo:** `scp` the binary to a Pi, run it, open the page.

### M0.2 Spec: entity and registry model → `docs/specs/entities.md` ✅

> Done: see the spec. It refines the draft below (D20); the spec is authoritative.

- `Device { id, name, manufacturer, model, area_id, via (bridge) }`
- `Area { id, name, floor }`
- `Entity { id: "light.hallway", device_id, area_id?, kind, state: TypedState, attributes: Map, last_changed, last_updated, context }`
- `TypedState` is an enum per kind, e.g. `Light { on, brightness?, color_temp?, rgb? }`, `Sensor { value: f64 | string, unit, device_class }`, `BinarySensor { on, device_class }`, plus `Unavailable`/`Unknown`.
- `Context { id, parent_id?, origin: Device | User(id) | Automation { extension, run_id } | Api(token_id) }`.
- v1 kinds: `light`, `switch`, `sensor`, `binary_sensor`. Next: `cover`, `climate`, `button`/`event`, `lock`.

### M0.3 Spec: first-party sequential engine → `docs/specs/rules.md` 🔶

> 🔶 The spec ([docs/specs/rules.md](docs/specs/rules.md), written in full) and the 3-layer validation
> it describes (parse, AST allow-list, registry type-check — `crates/irori-rules`) shipped in PR #9.
> What's genuinely still missing is the part that waits and calls services — the scheduler/runtime —
> which per D41 was never meant to land here: it ships later as a downloadable extension, not as
> part of `irori serve`. This line went unmarked for a while after the spec landed; fixed 2026-09-21.

This is **an engine**, not the OS. The core does not load or run it. Draft shape:

```json
{
  "id": "hallway_motion_light",
  "name": "Hallway motion light",
  "mode": "restart",
  "triggers": [
    { "type": "state", "entity": "binary_sensor.hallway_motion", "to": true }
  ],
  "conditions": [
    { "type": "expr", "expr": "num('sensor.hallway_lux') < 30" }
  ],
  "actions": [
    { "type": "call", "service": "light.turn_on",
      "target": { "entity": "light.hallway" }, "data": { "brightness_pct": 60 } },
    { "type": "wait", "until": { "type": "state", "entity": "binary_sensor.hallway_motion",
      "to": false, "for": "2m" }, "timeout": "10m" },
    { "type": "call", "service": "light.turn_off", "target": { "entity": "light.hallway" } }
  ]
}
```

Must decide:
- **Triggers:** state (with `for`), time (cron-like and `at`), sun (with offset), event (including events emitted by extensions, e.g. raw MQTT messages from the MQTT extension; the core stays protocol-agnostic), startup.
- **Conditions:** state, expr, time window, sun position; `all`/`any`/`not` combinators.
- **Actions:** call service, delay, wait (state/expr with timeout), `if`/`choose`, set variable, fire event, stop.
- **Modes:** single, restart, queued(max), parallel(max).
- **Expression language:** CEL vs custom. Expressions are type-checked at save time against the registry (entity exists, kind supports the attribute).
- **Node paths:** every node has a stable path (`triggers/0`, `actions/2/then/0`) used by traces and the visualizer.
- **Versioning:** a rule version = content hash; every save creates a version; traces reference a version.
- **Validation layers:** (1) JSON Schema, (2) semantic validation vs registry and services, (3) optional dry-run/backtest (Phase 2).

### M0.4 Spec: trace format → `docs/specs/traces.md` ⏳
- `Run { run_id, rule_id, rule_version, trigger: { node_path, event, context }, started_at, finished_at, outcome: Completed | ConditionFailed(path) | Aborted | Error | Superseded }`
- `Step { node_path, started_at, finished_at, reads: [{ entity, value }], result, outputs, error? }`
- Retention: last N runs per rule (default 50) + max age.

### M0.5 Spec: API protocol and auth → `docs/specs/api.md` ⏳
- WS message envelope `{ id, type, ... }`, modeled on HA's WS API for familiarity. Message types: `auth`, `subscribe_events`, `unsubscribe`, `get_states`, `get_registry`, `call_service`, `rules/list|get|save|delete|validate`, `traces/list|get`, `history/query`.
- HTTP: health, static UI, token-authenticated REST mirrors of read endpoints.
- Auth: first run prints a **one-time setup code** to stdout/log (so whoever reaches the wizard first on the LAN can't claim the instance). The wizard creates the owner account. Access tokens are for API, CLI, and external extensions (extension tokens are scoped to the permissions approved for their manifest, D23).

### M0.6 Spec: extension manifest + protocol contract (the most important specs for modularity) 🔶

> 🔶 Both specs are written: [docs/specs/extensions.md](docs/specs/extensions.md) and
> [docs/specs/protocols.md](docs/specs/protocols.md) (renamed 2026-09-21 from `integrations.md`,
> D44 — the prose inside still says "integration" throughout and hasn't had its own pass yet).
> They refine the draft below (each has a "Changes from the roadmap draft" section) and are
> authoritative otherwise. **Gaps, added 2026-09-21:** the reference implementation
> `examples/external-protocol-py` (listed below and in §2.1) was never written, and open question 6
> — "Unix socket vs WS-only transport, decide in M0.6" — shipped undecided; it's still open (§11 Q6).

**Part A — extension manifest → `docs/specs/extensions.md`** (D21–D25)
- **Package:** a directory or signed archive with `irori-extension.toml`, plus per-architecture binaries (for process-backed contributions) and/or web assets. Built-in extensions embed the same manifest, compiled in.
- **Manifest:**
  ```toml
  [extension]
  id = "switchbot"                 # slug; also the ProtocolId (D25)
  name = "SwitchBot"
  version = "0.3.0"
  irori = ">=0.4.0, <0.5.0"        # compatible core versions
  config_schema = "config.schema.json"

  [[contributes.protocol]]         # Phase 1
  iot_class = "cloud_polling"      # D24
  entity_kinds = ["switch", "sensor"]   # implies their standard services
  run = { command = "bin/switchbot" }   # omitted for built-in

  [[contributes.dashboard]]        # Phase 2c (reserved)
  id = "overview"
  entry = "dashboards/overview/index.html"
  bindings = { curtains = { kind = "cover", many = true } }

  [[contributes.card]]             # Phase 2c (reserved)
  id = "curtain-card"
  entry = "cards/curtain.js"

  [[contributes.app]]              # Phase 3 (reserved)
  id = "logs"
  title = "SwitchBot logs"
  run = { command = "bin/switchbot-logs" }

  [permissions]                    # D23
  network = ["api.switch-bot.com"]
  # api = ["states:read", "services:call"], serial = [], host_fs = [], host_shell = false
  ```
- **Specified fully in Phase 0:** `[extension]`, `[[contributes.protocol]]`, `[permissions]`, compatibility and versioning rules, and how unknown contribution kinds are handled (an older core ignores them with a warning, never a hard error).
- **Sketched only (reserved names and fields):** dashboard, card, app. Their detailed specs land in the phase that builds them.
- **Lifecycle states shown to users:** `installed → enabled → running | degraded | failed`, per extension and per contribution.

**Part B — protocol contract → `docs/specs/protocols.md`**
- **Lifecycle:** `setup(config) → start → running ⇄ degraded → stop`; health reporting with a reason; restart with backoff supervised by the core. A failing extension **never** takes down the core or other extensions.
- **What a protocol extension can do:** register/update/remove devices and entities; push state updates; register service handlers for the kinds it provides; emit events; log through the core (logs tagged by extension); persist small private key/value data.
- **What it cannot do:** read other extensions' internals, touch rules, write to the recorder directly.
- **Rust trait (built-in)**, roughly:
  ```rust
  #[async_trait]
  pub trait Protocol: Send + Sync + 'static {
      fn manifest(&self) -> Manifest;
      async fn setup(&mut self, ctx: ProtocolContext, config: serde_json::Value) -> Result<()>;
      async fn run(&mut self, shutdown: ShutdownSignal) -> Result<()>;
      async fn handle_service(&self, call: ServiceCall) -> Result<ServiceResponse>;
  }
  ```
  `ProtocolContext` is a handle offering exactly the "can do" list above.
- **External wire protocol:** the same operations as WS messages (`protocol/hello` with manifest + token → `protocol/configured` → `device/upsert`, `state/push`, `service/handle` requests from the core, `health`). A built-in protocol extension and an external one must be indistinguishable from the UI and CLI.
- **Config:** each extension's config lives in `extensions/<id>.toml`, validated against its `config_schema`. The UI renders a settings form from the schema and the CLI validates it. Extension authors never write settings UI code.
- **Reference implementations:** `irori-protocol-demo` (virtual devices, ~300 lines, the template to copy) and `examples/external-protocol-py` (the same virtual devices, external, in Python).

### M0.7 Spec: config dir → [docs/specs/config.md](docs/specs/config.md) ✅

> Done: the config directory exists and holds the decisions a person makes about their home —
> rooms (`areas.toml`), and what to call a device or an entity (`devices.toml`, `entities.toml`).
> Loaded at startup, hot-reloaded every two seconds, written atomically, and editable by hand or
> from the UI (D31, D32). `crates/irori-config` reads and writes; the core never touches the disk.
>
> Also done: one id, one name and one description per device (D36), ignoring devices (D37),
> `irori.toml` (D38), and `secrets.toml`, one table per extension, which is how extensions get settings at
> all. Changing a table restarts that extension (D34); an extension can list what it found but
> can't use yet, and the UI takes a secret for it (D35). Encrypted ESPHome devices work.
>
> Also done: floors (rooms grouped by level), asking before adding newly found devices (D39),
> `extensions/<id>.toml` joined with the extension's secrets, extension private storage
> (`protocols.md` §5, in SQLite), the first helpers — toggles, switches Irori keeps itself
> (D40), with a Helpers tab on the Devices page, and `floorplan.toml` (PR #16 — walls, floors,
> rooms, device placement; see [docs/specs/config.md](docs/specs/config.md) §3.7).
>
> **Not yet** *(updated 2026-09-21 — the previous version of this note said the M0.3 spec itself was
> outstanding, which stopped being true once PR #9 shipped it; what's actually still missing is only
> the config-dir loader for it)*: loading `rules/<id>.json` (the spec and validation exist,
> `crates/irori-config` just doesn't read the directory yet — waits on the rules runtime landing,
> M1.4), approved permissions and `config_schema` validation in `extensions/<id>.toml`, and helpers
> other than toggles.
- Config dir layout: `irori.toml` (server, location, recorder retention, enabled extensions), `rules/<id>.json`, `extensions/<id>.toml` (config plus approved permissions), `areas.toml`. Secrets go in a separate file (e.g. `secrets.toml`) that is git-ignorable. Runtime data (SQLite DB) lives in a separate data dir.
- Hot reload: file changes are validated, then applied atomically; invalid files are rejected with a clear error and the last good version stays active.
- UI edits write the same files (humans and the UI share one source of truth).
- CLI command tree (UI parity): see M1.7.

### M0.8 Spikes (timeboxed, one or two evenings each) 🔶 2 of 5 done
- CEL in Rust ✅ (D41): `cel` 0.14.5 against a fake hallway `StateView` in `crates/irori-rules`. Error messages name the entity id. ~39 µs/eval on a Mac debug build. Pi 4 and wasm still outstanding.
- Leptos vs Dioxus ✅ (D27): the same page in both, sharing `irori-types`, measured after `wasm-opt` and brotli. Leptos 119 KB, Dioxus 207 KB, both inside the 500 KB budget. The spikes then became the real Devices page; see [crates/irori-ui/README.md](crates/irori-ui/README.md).
- `rumqttd` embedded: start in-process, connect Zigbee2MQTT to it.
- `Protocol` trait vs external protocol: implement the demo extension both ways against a stub core, to prove the contract really is the same.
- Baseline measurement: empty `irori` binary on a Pi 4 (RSS, startup, size) to calibrate the budgets in §4.3.

**Exit criteria for this area** ⏳ **not yet met:** specs merged; JSON Schemas generated from `irori-types` (rules, manifests, config); a folder of golden example rules and configs (valid and invalid) with a test that validates them; spike results recorded in the decision log; performance budgets set from real baseline numbers *(blocked on the Pi 4 baseline spike above, still not run)*.

---

## 4. Core MVP: registry, protocols, recorder, rules, API, UI

Goal: a **barebones, fast, modular core** that a real home runs on for weeks without drama. The
deliverable *is* the barebones release: core + CLI + minimal UI + MQTT and Demo extensions. The only
contribution kind implemented so far is **protocol** (D22). (Formerly "Phase 1"; the milestones
below (M1.x) don't have to land in this order — see D42 and §0a.)

### M1.1 Registry, state store, event bus, extension host (≈3–4 wks) 🔶

> Trimmed version done (D26): in-memory registry and state with every check from the protocol contract, events, service calls (toggle, capability checks, 10 s timeout), the extension host with crash isolation and restart backoff, the `Protocol` trait, and the demo extension. A temporary read-only view at `/api/dev/*`. **Not yet** *(updated 2026-09-21 — config dir loading and hot reload shipped with M0.7 and is removed from this list)*: the CLI demo (needs the API, M1.5), and keeping the registry across restarts (M1.3).
- In-memory registry and state store; `tokio::sync::broadcast` event bus with typed events (`StateChanged`, `ServiceCalled`, `RuleRun*`, `RegistryUpdated`, `ExtensionStatus`).
- Context propagation through every state change and service call.
- Service registry: protocol extensions register handlers for the kinds they provide.
- **Extension host:** reads manifests of built-in extensions, checks compatibility and permissions, loads the ones enabled in `irori.toml`, and supervises them (panic/crash isolation, restart with backoff, health status).
- `irori-protocol-demo` built against the `Protocol` trait: virtual lights, switch, motion, illuminance, mmWave occupancy and target distance, temperature.
- Config dir loading and hot reload (`irori-config`).
- **Demo:** `irori serve` with only the demo extension; `irori` CLI lists devices, toggles a virtual light, and watches events with contexts. Kill the demo extension's task and watch the core restart it.

### M1.2 MQTT protocol + HA Discovery (≈3–4 wks) ⏳

> ⏳ Not started — `extensions/protocols/mqtt`'s own manifest says "Not implemented yet."; stub source only.

- Implemented **only** through the `Protocol` trait. If the trait can't express something MQTT needs, fix the trait, never special-case the core.
- Connect to an external broker (URL, credentials, TLS). Optional embedded broker (config flag).
- Subscribe to `homeassistant/+/+/config` and `homeassistant/+/+/+/config` (retained) → create devices and entities.
- Support `light` (JSON schema, as used by Z2M, plus the default schema), `switch`, `sensor`, `binary_sensor`; availability topics; value templates only in the simple forms Z2M/Tasmota actually emit (JSON path), not full Jinja.
- Service calls publish to the command topics given in discovery.
- Test fixtures: recorded discovery payloads from Z2M, Tasmota, ESPHome.
- **Demo — shadow mode (§4.2):** Irori connected to the broker your HA + Z2M already use; all Zigbee devices and their entities appear with live state; toggling a light from Irori works.

### M1.3 Recorder (≈2 wks) ⏳

> ⏳ Not started — `crates/irori-recorder` is a 3-line stub.

- SQLite in WAL mode; dedicated writer thread; batch commits (e.g. every 1s or 500 rows).
- Tables: `states`, `events`, `rule_versions` (snapshots of each rule file version the engine loaded), `rule_runs`, `rule_steps`, `users`, `tokens`, `registry_*`, `extension_kv`. Authored config (rules, extension config) stays in the config dir (D18).
- Retention/purge job (default 10 days of states; configurable excludes for chatty sensors) to limit SD card wear.
- History query API (entity, time range, downsampling for numeric sensors).
- **Demo:** 48h of real home history queried and plotted as text or CSV.

### M1.4 Rules engine runtime (≈5–7 wks — the heart; don't rush it) ⏳

> ⚠️ **Scope changed, 2026-09-21:** this milestone sits under "Core MVP" as if the runtime below ships
> compiled into `irori serve`. Per D41 it doesn't — the schema, parser, and validation (§M0.3) are a
> library the core never links, and the scheduler described here ships later as a downloadable
> extension, same as any other automation engine (D8, D21). `crates/irori-rules`'s own module doc says
> as much: "the engine that waits and calls services is later, and it will load as an extension, not
> as part of `irori serve`." Nothing below this note is started yet.

- `irori-rules` is pure: `trait Clock`, `trait StateView`, `trait ServiceCaller`; a runtime adapter wires them to the real core.
- Implements the Phase 0 spec: triggers, conditions, actions, modes, waits with timeouts, `for` durations, expressions.
- Emits traces for every run, persisted by the recorder.
- Semantic validation on save (entities exist, services exist, expressions type-check).
- **Test strategy:** golden tests with a simulated clock (e.g. "motion at 22:00:00, no motion at 22:00:30 → off at 22:02:30"); DST transition tests; property tests for expression evaluation; mode tests (restart/queued under bursts).
- **Demo:** the hallway motion rule runs on real devices; the raw trace JSON shows every read and decision.

### M1.5 API, auth, external extensions (≈3–4 wks) ⏳

> ⏳ Not started — `irori-api` and `irori-client` are 3-line stubs. Until this lands, D12's
> `--allow-unauthenticated-lan` requirement stays in force.

- WS and HTTP per spec; owner account; access tokens; `irori-client` crate with typed calls.
- `/metrics` (Prometheus text): event throughput, rule runs, extension health, recorder queue depth, memory.
- **External extension protocol** (D16): handshake, scoped tokens, device/state/service messages.
- **Local extensions:** `irori extensions add <path>` reads the manifest, shows its permissions for approval, and writes `extensions/<id>.toml`; the core spawns and supervises the extension's process. Installing from a registry comes in Phase 3.
- Permission enforcement for what Phase 1 can use: API scopes and entity/service scoping on the extension's token. Network, serial, and host permissions are recorded and shown, but only enforced where the OS makes it practical (documented honestly).
- **Demo:** the Python example extension's virtual devices appear next to the MQTT devices, indistinguishable in the CLI and API; a rule uses one of each.

### M1.6 Barebones embedded UI (≈3–4 wks) 🔶

> First slice done (D26): the **Devices** page, in Leptos, embedded in the binary — every entity
> grouped by device with live state, a switch for lights and switches, readings for sensors, and
> the extensions behind them. It polls `/api/dev/home` and commands through `/api/dev/command`
> until the real API exists.
>
> Since then: brightness and colour (PR #12) and areas, grouped by floor (PR #15), both shipped —
> removed from "Not yet" below, updated 2026-09-21. A Start screen (➕, PR #14) and the **Floorplan**
> page (➕, PR #16 — walls, floors, rooms, drag devices into rooms) also shipped; neither is on this
> list because neither was ever planned here (D17's UI list didn't include them either). Floorplan is
> planned to move out of this crate into its own extension (D43); Start likely stays here.
>
> **Not yet:** history, pushed changes instead of polling (M1.5), and serving the assets
> precompressed (§2.2), which the download budget in §4.3 assumes. See
> [crates/irori-ui/README.md](crates/irori-ui/README.md).

Deliberately minimal: fast to load, no dashboards, no charts beyond the basics. Four sections:
- **Setup wizard (first run):** setup code → owner account → enable extensions (MQTT: detect `localhost:1883` or use the embedded broker) → location and time zone.
- **Devices:** devices grouped by area/extension → entities with live state; toggle or set values; basic entity history (last 24h list or sparkline); rename, assign area; `iot_class` badge (local/cloud).
- **Automations:** rule list with enabled toggle and last run outcome; JSON editor with live schema and semantic validation inline (same validation code as the server, via wasm); trace list per rule as a raw step table (the visualizer comes in Phase 2).
- **Extensions:** installed extensions (built-in and external) with their contributions, status, health, logs, and approved permissions; enable/disable; settings form auto-generated from each extension's `config_schema`. Later phases add Dashboards and app pages to the sidebar only when an installed extension contributes them.
- **Settings:** access tokens, location/time zone, recorder retention, system info (version, uptime, memory, DB size).
- Budget: UI bundle within §4.3; first load on a Pi-served LAN < 1 s.
- **Demo:** the full loop from a fresh install in the browser: enable the MQTT extension → device appears → write rule → it fires → trace visible.

### M1.8 Firmware updates (≈1–2 wks) ⏳

> ⏳ Not started — no `update` entity kind exists yet (`crates/irori-types/src/kind.rs` has only
> `Light`, `Switch`, `Sensor`, `BinarySensor`).

A device that can update itself says so, and a person can let it (D30).

- **New entity kind: `update`** (`docs/specs/entities.md`). Capabilities: whether it can check on
  demand. State: `installed_version`, `latest_version`, `title`, `release_url`, `in_progress`,
  `progress`. Services: `update.install`, `update.check`.
- **ESPHome maps straight onto it**: its native API already carries
  `ListEntitiesUpdateResponse`, `UpdateStateResponse` and `UpdateCommandRequest`
  (`UPDATE` / `CHECK`), which is how Home Assistant offers ESPHome OTA. No ESPHome-specific
  screen is needed for this.
- **UI**: on the device page, "Firmware 2026.8.2 → 2026.9.1", the release notes, an Install
  button and progress. In the list, a device with an update available is marked.
- **Not this**: building firmware. Compiling and flashing from source is the ESPHome dashboard's
  job, and in Irori that's an extension contributing an `app` (Phase 3, §8), not core work.
- **Demo:** a device with an update pending, installed from the Devices page, with the progress
  visible and the version changing when it comes back.

### M1.7 CLI, packaging, release (≈2–3 wks) 🔶

> 🔶 The install pipeline (`install/install.sh`, PR #10) and the release/packaging tooling
> (`xtask/src/package.rs`, PR #13) are done. The CLI-parity command tree below is essentially
> unbuilt: `crates/irori/src/main.rs` only has `serve` and `version`.

CLI has **full parity with the UI**; every read command supports `--json`; shell completions:
```
irori serve [--config DIR] [--data DIR]
irori status                                  # core health, extensions, metrics summary
irori devices list|show <id>
irori entities list|get <id>|watch [<id>…]    # live stream
irori call <service> [--target <entity>] [--data k=v…]
irori rules list|show|validate <file>|apply <file>|enable|disable|delete
irori traces list <rule>|show <run_id>
irori extensions list|show|add <path>|enable|disable|remove|permissions|config validate|logs <id>
                                              # alias: irori ext …
irori history <entity> [--since 24h]
irori token create|list|revoke
irori config check                            # validate the whole config dir
irori backup|restore
irori completions <shell>
irori version
```
- The CLI talks to a running instance over the public API via `irori-client` (it's just another client); `config check` and `rules validate` also work offline.
- `install.sh`: detect OS/arch → download static binary → **verify SHA-256 (and signature, e.g. minisign)** → create config and data dirs → optionally install systemd unit → start → print wizard URL and setup code.
- Release pipeline: tagged builds for x86_64 and aarch64 musl, checksums, changelog; a `--no-default-features` "core-only" artifact as well.
- **Demo:** fresh Pi, `curl … | sh`, wizard in under 10 seconds (excluding download time); the entire setup redone from the CLI alone, with the config dir committed to git.

### 4.2 Shadow mode (the validation strategy for core-first) ⏳

> ⏳ Can't have started — its prerequisite, M1.2 (MQTT), isn't built yet.

Irori connects to the same MQTT broker as an existing HA + Zigbee2MQTT setup. Retained discovery messages give it every device without touching HA. Run a few low-stakes rules on Irori (e.g. a closet light) while HA keeps doing everything else. This is the dogfooding path for the author, and later the "try it without migrating" path for users.

**Watch out:** two controllers commanding the same device can fight. Document "assign each device's automations to one system".

### 4.3 Performance budgets ("lightning fast, lightweight") ⚠️

> ⚠️ *Added 2026-09-21:* only the UI bundle row below is actually gated in `ci.yml` today (the
> "Download stays inside the budget" step). The other six rows have no benchmark or CI check yet —
> D19's "regression over budget fails the build" only holds for this one metric so far.

Initial targets, to be recalibrated from the Phase 0 baseline (still outstanding — see §3's exit criteria). Measured in CI (benchmarks and a load generator) and on a real Raspberry Pi 4 before each release. A regression over budget fails the build.

| Metric (default build, Pi 4) | Budget | CI-gated? |
|---|---|---|
| Binary size (with UI) | < 15 MB | ⏳ no |
| Barebones UI bundle (wasm + assets, brotli) | < 500 KB | ✅ yes |
| Cold start to API ready | < 200 ms (excluding broker connect) | ⏳ no |
| Idle RSS, empty home | < 15 MB | ⏳ no |
| RSS with 1,000 entities + 10 days history + 100 rules | < 50 MB | ⏳ no |
| Internal latency: state change → rule evaluated → service call dispatched | p99 < 5 ms | ⏳ no |
| Sustained throughput without backlog | ≥ 2,000 state changes/s | ⏳ no |
| Recorder writes | batched; zero per-event fsync | ⏳ no |

**"Robust at its smallest" checks:** `--no-default-features` builds and runs; the core survives extension crashes, a broker outage (reconnect with backoff), a full disk (recorder degrades, rules keep running), and invalid config files (rejected, last good config stays active).

### Exit criteria for this area ⏳ not yet met
- Runs 30 consecutive days in the author's home in shadow mode with ≥ 5 real rules, no crashes, no missed triggers found.
- All §4.3 budgets met on a Pi 4.
- Adding a new protocol required **zero** changes to `irori-core`: proven by the MQTT, Demo, and Python external extensions, all described by the same manifest format.
- Everything is doable from the UI *and* from the CLI; the config dir round-trips through git.
- A rule can be written, validated, saved, run, and debugged (raw trace) entirely from the barebones UI.

---

## 5. Automation visualizer and debugger ⏳ (≈2–3 months)

Goal: the first thing that makes someone say "HA can't do that". Public alpha candidate. (Formerly
"Phase 2a" — an area like any other now, see D42; not started, and blocked on M1.4's runtime existing
at all since there'd be nothing to visualize otherwise.)

- **Graph view:** render a rule version as a left-to-right graph (triggers → conditions → action sequence with branches and waits). Layered layout, SVG, in `irori-ui`.
- **Trace playback:** pick a run → highlight the executed path; failed condition in red; click a node to see values read, result, and timing; step forward and back.
- **Run timeline:** per rule, runs over time with outcomes; filter "condition failed at node X".
- **Version diff:** graph and JSON diff between rule versions; traces always render against the version they ran.
- **Backtest / replay:** "run this rule (or this draft) against the last N days of recorded history" using the deterministic engine with a replay `Clock` and `StateView`; output = simulated runs and traces. Clearly label limits: replay can't know how devices *would* have reacted to simulated actions.
- **Visual editing (stretch):** edit simple rules from the graph; JSON stays the source of truth.
- **Demo:** the "day with Irori" scenario from INSPIRATION.md §4, minus the AI explanation.

## 6. AI layer ⏳ (≈3–4 months)

(Formerly "Phase 2b/2c" — not started; parked, not blocked, per §0a.) Everything in this section is **opt-in** (the `assist` cargo feature or a separate `irori-assist` process). The barebones build never includes it, and the core never depends on it (D13, D17). In the UI, AI features appear only when enabled.

### 6.1 Providers (`irori-assist`)
- `trait LlmProvider { complete(req) -> Result<Resp> }` with structured output / tool-use support; impls: Anthropic, OpenAI-compatible (covers many hosts), Ollama.
- Keys stored server-side (never exposed to the browser), configurable per feature (e.g. a cheap local model for explanations, a stronger cloud model for generation).
- Eval harness: a set of fixture homes (registries + history) and prompts with expected properties, runnable in CI against a chosen provider to catch prompt regressions.

### 6.2 AI rule authoring and explanation (Phase 2b)
- **Natural language → rule** with a validation loop: generate → JSON Schema check → semantic check vs registry → backtest summary → feed errors back to the model (max N iterations) → present a draft with graph, backtest, and diff for user approval. Never auto-save.
- **"Why did / didn't this fire?"** — context = rule version + relevant traces + history around the window → explanation plus an optional suggested edit (which goes through the same validation loop).
- **Demo:** the full INSPIRATION.md §4 scenario.

### 6.3 AI dashboard generation — generated HTML (Phase 2c)
The model generates self-contained HTML/CSS/JS dashboards. Required guardrails (D4):

- **Sandbox:** render in `<iframe sandbox="allow-scripts">` (no `allow-same-origin`), served with a strict CSP: no external network, no inline access to the host app.
- **Bridge SDK:** a tiny, stable, injected `irori.js` (`irori.subscribe(entityId, cb)`, `irori.call(service, target, data)`, `irori.history(entityId, range)`) talking to the host via `postMessage`. Generated code targets this small API, which lowers LLM error rates and keeps dashboards working across Irori upgrades.
- **Capability scoping:** at generation time, extract the entities and services the dashboard uses → the user approves the list → the host bridge enforces the allowlist. The dashboard never holds an API token.
- **Versioning:** store prompt, model, registry snapshot hash, generated HTML, and approved capabilities; history, diff, rollback, "regenerate with changes".
- **Knowledge base:** curated examples of good layouts per device type and form factor (wall tablet, phone, desktop) plus known anti-patterns, fed as few-shot context; grows from dashboards the user keeps.
- **Static checks before preview:** parse HTML, reject external URLs, lint that SDK calls reference allowlisted entities.
- **Demo:** "kitchen tablet dashboard" prompt → sandboxed preview → approve capabilities → pin as the default for a device or URL.

### 6.4 Dashboard and card contributions (Phase 2c, D22)
The sandbox, bridge, and capability allowlist above are *the* runtime for all dashboards, not only AI-generated ones. This is what makes pre-built dashboards and visualizations installable as extensions:
- **Dashboard contribution:** an HTML bundle using `irori.js` plus declared **bindings** (slots such as "all covers", "a temperature sensor in this area"). At install or first open, bindings are matched to the home's entities (auto-suggested, user-confirmed) and become the allowlist. The same dashboard works in any home.
- **Card contribution:** a web bundle registered under a name (`switchbot.curtain-card`) that dashboards, including AI-generated ones, can embed. It gets only the entities passed to it. The composition model (card inside the dashboard's sandbox vs. its own nested sandbox) is decided in the dashboards spec.
- **Generated → shareable:** "export as extension" turns a generated or hand-edited dashboard into a dashboard extension package (manifest + bundle + bindings instead of concrete entity ids).
- The barebones build still has no dashboards. The Dashboards sidebar section appears only when a dashboard extension is installed or `assist` is enabled.
- **Demo:** install a first-party "Home overview" dashboard extension from a local path; it binds to the demo extension's virtual devices and to real MQTT devices without edits.

## 7. HA backend adapter ⏳ (from the original plan)

(Formerly "Phase 2d" — not started; parked, not blocked, per §0a.) Once the visualizer and AI features work on Irori, make them work against an existing HA instance via its WS API. This reaches HA's user base without asking anyone to migrate.

- Client-side `Backend` abstraction (Irori or HA) in `irori-client`; design the trait in Phase 1 even if only Irori implements it.
- HA's automation config and trace JSON map onto Irori's rule and trace types (lossy where Jinja is involved; show those nodes as opaque).
- Decide the packaging then: an Irori binary in "HA companion mode", or an HA add-on.

---

## 8. Extension ecosystem and users ⏳ (≈4–6 months)

The manifest and the protocol, dashboard, and card contracts already exist and are proven (M0.6, M1.1, M1.5, §6.4). This area makes extensions **easy to install and share**, adds the **app** contribution kind, and ships the first new protocols and vendor connectors. (Formerly "Phase 3".)

### 8.1 Installing and sharing
- **`irori extensions install <name|url|path>`:** fetch a signed extension from a registry index (a simple git-hosted index is enough to start), verify signature and core compatibility, show contributions and permissions for approval, write `extensions/<id>.toml`, and start what needs starting. Plus `update` (re-approval if permissions grow, D23), `remove`, and a matching browse/install view in the UI's Extensions section.
- **Id namespacing:** decide before the registry opens (open question 12).
- **SDKs and templates, one per kind:** `cargo generate` templates (protocol built-in or external, app), a published Python package for external protocol extensions, a starter bundle for dashboards and cards, and "write your first extension in 30 minutes" docs.
- **Promotion path:** popular external protocol extensions can become built-in (compiled in, opt-in cargo feature) without changing their behavior, since it's the same contract.

### 8.2 App contributions
- **Contract:** the extension's process serves HTTP on a Unix socket (or loopback port) the core assigns. The core reverse-proxies it under `/apps/<id>/` behind Irori auth (similar to HA's "ingress"), and adds a sidebar entry. The app gets an extension token scoped to its approved permissions if it needs the Irori API.
- **Isolation:** app pages run in a sandboxed iframe with no access to the host UI's origin; apps never see user session cookies.
- **Host-access permissions (D23):** `host_shell` and broad `host_fs` show a "full access to this machine" warning, require the owner role, and are listed prominently in the Extensions view and `irori extensions permissions`.
- **First apps (first-party, opt-in, great for nerds and for proving the contract):**
  1. **Config editor / file explorer** scoped to the Irori config dir (`host_fs = ["$CONFIG"]`): edit rules and extension configs in the browser with validation.
  2. **Log viewer** over the core's structured logs.
  3. **Terminal** (`host_shell = true`): the canonical high-privilege example.

### 8.3 First new protocols (in order of value for tinkerers)
1. **ESPHome native API** (ESPHome's default transport, not MQTT). ✅ *Done early on the home-testing path (D26): `extensions/protocols/esphome` (an earlier draft of this document had it at `integrations/irori-int-esphome` — path and name corrected 2026-09-21, see §2.1 and D44), discovery and all four entity kinds, plaintext and encrypted (D35).*
2. **Z-Wave** via `zwave-js-server` (external)
3. **Matter** via `rs-matter` (built-in, opt-in feature)
4. **One vendor cloud connector** (e.g. SwitchBot, which has a documented public API) to prove `cloud_polling`/`cloud_push`, credential handling via `secrets.toml`, and the cloud badge end to end
5. Native Zigbee: low priority while Zigbee2MQTT covers it.

### 8.4 Users and the rest
- **Multi-user:** users, roles (owner, admin, member, guest), per-user default dashboards, per-area/per-entity permissions enforced in the API layer; per-role visibility of apps (e.g. only owners see the terminal); audit log via contexts.
- **HA automation importer:** HA automation YAML → Irori rules, with a report of unconvertible parts (templates).
- **Backup and restore:** single-file snapshots; scheduled backups.

## 9. Product and public launch ⏳ (formerly "Phase 4")

- Public alpha (target: after Phase 2a), then beta (after 2b).
- **License decision (D7)** — decided, before the repo goes public: Apache-2.0 (see the decision log and `LICENSE`).
- Docs site; `irori.dev` (check domain and trademark availability early, even in Phase 0).
- Hosted AI tier as an `LlmProvider` backed by an Irori service (usage credits).
- Remote access (e.g. an optional relay) and off-site backups as paid conveniences.
- Decide on an appliance image vs "binary on your Linux box" based on alpha user feedback.

### Lightweight validation along the way (compensates for D1)
- Publish the rule schema spec and a short write-up after Phase 0; ask for feedback in r/homeassistant, r/homeautomation, and the HA forums.
- Short dev-log posts or videos at M1.2 (shadow mode), M1.4 (traces), and Phase 2a (visualizer).
- Track: people who ask to try it, GitHub stars/watchers, and inbound "does it support X?" requests (they tell you which extension to build next).

---

## 10. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Solo-project scope creep / burnout | Project stalls | Demo-gated milestones; barebones scope for Phase 1; resist new extensions and polish before Phase 2a ships |
| Extension model over-generalized too early | Months designing dashboard/app contracts before the core works | D22: Phase 1 implements only the protocol kind; other kinds get reserved names in the manifest, nothing more |
| Protocol contract designed wrong | Every future protocol is painful; core changes leak in | Build MQTT, Demo, and an external Python extension against it in Phase 1; rule: fix the contract, never special-case the core |
| A built-in extension crashes or blocks the core | Robustness promise broken | Per-extension task supervision, panic isolation, bounded channels; third-party code runs only as external processes or in sandboxes |
| Malicious or compromised extension (especially apps with host access) | Full compromise of the home controller and the machine | Declared permissions + approval (D23), signed packages, re-approval when permissions grow, owner-only host access, sandboxed web contributions, clear warnings; document that permissions of native processes are best-effort without OS-level sandboxing (evaluate systemd/landlock later) |
| Performance drifts as features land | "Lightning fast" becomes marketing | CI-enforced budgets (§4.3); optional features compiled out, not just disabled |
| Plain-text config and UI edits conflict | Lost edits, confusing state | UI writes files atomically; file watcher detects external edits; conflicts surface as errors, never silent overwrites |
| Core-first validates the product idea late (D1) | Months spent on unwanted features | Shadow mode, early spec and dev-log posts, public alpha right after 2a |
| HA closes the UX gap | Weaker reason to switch | Compete on coherence (typed rules + traces + replay + AI); HA adapter (§7) turns HA users into users of those features |
| Rules engine semantics wrong or incomplete | Unreliable automations → lost trust | Spec first, golden and DST tests, 30-day dogfooding gate |
| Generated HTML dashboards are insecure or fragile (D4) | Security incident, broken dashboards after upgrades | Sandbox + CSP + bridge allowlist + stable SDK + versioning (§6.3) |
| Rust/WASM UI ecosystem gaps (graph rendering, charts) (D5) | Slower visualizer work | Phase 0 spike; hand-rolled SVG; JS interop escape hatch for layout or charts |
| Local LLMs too weak for generation | Poor AI experience for privacy-focused users | Validation loops, eval harness, per-feature model choice |
| SD card wear from the recorder | Hardware failures blamed on Irori | Batched writes, retention, excludes, documented SSD recommendation |
| Two controllers fighting in shadow mode | Confusing device behavior | Docs plus a UI warning when a device is also commanded by another source (visible via MQTT) |
| ~~License ambiguity blocks contributors~~ | ~~Can't accept PRs~~ | Resolved: Apache-2.0 (D7) |

---

## 11. Open questions

1. ~~Rule storage~~ → decided: plain-text config dir is the source of truth (D18).
2. ~~**Expression language:** CEL vs a tiny custom language~~ → decided: CEL (D41). *(Marked resolved 2026-09-21 — this was decided when the others were struck but never updated.)*
3. ~~**Leptos vs Dioxus**~~ — decided: Leptos (D27).
4. **Embedded broker default:** off (external broker) or on when none is detected?
5. **Where AI features run when enabled:** `assist` cargo feature in the same binary vs a separate `irori-assist` process. Both are allowed by D13; pick a default in Phase 2.
6. **External extension transport:** WS over TCP only, or also a Unix socket for local extensions (faster, no port)? ⚠️ Was supposed to be decided in M0.6, which has since shipped — still open, deadline passed without a decision (noted 2026-09-21).
7. **Built-in extension loading:** compile-time only (cargo features), or also dynamic loading (`.so`/WASM components) later? *Lean: compile-time + external processes only; WASM components are an interesting Phase 3+ experiment for sandboxed in-process extensions.*
8. **Config format details:** TOML for everything, or JSON for rules (current) and TOML for the rest? JSON rules match the schema and LLM output; TOML reads nicer by hand.
9. **Monetization mechanism:** hosted AI credits vs remote access vs a hosted instance.
10. **Appliance image:** yes or no (defer to Phase 4 with user data).
11. ~~**License**~~ → decided: Apache-2.0 (D7).
12. **Extension id namespacing:** flat slugs (`switchbot`, today's `ProtocolId` format) or namespaced (`author.switchbot`) to avoid collisions in a public registry? Namespacing would need a new id format, since the current slug rules forbid dots. Decide before the registry opens in Phase 3.
13. **Card composition:** do cards run inside the dashboard's sandbox (simpler, faster) or each in a nested sandbox (stronger isolation)? Decide in the dashboards spec (§6.4).
14. **Extensions contributing rule building blocks:** should extensions add typed triggers/conditions/actions, or only services and events (which rules can already use)? *Lean: services and events only, to keep D8's closed rule schema.*
15. **What contribution kind does a first-party full page (Floorplan, and whatever comes after it) use?** *(Added 2026-09-21, per D43.)* Today's kinds are `protocol` (headless), `dashboard`/`card` (sandboxed iframe + `irori.js` bridge, Phase 2c), and `app` (proxied external process, Phase 3) — none give a first-party page direct registry and config-dir access from inside the main nav the way Devices or Settings have it. Options: a new `page` contribution kind reserved for first-party, trusted-by-construction extensions only; or relax `app`/`dashboard` to allow an in-process, unsandboxed variant when the extension is first-party. Decide before moving Floorplan.
