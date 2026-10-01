# Spec: automations as extensions

Status: **accepted** (X19 / C30 / C31). Changes go through a PR that updates this file, the types
in `crates/irori-types` and `crates/irori-protocol`, the generated `schemas/`, and the examples in
`fixtures/types/` together.

This document has two halves:

- **Part A — the pillars.** What an Irori automation engine has to be to beat Home Assistant
  where it's weakest: building complex automations, and finding out why one didn't do what you
  expected. The first-party flow engine ([flows.md](flows.md)) is written against these.
- **Part B — the contract.** What the core offers any automation extension, first- or
  third-party: the wire operations an engine uses to watch the home and act on it, and the
  **app** contribution that puts its own page in the sidebar.

The core does not ship an engine and does not know what a "rule" or a "flow" is (INSPIRATION
tenet 2). An engine is an extension, installed and removed like ESPHome.

---

## Part A — the pillars

### Why HA is hard here

| Pain | What people run into |
|---|---|
| Building anything non-trivial | The visual editor is a long form of nested cards; `choose` inside `if` inside `repeat` stops fitting on a screen, and people fall back to YAML and Jinja templates that nothing checks until 22:04 |
| Knowing why it didn't run | A trace exists only for runs that *started*. "Motion happened and nothing turned on" leaves no trace at all; the answer lives in the logbook, the history graph, and guesswork |
| Knowing why it did something odd | Traces show pass/fail per step, but not always the values that were read, and not on the thing you edited |
| Trying a change safely | The only test is to walk past the sensor again, or to "run actions", which skips triggers and conditions |
| Losing what worked | Saving overwrites; the previous version and its behaviour are gone |

Prior art we borrow from: **Node-RED** (live node status, debug sidebar, inject nodes), **Homey
Advanced Flow** (cards on a canvas, the flow lights up as it runs, test from any card), **n8n**
(each node shows the data it saw in the last execution; re-run one node), **Zapier** (test each
step before publishing, run history with per-step data), **Apple Shortcuts** (steps read as
sentences with tappable values).

### The pillars

Each pillar says what the first-party engine does. A third-party engine is free to do otherwise,
but the contract in Part B is shaped so that it can do all of them.

1. **See the whole automation.** A canvas holds several triggers, branches, parallel paths and
   merges in one picture. A node reads as a sentence with the live value next to it
   ("Hallway lux below 30 — now 42"), not as a form.
2. **Freeform, with semantics you can reason about.** What keeps a canvas from becoming Node-RED
   spaghetti is not the drawing but the rules underneath: graphs are acyclic; one trigger firing
   is one **run**, and every path it takes shares its id; merging parallel paths is explicit (a
   **join**); variables are checked to be set on every path that reads them; an unavailable
   sensor never counts as a value. Where nodes sit on the canvas is not part of the definition.
3. **You can't save a broken automation.** Every edit is checked against the real home — the
   entity exists, it's the right kind, it can do what's asked, the expression type-checks — and
   problems are pinned to the node, in words. A draft with problems is kept but not armed.
   Pickers only offer what fits.
4. **Every run explains itself.** A run's trace is the graph lit up: which wires carried it, what
   each node read (entity, availability, value), which way it went and why, what each call
   answered. It can be played back in time order.
5. **Non-runs are first-class.** When a firing is refused (a run is already going), a `for` timer
   resets, or a watched sensor drops off, that is recorded as a **near-miss** with the reason.
   "Why didn't it fire at 22:04?" is a page: the watched entities' changes around that time,
   near-misses, and runs, on one timeline.
