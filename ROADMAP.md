# Irori — Roadmap

Companion document: [INSPIRATION.md](INSPIRATION.md) (vision, tenets, target user).
Last revised: 2026-09-15. Based on the original `irori-project-plan.md`, revised after review.

---

## 0. How to use this document

- Phases are sequential; milestones within a phase are mostly sequential.
- Every milestone ends with a **demo** — something you can show running. If it can't be demoed, it isn't done.
- Estimates assume a solo side project (~8–12 h/week). They're gut-feel sizing to catch scope creep, not commitments.
- When a decision changes, update §1 (Decision log) first, then the affected sections.

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
| D7 | **License: decide later — hard deadline** before the repo goes public *or* the first external contribution, whichever comes first | Relicensing after outside contributions needs every contributor's consent. |
| D8 | **Automations are a closed typed schema + small expression language, no templates** | The advantage is validation against the real home, not "JSON vs YAML". |
| D9 | **Traces are emitted by the rules engine, stored separately from state history** | The visualizer needs "condition X read Y and evaluated false", which state history can't reconstruct. |
| D10 | **Rules engine is deterministic: injected clock + state source** | Enables backtesting/replay (a key Phase 2 feature) and makes tests trivial. Costs nothing if done on day one, very expensive to retrofit. |
| D11 | **The MQTT integration speaks HA MQTT Discovery** | Zigbee2MQTT, Tasmota, ESPHome-over-MQTT already publish it → zero-config device onboarding. |
| D12 | **Auth exists from the first network-facing build** | Single owner + access tokens in Phase 1; multi-user in Phase 3. A LAN-exposed unauthenticated home controller is not acceptable, even in alpha. Until auth lands (M1.5), `irori serve` refuses non-loopback binds unless `--allow-unauthenticated-lan` is passed; the flag is removed with auth. |
| D13 | **AI features live in a separate crate that depends only on the public API client**, available behind an opt-in cargo feature (off in the barebones build, see D17) or as a separate process | Keeps "not part of core" enforced by the compiler; the core never pays for AI it doesn't use. |
| D14 | **HA-familiar domain/service vocabulary** (`light.turn_on`, `binary_sensor`, …) with typed state | LLMs already know it; eases a future HA backend adapter and HA importer. |
| D15 | **Protocols are integrations behind one interface; the core has no protocol code.** MQTT is the first integration, not part of the core | Replaces the original plan's "MQTT bundled in core". Building MQTT against the interface proves the interface is good enough for Zigbee/Matter/Z-Wave later. |
| D16 | **Two integration tiers, one contract:** *built-in* (Rust crates compiled in via cargo features, run in-process through the `Integration` trait) and *external* (any language, separate process, same contract over the WS API) | Built-in = fastest, single binary, first-party only. External = crash-isolated, language-agnostic, how nerds and third parties extend Irori. External integrations work in Phase 1 (run from a local path); the install-from-registry flow comes in Phase 3. |
| D17 | **Barebones default build:** core + CLI + minimal UI (Devices, Automations, Integrations, Settings). Only the MQTT and Demo integrations are compiled in by default; AI is **not** in the default build | "Robust at its smallest". A slim build with no integrations must still start and serve the UI. |
| D18 | **Nerd friendly as a requirement:** plain-text config (`irori.toml`, `rules/*.json`, `integrations/*.toml`) as the source of truth; CLI can do everything the UI can, with `--json`; structured logs; `/metrics`; shell completions | Makes the system scriptable, diffable, and git-friendly. SQLite holds runtime data (history, traces, versions), not the config users author. |
| D19 | **Performance budgets enforced in CI** (§4.3) | Otherwise "lightning fast" drifts. Benchmarks run on every PR; budget regressions fail the build. |

### Review notes on the original plan (kept for context)

