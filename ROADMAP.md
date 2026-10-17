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
| C7 | Extension install/uninstall | Official packages from `official.toml`, install by URL, separate release packages, update an installed one in place | ✅ | C6 |
| C8 | Install & packaging | `install.sh`, release packaging, port fallback | ✅ | C1 |
| C9 | Rules spec & validation | Rule format, CEL, 3-layer validation as a library | ✅ | C2 |
| C10 | Core log window | In-memory log of the core and each extension, shown in Settings | ✅ | C6 |
| C11 | UI: Devices | Devices and entities as compact tables by area/floor, live state, a control per kind, add-device flow, removing a device unpairs it | ✅ | C5 |
| C12 | UI: Start & Settings | Start screen, Settings, restart button, motion with a switch to turn it off (`docs/motion_design.md`) | ✅ | C11 |
| C13 | UI: Extensions page | Browse, install, uninstall official extensions | ✅ | C7 |
| C14 | UI: First-run setup | A welcome that asks who you are and where the home is; a first run works without it (`docs/specs/config.md` §2), so it is a prompt, not a gate. Users in Settings: owner or user, each with a password, and a sign-in that never lists who lives here (`users.toml`); https with `--tls`. Location and time zone on a map (`home.toml`), which is what arms `time` and `sun` triggers in Automations | ✅ | |
| C16 | API spec & auth | Public API protocol, tokens and scopes for programs that aren't the page (`docs/specs/api.md`). The page's own sign-in is C14 | ✅ | |
| C17 | Public API | HTTP at `/api/…`, scoped tokens, `/api/ws` push, inbound extensions on `/api/extension` | ✅ | C16 |
| C18 | Recorder | Entity history in `irori.db` that survives a restart. The page shows the last day; the file keeps detailed history for `[recorder] retain_days` (default 10) and hourly summaries afterwards. Both are set from the History row in Settings | ✅ | C5 |
| C21 | UI: Entity history | Last-24h list, chart and state timeline per entity, read from the recorder | ✅ | C18 |
| C22 | CLI command tree | Full UI parity (`devices`, `rules`, `extensions`, `token`…) with `--json`, plus `irori uninstall` for what `install.sh` put down (`docs/specs/cli.md`). Drawing, the map search, and the flow canvas stay on the page | ✅ | C17 |
| C23 | Updates | The `update` kind: firmware for Zigbee, ESPHome and MQTT devices, and Irori's own extensions, in one list on the Devices page. Install asks first, a version can be skipped, installs on one protocol go one at a time, and a failed one says why. Irori says when a newer release of itself is out (`[updates] check`), and one installed with `install.sh` replaces itself: checksum checked, the new binary tried first, the old one kept beside it | ✅ | C5, C32 |
| C24 | Performance budgets in CI | Binary size, RSS, latency, throughput checks (only UI bundle size is enforced today: under 5 MB brotli) | 🟠 | C1 |
| C25 | JSON Schemas & golden examples | Schemas generated from `irori-types`, checked fresh in CI, valid/invalid example tests (`fixtures/`) | ✅ | C2, C3, C4, C9 |
| C26 | Extension signing & permissions | Signature verification, approve permissions on install/update. Only the "full access" approval exists; no signing | 🟠 | C7 |
| C28 | Multi-user | Per-area permissions, an audit log, a regular user's own Automations view. Owner and user roles are C14 | - | C17 |
| C29 | Backup & restore | One-file backups of the config and the database, taken while running, sealed with a recovery key, on a schedule or before an update, copied to folders (mounted shares, USB), WebDAV and S3. Restore from the page, the terminal, or the first-run welcome, with parts chosen (history can be left as it is); a backup can be checked without restoring it (`docs/specs/backup.md`) | ✅ | C18 |
| C30 | Engine API for extensions | Engines subscribe to state and registry, read history, call services as `Origin::Automation`; API scopes enforced (`docs/specs/automations.md` §B2) | ✅ | C6 |
| C31 | App pages in the sidebar | Extensions add a sidebar page: static bundle, sandboxed frame, bridge, engine RPC (`docs/specs/automations.md` §B3–B4) | ✅ | C6, C12 |
| C32 | Entity kinds | Every Home Assistant device kind: 29 in (covers, locks, fans, climate, media players, updates, alarm panels, dates and times, vacuums, mowers, scenes, trackers, timers, schedules…), `entity_format` so older engines keep working, unsupported parts of a device listed instead of dropped. Automations read a kind's other values by name (a blind's position), and temperatures are °C throughout. `remote` isn't planned: no protocol here reports one | ✅ | C2 |
| C33 | Helpers | Values Irori keeps itself, built into the core (`extensions/helpers.toml`): toggles, numbers, counters, choices, texts, buttons, dates and times, timers and schedules, and values worked out from other entities (groups, thresholds, trends, meters, integrals, derivatives, statistics, history). Each is an ordinary entity, made from the Helpers tab, the CLI or the file. Any number can be counted up and down. Built-ins read the home in-process within their API scopes | ✅ | C5 |
| C34 | Irori as a device | The machine Irori runs on is a device in its own home, with CPU, memory and disk meters | ✅ | C5 |
| C35 | Dev container & device lab | `dev/pi`: a Raspberry Pi-like container to try a PR; `dev/lab`: emulated ESPHome and Zigbee devices. Matter nodes need binaries that aren't there | 🟠 | C8 |

