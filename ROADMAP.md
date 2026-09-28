# Irori — Roadmap

What's built and what isn't. Rows aren't in order: anything can be picked up once its dependencies are ✅.

**Status:** ✅ done · 🟠 in progress / partial · - not started

Vision: [INSPIRATION.md](INSPIRATION.md).

## Core

| ID | Feature | Description | Status | Depends on |
|----|---------|-------------|:------:|------------|
| C1 | Workspace & toolchain | Cargo workspace, CI, lints, xtask | ✅ | |
| C2 | Entity & registry spec | Entities, devices, areas, kinds (`docs/specs/entities.md`) | ✅ | |
| C3 | Extension spec | Manifest + protocol contract (`docs/specs/extensions.md`) | ✅ | |
| C4 | Config dir | `irori.toml`, per-extension config, hot reload (`docs/specs/config.md`) | ✅ | |
| C5 | Registry, state & event bus | In-memory registry/state, typed events, service calls with contexts | ✅ | C2 |
| C6 | Extension host | Load, supervise and restart extensions, crash isolation | ✅ | C3, C5 |
| C7 | Extension install/uninstall | Official packages from `official.toml`, install by URL, separate release packages | ✅ | C6 |
| C8 | Install & packaging | `install.sh`, release packaging, port fallback | ✅ | C1 |
| C9 | Rules spec & validation | Rule format, CEL, 3-layer validation as a library | ✅ | C2 |
| C10 | Core log window | In-memory log of the core and each extension, shown in Settings | ✅ | C6 |
| C11 | UI: Devices | Devices by area/floor, live state, toggle, brightness & colour, add-device flow | ✅ | C5 |
| C12 | UI: Start & Settings | Start screen, Settings, restart button | ✅ | C11 |
| C13 | UI: Extensions page | Browse, install, uninstall official extensions | ✅ | C7 |
| C14 | UI: Setup wizard | First run: owner account, extensions, location/time zone | - | C16 |
| C15 | Trace spec | Trace format for rule runs (`docs/specs/traces.md`) | - | C9 |
| C16 | API spec & auth | Public API protocol, tokens, auth (`docs/specs/api.md`) | - | |
| C17 | Public API | Real API replacing `/api/dev/*`, push instead of polling, external extensions | - | C16 |
| C18 | Recorder | History storage; registry survives restarts | - | C5 |
| C19 | Rules engine runtime | Run rules, emit traces | - | C9, C15 |
| C20 | UI: Automations | Rule list, JSON editor with inline validation, trace table | - | C19 |
| C21 | UI: Entity history | Last-24h list / sparkline per entity | - | C18 |
| C22 | CLI command tree | Full UI parity (`devices`, `rules`, `extensions`, `token`…) with `--json` | 🟠 | C17 |
| C23 | Firmware updates | `update` entity kind, update from UI | - | C5 |
| C24 | Performance budgets in CI | Binary size, RSS, latency, throughput checks (only UI bundle size today) | 🟠 | C1 |
| C25 | JSON Schemas & golden examples | Schemas generated from `irori-types`, valid/invalid example tests | - | C2, C3, C4, C9 |
| C26 | Extension signing & permissions | Signature verification, approve permissions on install/update | - | C7 |
| C27 | Automation visualizer | Rule graph, trace playback, run timeline, version diff, backtest | - | C18, C19 |
| C28 | Multi-user | Users, roles, per-area permissions, audit log | - | C17 |
| C29 | Backup & restore | Single-file snapshots, scheduled backups | - | C18 |

## Extensions

| ID | Feature | Description | Status | Depends on |
|----|---------|-------------|:------:|------------|
| X1 | Demo | Virtual lights, switches and sensors | ✅ | C6 |
| X2 | ESPHome | Native API, discovery, plaintext and encrypted | ✅ | C6 |
| X3 | HA Discovery library | `irori-ha-discovery`: HA MQTT Discovery parsing, shared by MQTT and Zigbee | ✅ | C2 |
| X4 | Zigbee | Self-installed Zigbee2MQTT, embedded broker, permit-join | ✅ | C6, X3 |
| X5 | MQTT | Generic MQTT + HA Discovery against an external broker | - | C6, X3 |
| X6 | Floorplan | Walls, floors, rooms, drag devices into rooms — built in the UI, to move out into an extension | 🟠 | C11 |
| X7 | Shadow mode | Run alongside HA on the same devices to validate Irori | - | X5 |
| X8 | Extension SDKs & templates | `cargo generate` templates, Python package, docs | - | C17 |
| X9 | App contributions | Extensions serving pages under `/apps/<id>/` (config editor, log viewer, terminal) | - | C17, C26 |
| X10 | AI providers | `irori-assist`: Anthropic, OpenAI-compatible, Ollama | - | |
| X11 | AI rule authoring | Natural language → validated rule; "why did/didn't this fire?" | - | X10, C19, C27 |
| X12 | Dashboards & cards | Sandboxed HTML dashboards with `irori.js` bridge, installable as extensions | - | C17 |
| X13 | AI dashboards | Generate dashboards from a prompt | - | X10, X12 |
| X14 | HA backend adapter | Visualizer and AI features against an existing HA instance | - | C27 |
| X15 | HA automation importer | HA YAML → Irori rules, with a report of what didn't convert | - | C19 |
| X16 | Z-Wave | Via `zwave-js-server` | - | C17 |
| X17 | Matter | Via `rs-matter` | - | C6 |
| X18 | Vendor cloud connector | e.g. SwitchBot: proves cloud polling/push and secrets | - | C6 |