The original plan's direction holds. The main changes are (including the follow-up on core priorities, D0 and D15–D19):
- MQTT moved out of the core into the first integration; the integration interface is now designed in Phase 0 instead of Phase 3.
- Barebones default build, CLI parity, plain-text config, and CI-enforced performance budgets.
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
│  BUILT-IN INTEGRATIONS (cargo features)        CORE (no protocol code)         │
│  ┌──────────────────────────┐                  ┌────────────────────────────┐  │
│  │ irori-int-mqtt  [default]│── Integration ──►│ integration host           │  │
│  │  HA discovery, commands, │     trait        │  lifecycle, health, config │  │
│  │  optional embedded broker│                  ├────────────────────────────┤  │
│  ├──────────────────────────┤                  │ registry · state · events  │  │
│  │ irori-int-demo  [default]│─────────────────►│ context propagation        │  │
│  │  virtual devices         │                  ├────────────────────────────┤  │
│  ├──────────────────────────┤                  │ irori-rules (deterministic)│  │
│  │ irori-int-zigbee/matter… │ (later, opt-in)  │ irori-recorder (SQLite)    │  │
│  └──────────────────────────┘                  └─────────────┬──────────────┘  │
│                                                              │                 │
│                                                ┌─────────────▼──────────────┐  │
│  config dir (source of truth, git-friendly)    │ irori-api  WS + HTTP, auth │  │
│  irori.toml · rules/*.json · integrations/*    │ /metrics · embedded UI     │  │
│                                                └─────────────┬──────────────┘  │
│  irori-assist (AI) — opt-in feature, NOT in barebones build  │                 │
└──────────────────────────────────────────────────────────────┼─────────────────┘
                                                               │ same public contract
                         ┌──────────────────────┬──────────────┴───────┬───────────────────┐
                         ▼                      ▼                      ▼                   ▼
                  Browser: irori-ui      irori CLI (full         EXTERNAL integrations   Scripts
                  Devices · Automations  UI parity, --json)      (separate processes,    (irori-client
                  Integrations · Settings                        any language: ESPHome,  or raw WS)
                                                                 Z-Wave, your own…)
```

**The one idea to hold onto:** the core exposes a single integration contract. A built-in integration implements it as a Rust trait in-process; an external integration implements the *same* contract as messages over the WS API. The MQTT integration is the proof that the contract works.

### 2.1 Workspace layout

```
irori_os/
  Cargo.toml                 # workspace
  crates/
    irori-types/             # entities, typed state, registry, rules, traces, API messages
                             # serde + schemars; compiles to native AND wasm32
    irori-core/              # registry, state store, event bus, context, integration host
    irori-integration/       # THE integration SDK: Integration trait, manifest, config schema,
                             # entity/service registration types, external-process protocol
    irori-rules/             # rules engine: pure, deterministic, Clock + StateView traits
    irori-recorder/          # rusqlite (bundled), dedicated writer thread, retention
    irori-config/            # load/validate/watch the plain-text config dir (hot reload)
    irori-api/               # axum WS/HTTP, auth, /metrics, static UI serving
    irori-client/            # typed Rust client for the public API (CLI, external integrations, assist)
    irori-ui/                # Leptos CSR app (built to wasm, embedded via rust-embed)
    irori/                   # binary: CLI + wiring + embedded assets; cargo features pick integrations
  integrations/
    irori-int-mqtt/          # rumqttc, HA discovery → registry, command publishing, optional broker
    irori-int-demo/          # virtual lights/sensors/switches; the reference integration to copy
  extras/
    irori-assist/            # AI: LLM providers, rule authoring, explainer, dashboards (opt-in)
  examples/
    external-integration-py/ # ~100-line external integration in Python over WS (proves "any language")
  schemas/                   # generated JSON Schemas (checked in, CI verifies fresh)
  docs/specs/                # entity model, rule schema, trace format, API protocol, auth, integrations
  fixtures/                  # recorded Z2M/Tasmota discovery payloads, sample homes
  benches/                   # performance budget benchmarks (§4.3)
  install/                   # install.sh, systemd unit template
```

**Dependency rules (enforce in CI):**
- `irori-core` and `irori-rules` depend on **no** integration crate and no protocol library (no `rumqttc` in the core's dependency tree).
- Integrations depend only on `irori-integration` and `irori-types`.
- `irori-assist` and external tools depend only on `irori-types` and `irori-client`.

**Cargo features on the `irori` binary:** `default = ["int-mqtt", "int-demo", "ui"]`; opt-in: `assist`, and future `int-zigbee`, `int-matter`, …. `--no-default-features` must still build, start, and serve the API. That's the "robust at its smallest" test.

### 2.2 Tech stack

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust (stable) | |
| Async runtime | `tokio` | |
| HTTP / WebSocket | `axum` | |
| MQTT client | `rumqttc` | |
| Embedded broker (optional) | `rumqttd` | Evaluate maturity in Phase 0; fall back to "requires external broker" |
| Persistence | `rusqlite` with `bundled` | Sync API on a dedicated writer thread; simpler than `sqlx` and musl-friendly. WAL mode. |
| Serialization / schema | `serde` + `schemars` | JSON Schema generated from Rust types |
| Expression language | **CEL** via `cel-interpreter` (candidate) | Non-Turing-complete, typed, well known to LLMs. Spike vs a tiny custom language in Phase 0. |
| Time / time zones | `jiff` | DST-correct scheduling; DST bugs are classic automation failures |
| Sun position | `sunrise` (or similar) | sunrise/sunset/elevation triggers |
| Password hashing | `argon2` | |
| Frontend | Leptos (CSR) → wasm, `trunk` build | Confirm vs Dioxus in Phase 0 |
| Graph layout (visualizer) | Hand-rolled layered layout → SVG | Rule graphs are near-trees; fall back to JS interop (e.g. elkjs) if needed |
| Asset embedding | `rust-embed` | Serve precompressed (brotli) assets |
| Cross-compilation | `cargo-zigbuild`, musl targets | x86_64, aarch64 required; armv7 best-effort |
| LLM access | Thin provider trait over HTTP (`reqwest`) | Anthropic, OpenAI-compatible, Ollama |
| CLI | `clap` (+ `clap_complete`) | Shell completions; `--json` on every read command |
| Config | `toml` + `serde`, `notify` for hot reload | Plain-text config dir is the source of truth (D18) |
| Logs / metrics | `tracing` (JSON or pretty output), Prometheus text at `/metrics` | |
| Allocator (evaluate) | system vs `mimalloc` | Measure against budgets; musl's default allocator is slow |
| Benchmarks | `criterion` + a load-generator binary | Budgets in §4.3 |
| Matter (future integration) | `rs-matter` | |

---

## 3. Phase 0 — Foundations and specs (≈3–5 weeks)

Goal: the decisions that are expensive to change later are written down and prototyped. **Little product code, lots of leverage.**

### M0.1 Workspace and toolchain
- Cargo workspace with the crates above as empty shells; `fmt`, `clippy -D warnings`, `test` in CI.
- Hello-world binary that embeds a static page and opens a bundled SQLite DB, **cross-compiled to aarch64-musl and run on a Raspberry Pi** (or QEMU).
- Dependency-rule check (§2.1) in CI (e.g. `cargo-deny` bans or a small script over `cargo metadata`).
- **Demo:** `scp` the binary to a Pi, run it, open the page.

### M0.2 Spec: entity and registry model → `docs/specs/entities.md`
- `Device { id, name, manufacturer, model, area_id, via (bridge) }`
- `Area { id, name, floor }`
- `Entity { id: "light.hallway", device_id, area_id?, kind, state: TypedState, attributes: Map, last_changed, last_updated, context }`
- `TypedState` is an enum per kind, e.g. `Light { on, brightness?, color_temp?, rgb? }`, `Sensor { value: f64 | string, unit, device_class }`, `BinarySensor { on, device_class }`, plus `Unavailable`/`Unknown`.
- `Context { id, parent_id?, origin: Device | User(id) | Rule { rule_id, run_id } | Api(token_id) }`.
- v1 kinds: `light`, `switch`, `sensor`, `binary_sensor`. Next: `cover`, `climate`, `button`/`event`, `lock`.

### M0.3 Spec: rule schema + expression language → `docs/specs/rules.md`
Draft shape (to be finalized):

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
- **Triggers:** state (with `for`), time (cron-like and `at`), sun (with offset), event (including events emitted by integrations, e.g. raw MQTT messages from the MQTT integration; the core stays protocol-agnostic), startup.
- **Conditions:** state, expr, time window, sun position; `all`/`any`/`not` combinators.
- **Actions:** call service, delay, wait (state/expr with timeout), `if`/`choose`, set variable, fire event, stop.
- **Modes:** single, restart, queued(max), parallel(max).
- **Expression language:** CEL vs custom. Expressions are type-checked at save time against the registry (entity exists, kind supports the attribute).
- **Node paths:** every node has a stable path (`triggers/0`, `actions/2/then/0`) used by traces and the visualizer.
- **Versioning:** a rule version = content hash; every save creates a version; traces reference a version.
- **Validation layers:** (1) JSON Schema, (2) semantic validation vs registry and services, (3) optional dry-run/backtest (Phase 2).

### M0.4 Spec: trace format → `docs/specs/traces.md`
- `Run { run_id, rule_id, rule_version, trigger: { node_path, event, context }, started_at, finished_at, outcome: Completed | ConditionFailed(path) | Aborted | Error | Superseded }`
- `Step { node_path, started_at, finished_at, reads: [{ entity, value }], result, outputs, error? }`
- Retention: last N runs per rule (default 50) + max age.

### M0.5 Spec: API protocol and auth → `docs/specs/api.md`
- WS message envelope `{ id, type, ... }`, modeled on HA's WS API for familiarity. Message types: `auth`, `subscribe_events`, `unsubscribe`, `get_states`, `get_registry`, `call_service`, `rules/list|get|save|delete|validate`, `traces/list|get`, `history/query`.
- HTTP: health, static UI, token-authenticated REST mirrors of read endpoints.
- Auth: first run prints a **one-time setup code** to stdout/log (so whoever reaches the wizard first on the LAN can't claim the instance). The wizard creates the owner account. Access tokens are for API, CLI, and external integrations (integration tokens are scoped to what their manifest declares).

### M0.6 Spec: integration contract → `docs/specs/integrations.md` (the most important spec for modularity)
- **Manifest:** `{ id, name, version, kind: built-in | external, config_schema (JSON Schema), provides: { entity_kinds, services, events }, requires: { network?, serial_ports?, … } }`.
- **Lifecycle:** `setup(config) → start → running ⇄ degraded → stop`; health reporting with a reason; restart with backoff supervised by the core. A failing integration **never** takes down the core or other integrations.
- **What an integration can do:** register/update/remove devices and entities; push state updates; register service handlers for the kinds it provides; emit events; log through the core (logs tagged by integration); persist small private key/value data.
- **What it cannot do:** read other integrations' internals, touch rules, write to the recorder directly.
- **Rust trait (built-in)**, roughly:
  ```rust
  #[async_trait]
  pub trait Integration: Send + Sync + 'static {
      fn manifest(&self) -> Manifest;
      async fn setup(&mut self, ctx: IntegrationContext, config: serde_json::Value) -> Result<()>;
      async fn run(&mut self, shutdown: ShutdownSignal) -> Result<()>;
      async fn handle_service(&self, call: ServiceCall) -> Result<ServiceResponse>;
  }
  ```
  `IntegrationContext` is a handle offering exactly the "can do" list above.
- **External protocol:** the same operations as WS messages (`integration/hello` with manifest + token → `integration/configured` → `device/upsert`, `state/push`, `service/handle` requests from the core, `health`). A built-in integration and an external one must be indistinguishable from the UI and CLI.
- **Config:** each integration's config lives in `integrations/<id>.toml`, validated against its `config_schema`. The UI renders a settings form from the schema and the CLI validates it. Integration authors never write UI code.
- **Reference implementations:** `irori-int-demo` (virtual devices, ~300 lines, the template to copy) and `examples/external-integration-py` (the same virtual devices, external, in Python).

### M0.7 Spec: config dir and CLI → `docs/specs/config-cli.md`
- Config dir layout: `irori.toml` (server, location, recorder retention, enabled integrations), `rules/<id>.json`, `integrations/<id>.toml`, `areas.toml`. Secrets go in a separate file (e.g. `secrets.toml`) that is git-ignorable. Runtime data (SQLite DB) lives in a separate data dir.
- Hot reload: file changes are validated, then applied atomically; invalid files are rejected with a clear error and the last good version stays active.
- UI edits write the same files (humans and the UI share one source of truth).
- CLI command tree (UI parity): see M1.7.

### M0.8 Spikes (timeboxed, one or two evenings each)
- CEL in Rust: evaluate typed expressions against a fake state view; check error messages are LLM-actionable. Measure eval cost (target: µs per expression).
- Leptos vs Dioxus: CSR app sharing `irori-types`, embedded in the binary; measure wasm size (target < 500 KB brotli for the barebones UI).
- `rumqttd` embedded: start in-process, connect Zigbee2MQTT to it.
- Integration trait vs external protocol: implement the demo integration both ways against a stub core, to prove the contract really is the same.
- Baseline measurement: empty `irori` binary on a Pi 4 (RSS, startup, size) to calibrate the budgets in §4.3.

**Phase 0 exit:** specs merged; JSON Schemas generated from `irori-types` (rules, manifests, config); a folder of golden example rules and configs (valid and invalid) with a test that validates them; spike results recorded in the decision log; performance budgets set from real baseline numbers.

---

## 4. Phase 1 — Core MVP (≈4–6 months)

Goal: a **barebones, fast, modular core** that a real home runs on for weeks without drama. The Phase 1 deliverable *is* the barebones release: core + CLI + minimal UI + MQTT and Demo integrations.

### M1.1 Registry, state store, event bus, integration host (≈3–4 wks)
- In-memory registry and state store; `tokio::sync::broadcast` event bus with typed events (`StateChanged`, `ServiceCalled`, `RuleRun*`, `RegistryUpdated`, `IntegrationStatus`).
- Context propagation through every state change and service call.
- Service registry: integrations register handlers for the kinds they provide.
- **Integration host:** loads built-in integrations enabled in `irori.toml`, supervises them (panic/crash isolation, restart with backoff, health status).
- `irori-int-demo` built against the `Integration` trait: virtual light, switch, motion sensor, temperature sensor.
- Config dir loading and hot reload (`irori-config`).
- **Demo:** `irori serve` with only the demo integration; `irori` CLI lists devices, toggles a virtual light, and watches events with contexts. Kill the demo integration's task and watch the core restart it.

### M1.2 MQTT integration + HA Discovery (≈3–4 wks)
- Implemented **only** through the `Integration` trait. If the trait can't express something MQTT needs, fix the trait, never special-case the core.
- Connect to an external broker (URL, credentials, TLS). Optional embedded broker (config flag).
- Subscribe to `homeassistant/+/+/config` and `homeassistant/+/+/+/config` (retained) → create devices and entities.
- Support `light` (JSON schema, as used by Z2M, plus the default schema), `switch`, `sensor`, `binary_sensor`; availability topics; value templates only in the simple forms Z2M/Tasmota actually emit (JSON path), not full Jinja.
- Service calls publish to the command topics given in discovery.
- Test fixtures: recorded discovery payloads from Z2M, Tasmota, ESPHome.
- **Demo — shadow mode (§4.2):** Irori connected to the broker your HA + Z2M already use; all Zigbee devices and their entities appear with live state; toggling a light from Irori works.

### M1.3 Recorder (≈2 wks)
- SQLite in WAL mode; dedicated writer thread; batch commits (e.g. every 1s or 500 rows).
- Tables: `states`, `events`, `rule_versions` (snapshots of each rule file version the engine loaded), `rule_runs`, `rule_steps`, `users`, `tokens`, `registry_*`, `integration_kv`. Authored config (rules, integration settings) stays in the config dir (D18).
- Retention/purge job (default 10 days of states; configurable excludes for chatty sensors) to limit SD card wear.
- History query API (entity, time range, downsampling for numeric sensors).
- **Demo:** 48h of real home history queried and plotted as text or CSV.

### M1.4 Rules engine (≈5–7 wks — the heart; don't rush it)
- `irori-rules` is pure: `trait Clock`, `trait StateView`, `trait ServiceCaller`; a runtime adapter wires them to the real core.
- Implements the Phase 0 spec: triggers, conditions, actions, modes, waits with timeouts, `for` durations, expressions.
- Emits traces for every run, persisted by the recorder.
- Semantic validation on save (entities exist, services exist, expressions type-check).
- **Test strategy:** golden tests with a simulated clock (e.g. "motion at 22:00:00, no motion at 22:00:30 → off at 22:02:30"); DST transition tests; property tests for expression evaluation; mode tests (restart/queued under bursts).
- **Demo:** the hallway motion rule runs on real devices; the raw trace JSON shows every read and decision.

### M1.5 API, auth, external integrations (≈3–4 wks)
- WS and HTTP per spec; owner account; access tokens; `irori-client` crate with typed calls.
- `/metrics` (Prometheus text): event throughput, rule runs, integration health, recorder queue depth, memory.
- **External integration protocol** (D16): handshake, scoped tokens, device/state/service messages. Run external integrations from config (`integrations/<id>.toml` with `command = "/path/to/binary"`); the core spawns and supervises them. The install-from-registry flow comes in Phase 3.
- **Demo:** the Python example integration's virtual devices appear next to the MQTT devices, indistinguishable in the CLI and API; a rule uses one of each.

### M1.6 Barebones embedded UI (≈3–4 wks)
Deliberately minimal: fast to load, no dashboards, no charts beyond the basics. Four sections:
- **Setup wizard (first run):** setup code → owner account → enable integrations (MQTT: detect `localhost:1883` or use the embedded broker) → location and time zone.
- **Devices:** devices grouped by area/integration → entities with live state; toggle or set values; basic entity history (last 24h list or sparkline); rename, assign area.
- **Automations:** rule list with enabled toggle and last run outcome; JSON editor with live schema and semantic validation inline (same validation code as the server, via wasm); trace list per rule as a raw step table (the visualizer comes in Phase 2).
- **Integrations:** installed/enabled integrations (built-in and external) with status, health, and logs; enable/disable; settings form auto-generated from each integration's `config_schema`.
- **Settings:** access tokens, location/time zone, recorder retention, system info (version, uptime, memory, DB size).
- Budget: UI bundle within §4.3; first load on a Pi-served LAN < 1 s.
- **Demo:** the full loop from a fresh install in the browser: enable MQTT → device appears → write rule → it fires → trace visible.

### M1.7 CLI, packaging, release (≈2–3 wks)
CLI has **full parity with the UI**; every read command supports `--json`; shell completions:
```
irori serve [--config DIR] [--data DIR]
irori status                                  # core health, integrations, metrics summary
irori devices list|show <id>
irori entities list|get <id>|watch [<id>…]    # live stream
irori call <service> [--target <entity>] [--data k=v…]
irori rules list|show|validate <file>|apply <file>|enable|disable|delete
irori traces list <rule>|show <run_id>
irori integrations list|show|enable|disable|config validate|logs <id>
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

### 4.2 Shadow mode (the validation strategy for core-first)
Irori connects to the same MQTT broker as an existing HA + Zigbee2MQTT setup. Retained discovery messages give it every device without touching HA. Run a few low-stakes rules on Irori (e.g. a closet light) while HA keeps doing everything else. This is the dogfooding path for the author, and later the "try it without migrating" path for users.

**Watch out:** two controllers commanding the same device can fight. Document "assign each device's automations to one system".

### 4.3 Performance budgets ("lightning fast, lightweight")
Initial targets, to be recalibrated from the Phase 0 baseline. Measured in CI (benchmarks and a load generator) and on a real Raspberry Pi 4 before each release. A regression over budget fails the build.

| Metric (default build, Pi 4) | Budget |
|---|---|
| Binary size (with UI) | < 15 MB |
| Barebones UI bundle (wasm + assets, brotli) | < 500 KB |
| Cold start to API ready | < 200 ms (excluding broker connect) |
| Idle RSS, empty home | < 15 MB |
| RSS with 1,000 entities + 10 days history + 100 rules | < 50 MB |
| Internal latency: state change → rule evaluated → service call dispatched | p99 < 5 ms |
| Sustained throughput without backlog | ≥ 2,000 state changes/s |
| Recorder writes | batched; zero per-event fsync |

**"Robust at its smallest" checks:** `--no-default-features` builds and runs; the core survives integration crashes, a broker outage (reconnect with backoff), a full disk (recorder degrades, rules keep running), and invalid config files (rejected, last good config stays active).

### Phase 1 exit criteria
- Runs 30 consecutive days in the author's home in shadow mode with ≥ 5 real rules, no crashes, no missed triggers found.
- All §4.3 budgets met on a Pi 4.
- Adding a new protocol required **zero** changes to `irori-core`: proven by MQTT, Demo, and the Python external integration.
- Everything is doable from the UI *and* from the CLI; the config dir round-trips through git.
- A rule can be written, validated, saved, run, and debugged (raw trace) entirely from the barebones UI.

---

## 5. Phase 2a — Automation visualizer and debugger (≈2–3 months)

Goal: the first thing that makes someone say "HA can't do that". Public alpha candidate.

- **Graph view:** render a rule version as a left-to-right graph (triggers → conditions → action sequence with branches and waits). Layered layout, SVG, in `irori-ui`.
- **Trace playback:** pick a run → highlight the executed path; failed condition in red; click a node to see values read, result, and timing; step forward and back.
- **Run timeline:** per rule, runs over time with outcomes; filter "condition failed at node X".
- **Version diff:** graph and JSON diff between rule versions; traces always render against the version they ran.
- **Backtest / replay:** "run this rule (or this draft) against the last N days of recorded history" using the deterministic engine with a replay `Clock` and `StateView`; output = simulated runs and traces. Clearly label limits: replay can't know how devices *would* have reacted to simulated actions.
- **Visual editing (stretch):** edit simple rules from the graph; JSON stays the source of truth.
- **Demo:** the "day with Irori" scenario from INSPIRATION.md §4, minus the AI explanation.

## 6. Phase 2b/2c — AI layer (≈3–4 months)

Everything in this section is **opt-in** (the `assist` cargo feature or a separate `irori-assist` process). The barebones build never includes it, and the core never depends on it (D13, D17). In the UI, AI features appear only when enabled.

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

## 7. Phase 2d — HA backend adapter (from the original plan)

Once the visualizer and AI features work on Irori, make them work against an existing HA instance via its WS API. This reaches HA's user base without asking anyone to migrate.

- Client-side `Backend` abstraction (Irori or HA) in `irori-client`; design the trait in Phase 1 even if only Irori implements it.
- HA's automation config and trace JSON map onto Irori's rule and trace types (lossy where Jinja is involved; show those nodes as opaque).
- Decide the packaging then: an Irori binary in "HA companion mode", or an HA add-on.

---

## 8. Phase 3 — Integration ecosystem and users (≈4–6 months)

The integration *contract* already exists and is proven in Phase 1 (M0.6, M1.1, M1.5). Phase 3 makes integrations **easy to install and share**, and adds the first new protocols.

- **`irori integrations install <name|url|path>`:** fetch a signed external integration from a registry index (a simple git-hosted index is enough to start), verify it, write `integrations/<id>.toml`, spawn it. Plus `update`, `remove`, and a matching "Install" button in the UI's Integrations section.
- **Integration SDKs and templates:** `cargo generate` template for Rust (built-in or external), a published Python package for the external protocol, and "write your first integration in 30 minutes" docs.
- **Permissions:** the manifest's `requires` (network, serial ports, …) is shown to the user on install; integration tokens are scoped to the manifest's `provides`.
- **Promotion path:** popular external integrations can become built-in (compiled in, opt-in cargo feature) without changing their behavior, since it's the same contract.
- **First new protocol integrations (in order of value for tinkerers):**
  1. **ESPHome native API** (ESPHome's default transport, not MQTT)
  2. **Z-Wave** via `zwave-js-server` (external)
  3. **Matter** via `rs-matter` (built-in, opt-in feature)
  4. Native Zigbee: low priority while Zigbee2MQTT covers it.
- **Multi-user:** users, roles (owner, admin, member, guest), per-user default dashboards, per-area/per-entity permissions enforced in the API layer; audit log via contexts.
- **HA automation importer:** HA automation YAML → Irori rules, with a report of unconvertible parts (templates).
- **Backup and restore:** single-file snapshots; scheduled backups.

## 9. Phase 4 — Product and public launch

- Public alpha (target: after Phase 2a), then beta (after 2b).
- **License decision (D7)** before the repo goes public.
- Docs site; `irori.dev` (check domain and trademark availability early, even in Phase 0).
- Hosted AI tier as an `LlmProvider` backed by an Irori service (usage credits).
- Remote access (e.g. an optional relay) and off-site backups as paid conveniences.
- Decide on an appliance image vs "binary on your Linux box" based on alpha user feedback.

### Lightweight validation along the way (compensates for D1)
- Publish the rule schema spec and a short write-up after Phase 0; ask for feedback in r/homeassistant, r/homeautomation, and the HA forums.
- Short dev-log posts or videos at M1.2 (shadow mode), M1.4 (traces), and Phase 2a (visualizer).
- Track: people who ask to try it, GitHub stars/watchers, and inbound "does it support X?" requests (they tell you which integration to build next).

---

## 10. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Solo-project scope creep / burnout | Project stalls | Demo-gated milestones; barebones scope for Phase 1; resist new integrations and polish before Phase 2a ships |
| Integration contract designed wrong | Every future protocol is painful; core changes leak in | Build MQTT, Demo, and an external Python integration against it in Phase 1; rule: fix the contract, never special-case the core |
| A built-in integration crashes or blocks the core | Robustness promise broken | Per-integration task supervision, panic isolation, bounded channels; third-party code runs only as external processes |
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
| License ambiguity blocks contributors | Can't accept PRs | D7 deadline |

---

## 11. Open questions

1. ~~Rule storage~~ → decided: plain-text config dir is the source of truth (D18).
2. **Expression language:** CEL vs a tiny custom language (decide in M0.8).
3. **Leptos vs Dioxus** (decide in M0.8).
4. **Embedded broker default:** off (external broker) or on when none is detected?
5. **Where AI features run when enabled:** `assist` cargo feature in the same binary vs a separate `irori-assist` process. Both are allowed by D13; pick a default in Phase 2.
6. **External integration transport:** WS over TCP only, or also a Unix socket for local integrations (faster, no port)? Decide in M0.6.
7. **Built-in integration loading:** compile-time only (cargo features), or also dynamic loading (`.so`/WASM components) later? *Lean: compile-time + external processes only; WASM components are an interesting Phase 3+ experiment for sandboxed in-process integrations.*
8. **Config format details:** TOML for everything, or JSON for rules (current) and TOML for the rest? JSON rules match the schema and LLM output; TOML reads nicer by hand.
9. **Monetization mechanism:** hosted AI credits vs remote access vs a hosted instance.
10. **Appliance image:** yes or no (defer to Phase 4 with user data).
11. **License** (D7 deadline).