## Extensions

| ID | Feature | Description | Status | Depends on |
|----|---------|-------------|:------:|------------|
| X1 | Demo | A demo house living a day every minute, room by room: doors, presence, vibration, leak and smoke sensors, a garage door, a smart lock, a video doorbell, lights, a TV, metered plugs, cameras, climate and soil sensors, a thermostat and a water boiler | ✅ | C6 |
| X2 | ESPHome | Native API, discovery, plaintext and encrypted | ✅ | C6 |
| X3 | HA Discovery library | `irori-ha-discovery`: HA MQTT Discovery parsing, shared by MQTT and Zigbee | ✅ | C2 |
| X4 | Zigbee | Self-installed Zigbee2MQTT, embedded broker, permit-join | ✅ | C6, X3 |
| X5 | MQTT | Generic MQTT + HA Discovery against an external broker | ✅ | C6, X3 |
| X6 | Floorplan | Walls, floors, rooms, drag devices into rooms, device glyphs, doors and windows that follow their contact sensor, snapping to midpoints and guides, angles, room colours, the plan as JSON, and an assistant that answers about a floor and draws on the plan being edited. Built into the UI. Still open: create/delete areas from the plan, a wall's length typed, per-device icon and label (#27) | 🟠 | C11 |
| X7 | Shadow mode | Run alongside HA on the same devices to validate Irori | - | X5 |
| X8 | Extension SDKs & templates | `cargo generate` templates, Python package, docs | - | C17 |
| X9 | App contributions | Pages under `/apps/<id>/` beyond the sidebar slot that C31 delivered (config editor, log viewer, terminal) | - | C17, C26, C31 |
| X10 | AI providers | `irori-assist`: Anthropic, OpenAI-compatible, and a local Ollama that Irori installs and manages. Settings, chats for the home, a device, an automation and Settings, and read-only tools are in, plus `propose_helper` and `propose_flow`, which fill a copy the person has open and do not write until Create or Save. No service calls | 🟠 | |
| X11 | AI rule authoring | Natural language → validated flow; "why did/didn't this fire?". Explaining a run is in. Authoring is in for the automation open in the editor, saved or new, with a cloud model: the suggestion lands on the canvas, unsaved, and can be backtested | 🟠 | X10, X19 |
| X12 | Dashboards & cards | Sandboxed HTML dashboards with `irori.js` bridge, installable as extensions | - | C17 |
| X13 | AI dashboards | Generate dashboards from a prompt | - | X10, X12 |
| X14 | HA backend adapter | Visualizer and AI features against an existing HA instance | - | X19 |
| X15 | HA automation importer | HA YAML → Irori flows, with a report of what didn't convert | - | X19 |
| X16 | Z-Wave | Via `zwave-js-server` | - | C17 |
| X17 | Matter | Via `rs-matter` | - | C6 |
| X18 | Vendor cloud connector | e.g. SwitchBot: proves cloud polling/push and secrets | - | C6 |
| X19 | Automations (flows) | Freeform flow engine + canvas editor: live validation, traces on the canvas, near-misses, live runs, test and dry runs, 24h backtest, versions, every entity kind (`docs/specs/flows.md`). Time and sun triggers are in (armed from the home's time zone and location). Still open: a lock or alarm code comes from `secrets.toml` and never sits in the flow file or a trace | 🟠 | C9, C30, C31 |
| X20 | Google Cast | Chromecasts, speakers and speaker groups on the local network, as media players | ✅ | C6, C32 |