6. **Watch it live.** While a run is going, the canvas shows where it is ("waiting for occupancy
   — 1:12, gives up in 8:48"), and it can be cancelled.
7. **Try it before you trust it.** Fire any trigger by hand, for real or **dry** (calls are
   recorded instead of sent, time fast-forwards, and values can be overridden: "pretend lux is
   20"). **Backtest** a draft against the last day of history to see when it would have run.
8. **Nothing is lost.** Every saved definition is a version; each run points at the version it
   ran and is drawn on that version's graph; versions can be diffed and restored.
9. **Engines are extensions.** Everything above sits on the contract in Part B, which a
   third-party engine gets on the same terms.

**Named, not built yet:** flows calling flows (composition); "automated by" and "why did this
change?" on a device's page, from contexts; AI authoring and explanation (X11); events from
protocols; backtests longer than a day (needs the recorder, C18); loops.

---

## Part B — the contract

### B1. Manifest

An engine declares two contributions (both specified here; [extensions.md](extensions.md) §6):

```toml
[extension]
id = "automations"
name = "Automations"
version = "0.1.0"
irori = ">=0.1.0, <0.2.0"
icon = "icon.svg"

[[contributes.automation]]
run = { command = "bin/irori-ext-automations" }

[[contributes.app]]
label = "Automations"
entry = "app/index.html"

[permissions]
api = ["registry:read", "states:read", "events:read", "services:call", "history:read"]
host_fs = ["$CONFIG/flows"]
```

`[[contributes.automation]]`

| Field | Type | Required | Notes |
|---|---|---|---|
| `run` | `{ command, args }` | external only | How the core starts the engine's process. Same shape as a protocol's |

`[[contributes.app]]`

| Field | Type | Required | Notes |
|---|---|---|---|
| `label` | `Name` | yes | The sidebar entry's text |
| `entry` | package path ending in `.html` | yes | The page, served from the package |
| `placement` | `"sidebar"` | no, default `"sidebar"` | Only value in this version. Kept so a later editable sidebar, or other slots, don't need a new kind |

The sidebar icon is the extension's own `icon`.

Rules for this version:

- At most one `automation` and at most one `app` per extension.
- An extension runs **at most one process**: it contributes a `protocol` or an `automation`, not
  both. (An extension that wants both ships two.)
- An `app` may come alone: a page with no process. It can read what its scopes allow through the
  bridge (§B4) but has no engine to call.

### B2. Wire operations

An engine speaks the same JSON Lines protocol as a protocol extension
([protocols.md](protocols.md)): `hello` first, then tagged messages both ways, `id` matching
requests to replies. It uses none of the protocol operations (describe, report, …) and these
instead:

**Extension → core**

| Message | Scope | Answer (`value`) |
|---|---|---|
| `get_registry { id }` | `registry:read` | `{ entities: [Entity], timezone: bool, location: bool }` |
| `get_states { id }` | `states:read` | `[EntityState]` |
| `get_history { id, entities: [EntityId], since: Timestamp }` | `history:read` | `{ "<entity_id>": [EntityState] }`, oldest first |
| `subscribe { id, states: bool, registry: bool }` | `events:read` | `null`. Afterwards the core pushes the events asked for |
| `call_service { id, entity_id, command, data?, run_id, parent_id? }` | `services:call` | `null`, or an error |
| `app_answer { id, value?, error? }` | — | (answers `app_request`) |

**Core → extension**

| Message | When |
|---|---|
| `answer { id, value?, error? }` | Reply to any request above |
| `state_changed { entity_id, old_state?, new_state }` | After `subscribe { states: true }`, for every state change in the home |
| `registry_changed {}` | After `subscribe { registry: true }`, when an entity or device is added, changed or removed; and after any subscription, when the engine fell too far behind and missed events. A nudge: the engine asks again with `get_registry` and `get_states` |
| `app_request { id, method, params }` | The engine's page asked it something (§B3) |

The core never waits on an engine: pushes queue without limit on the engine's side, so an
engine that awaits an answer while changes pour in still gets it. A request unanswered after 30 s
fails with an error.

A request outside the extension's declared scopes is answered with an error naming the missing
scope, e.g. `this extension didn't ask for services:call`. **Scopes are enforced here**, on every
request.

`call_service` takes what a person's command takes: `command`, one of the entity's kind's
actions (the part of a service name after the dot: `turn_on`, `turn_off`) or `toggle`, and `data`
as that service takes it (a light's `turn_on`: `brightness` 1–255, `color_temp_kelvin`, `rgb`),
resolved against the entity exactly as a command from the UI is ([protocols.md](protocols.md)
§7.1). The core checks `command` against the entity, not the wire, so an engine needs no update
for a kind added after it was built.
People-facing spellings such as `brightness_pct` are the engine's to convert. The core builds the call's context itself:
`Origin::Automation { extension: <this extension>, run_id }`, with `parent_id` if given (the
context of the state change that started the run). An engine can't claim to be another
extension or a person.

Errors of `call_service` begin with a code so an engine can branch without parsing prose:
`unknown_entity:`, `not_supported:`, `not_running:`, `unavailable:`, `failed:`, `timeout:`.

**Files.** An engine keeps its definitions where its permissions say. When `host_fs` names
`$CONFIG` or a path under it, the core sets `IRORI_CONFIG_DIR` for the process to the config
directory. `IRORI_EXTENSION_DATA` (its private state directory) is always set.

### B3. App pages

For each running extension with an `app` contribution, the core:

- lists it at `GET /api/dev/apps`:
  `[{ "extension": "automations", "label": "Automations", "has_icon": true, "api": [scopes], "has_engine": true, "built": true }]`;
- serves the package's files under `/pages/<extension>/…`, with `entry` at `/pages/<extension>/`
  and its sibling files beside it. (`/apps/<extension>/…` is the shell's own address for the page,
  so a reload there reaches the shell.)
  A package whose `entry` file is missing is still listed, with `built: false`, and the shell
  says so on the page instead of showing an empty frame;
- relays `POST /api/dev/apps/<extension>/rpc` with body `{ "method": "…", "params": … }` to the
  engine as `app_request`, and returns `{ "value": … }` or an error status with `{ "error": "…" }`.
  30 s timeout (a backtest can take a moment). `503` if the engine isn't running.

What the core promises about the static files:

- `Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src
  'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'self'`, with
  the server's own address (from the request's `Host`) added beside each `'self'`: in the
  sandboxed frame Safari and Firefox match `'self'` against the opaque origin, i.e. nothing.
- `Access-Control-Allow-Origin: *` **on these files only** — the page runs in a sandboxed frame
  with an opaque origin, so its own script, wasm and stylesheet fetches are cross-origin. Never
  on `/api/…`: until auth exists (C16), a CORS-open API would be open to any web page.
- Paths are package paths (§3 of extensions.md): no `..`, no hidden files.

The **shell** (the core's own UI) adds an entry to the sidebar for each app, after its own
sections, with the extension's icon loaded as an image. `/apps/<extension>/<rest>` shows the page
full-width in:

```html
<iframe sandbox="allow-scripts" src="/pages/<extension>/#/<rest>">
```

`allow-scripts` without `allow-same-origin`: the page can't read the shell's storage or call
`/api/…` as the shell. Everything it needs goes through the bridge.

### B4. The bridge

`postMessage` between the page and the shell. Every message has `irori: 1`.

Page → shell: `{ irori: 1, id, op, args }`. Shell → page: `{ irori: 1, id, value }` or
`{ irori: 1, id, error }`, and unsolicited `{ irori: 1, event, … }`.

| `op` | Needs | Does |
|---|---|---|
| `hello` | — | Answers `{ theme, path }` |
| `rpc` `{ method, params }` | an engine | `POST /api/dev/apps/<id>/rpc` |
| `registry` | `registry:read` | Entities, devices and areas as the shell has them |
| `states` | `states:read` | Current states |
| `history` `{ entity_id }` | `history:read` | The last day of one entity's changes |
| `navigate` `{ path }` | — | Updates the address bar's `/apps/<id>/<path>`, so back/forward and deep links work |
| `log` `"…"` | — | Writes a line to the shell's console, unanswered: how a page reports its own crash |

| `event` | When |
|---|---|
| `theme` `{ theme }` | Light/dark or motion preference changed |
| `path` `{ path }` | Back/forward moved inside the page |

`theme` is `{ dark: bool, reduced_motion: bool, tokens: { "--bg": "…", … } }`: the shell's own
CSS custom properties, so the page looks like the rest of Irori without shipping a copy of its
stylesheet. The page can't use `localStorage` (the sandboxed origin has none); preferences come
from here.

The shell only answers messages whose `event.source` is the page's own frame — an opaque origin
reports its origin as the string `"null"`, so that string proves nothing. It checks the op
against the scopes in `/api/dev/apps`.

### B5. What the core shows

Nothing engine-specific. An engine's status, log and settings appear on the Extensions page like
any other extension's. Its runs, definitions and traces are its own business and its own page's.

A later version can add shared views — every engine's automations in one list, "automated by" on
a device — through a small typed method set every engine answers (`automations.list`,
`automations.for_entity`). Not in this version.

### B6. Entity formats, and what the flows engine needs to support every kind

An engine reads entities and states in the entity format its manifest declares
([extensions.md](extensions.md) §5); Irori leaves out the kinds and fields that came later. An
engine installed before Irori learned a kind keeps working: it doesn't see that kind until it's
updated. The first-party engine declares format 2.

Automations don't really need to know what kind of device something is. They need to know what
kind of **value** it has, and what can be asked of it. Irori is adding every kind of device Home
Assistant knows (`docs/specs/entities.md` §4.4). For the flows engine and its page to support all
of them without changing for each one, it needs the following. Until then, raising its
`entity_format` past a kind it can't handle would hand it entities it can't do anything useful
with.

1. **Read entities by value, not by kind.** Today the engine and its page parse entities with
   `irori-types`' closed enums and refuse what they don't know. They should read what every entity
   has: its id (as text), its value and that value's shape (on/off, number, text, or none), and
   what's known about the value:
   - its allowed values (`options`), its range and step, and its unit;
   - whether every report counts as something happening, even an identical one (an `event`, a
     button press);
   - whether it has a value at all (a `button` doesn't).

   `State::primary()` and `Capabilities::primary_shape()` are the start of this.
2. **Call actions from a list, not a fixed set.** `RuleService` and `CallData` know only the light
   and switch services. A blind's `set_position {position: 0–100}` or a select's
   `select_option {option}` should come from a list of each entity's actions and their settings
   (names, types, ranges, choices), checked when the flow is saved. Today the core checks each
   service's data in `Service::from_data`. Data from a flow is parsed per service, not by its shape.
3. **Trigger on happenings, not just changes.** A state trigger fires when the value changes.
   Entities whose every report counts (remote buttons, doorbells) need a trigger that fires on
   each one, ignoring what the protocol replays on reconnect.
4. **Read more than the main value.** `num(id, 'field')` and `text(id, 'field')`, and `field` on
   state triggers and conditions, for a blind's position or a thermostat's current temperature
   ([rules.md](rules.md) amendment).
5. **Compare units properly.** Temperatures are °C throughout the model; other units are as
   reported, so comparing two numbers in different units is the author's to watch for until
   units are normalised.
6. **Make the editor's choices depend on what each entity offers**: entity pickers by value
   shape and action list, value inputs by shape (a dropdown for options, a slider for a range),
   and the hand-kept lists noted in the plan (`WATCHABLE`, the call node's fixed action list,
   the templates' kind lists).
7. **Skip what it doesn't understand** instead of failing. With 1–6 done, its `entity_format` can
   follow Irori's own.

### B7. Not in this spec

| Topic | Where |
|---|---|
| What a flow is, how it runs, its traces | [flows.md](flows.md) |
| Push instead of polling for pages | C17 |
| Signing, approving permissions at install | C26 |
| Engines receiving bus events from protocols | later; [rules.md](rules.md) open question 3 |
