# Spec: flows — the first-party automation engine

Status: **accepted** (X19). The engine behind the **Automations** extension
(`extensions/automations`). Changes go through a PR that updates this file,
`crates/irori-flow-types`, `crates/irori-flows`, `schemas/flow.schema.json`, and
`fixtures/types/flow/` together.

It's written against the pillars in [automations.md](automations.md) Part A and runs on the
contract in Part B. It reuses the typed building blocks of the sequential engine spec
([rules.md](rules.md)): trigger, condition and wait shapes, services, durations, the CEL
expression surface, and the registry type-check. It does **not** reuse the rule document: a flow
is a graph, not a list.

---

## 1. Key decisions

| # | Decision | Why |
|---|---|---|
| F1 | **A flow is a directed acyclic graph** of nodes and wires, drawn freely on a canvas. | Several triggers, branches, parallel paths and merges fit in one picture (pillar 1). Acyclic keeps every run finite and every trace readable. |
| F2 | **One trigger firing is one run.** Every path it takes carries the same `run_id`. | A trace is one object with one outcome, even when paths fork (pillar 4). |
| F3 | **Mode is per flow**: `single` (default), `restart`, `queued { max }`, `parallel { max }`, max 1–32. | Same as rules.md K7. With several triggers, a door-open firing in `restart` cancels the motion run's wait: one flow, one thing going on. |
| F4 | **Fork is implicit, merge is explicit.** A port with several wires runs them all. Paths that forked meet again only through a `join` (`all` or `first`); the validator warns otherwise. Paths that split at *different ports of one exclusive node* (a gate's yes/no, a wait's matched/timeout, a switch's cases) never both run, so they rejoin freely. | Node-RED's "a node runs once per message that arrives" is what multiplies runs nobody meant to have. |
| F5 | **Node ids are stable names** (`motion`, `dark`), chosen when the node is created. Traces key `(flow_id, version, node_id)`. | Adding a node doesn't renumber the others, unlike tree paths. |
| F6 | **Layout is not the definition.** `layout` (x/y per node), `groups` (branch names) and `enabled` are left out of the version hash; `nodes` keys and `wires` are sorted before hashing. | Dragging a node, or tidying the file, must not fork a flow's history (pillar 8). |
| F7 | **Sans-IO engine.** The engine never awaits, sleeps or reads the clock: it's handed events and "now", and hands back calls to make and trace records. | The same code runs live, in a dry run with a virtual clock, and in a backtest over history (pillar 7), with nothing to mock. rules.md §14's async traits were for a different engine; this one chooses replay first. |
| F8 | **Unavailable fails closed**, as rules.md K5: `num()` of an unavailable sensor is an error, a condition that errors is `no`, a `for` hold resets. | A three-hour-old lux reading must not turn lights on. |
| F9 | **In-flight calls are not cancelled** when a run is superseded or aborted; waits, delays and joins are. | rules.md K8. |
| F10 | **Validation is on the server** (the extension), not in the page. The page shares the flow *types* (`irori-flow-types`, no CEL) and asks the engine to check a draft. | CEL has never been compiled to wasm; the page stays small and the check is the same one that arms the flow. |

---

## 2. The file

`$CONFIG/flows/<id>.json`, one flow per file, filename = `id`. Git-friendly, hand-editable, and the
same file the canvas saves (rules.md D18).

```json
{
  "id": "hallway_motion_light",
  "name": "Hallway motion light",
  "mode": "restart",
  "nodes": {
    "motion": { "type": "trigger", "trigger": { "type": "state", "entity": "binary_sensor.demo_movement_occupancy", "to": true } },
    "dark":   { "type": "gate", "condition": { "type": "expr", "expr": "num('sensor.demo_luminosity_illuminance') < 30" } },
    "on":     { "type": "call", "service": "light.turn_on", "entity": "light.demo_hall_light", "data": { "brightness_pct": 60 } },
    "clear":  { "type": "wait", "until": { "type": "state", "entity": "binary_sensor.demo_mmwave_occupancy", "is": false, "for": "2m" }, "timeout": "10m" },
    "off":    { "type": "call", "service": "light.turn_off", "entity": "light.demo_hall_light" }
  },
  "wires": [
    ["motion", "dark"],
    ["dark:yes", "on"],
    ["on", "clear"],
    ["clear:matched", "off"],
    ["clear:timeout", "off"]
  ],
  "layout": { "motion": [0, 0], "dark": [280, 0], "on": [560, 0], "clear": [840, 0], "off": [1120, 0] }
}
```

### 2.1 Top level

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `id` | `RuleId` slug, 1–64 | yes | | Equals the file stem |
| `name` | `Name` | yes | | |
| `description` | `Description` | no | | |
| `enabled` | bool | no | `true` | Not part of the version |
| `mode` | as rules.md §8 | no | `"single"` | |
| `nodes` | map of node id → node, 1–128 | yes | | Node id: `ObjectId` slug |
| `wires` | array of `[from, to]`, 0–512 | no | `[]` | `from` is `node` or `node:port`; `to` is a node id |
| `layout` | map of node id → `[x, y]` | no | `{}` | Canvas position, finite numbers. Not part of the version |
| `groups` | map of node id → `Name` | no | `{}` | Names given to the canvas's branch backgrounds, by the node each starts at (a branch by its first trigger, a part of one by its first node). Not part of the version |

Unknown fields are rejected everywhere.

### 2.2 Nodes

| `type` | Fields | Output ports | Does |
|---|---|---|---|
| `trigger` | `trigger`: a rules.md §5 trigger (`state`, `time`, `sun`, `event`, `startup`) | `out` | Starts a run. Has no inputs |
| `gate` | `condition`: a rules.md §6 condition (incl. `all`/`any`/`not`) | `yes`, `no` | Checks once, now |
| `switch` | `cases`: 1–16 conditions | `case_1` … `case_N`, `else` | The first case that holds |
| `call` | `service`, `entity`, `data?` (rules.md §7.1, and §2.2.1) | `out`, `error` | Calls a service. A failure goes to `error`; if nothing is wired there, the run ends with `error` |
| `set` | `name`, `expr` | `out` | Works out a run variable, read with `var('name')` or by a later `call`'s settings. The canvas calls it "Calculate" |
| `delay` | `for`: duration | `out` | Waits |
| `wait` | `until`: a rules.md §7.3 matcher, `timeout`: duration | `matched`, `timeout` | Waits for a level to hold (for `for`, if given) |
| `join` | `mode`: `all` \| `first`; `timeout` (required for `all`) | `out`, and `timeout` for `all` | Merges paths of one run |
| `stop` | `reason?` | — | Ends the whole run, `completed` |

`from` without a port means `out`; naming `out` on a node that has no `out` port is an error.

#### 2.2.1 Settings worked out when the call runs

A call names any standard service of any kind (`cover.set_position`, `climate.set_temperature`,
`select.select_option`; [rules.md](rules.md) §"Services a rule may name"), and its `data` is that
service's own fields.

Each number in a call's `data` (a light's `brightness_pct`, a blind's `position`, a thermostat's
`temperature`) is either written down or an object `{ "expr": "…" }`: an expression giving a
number, worked out from the home as it is when the call runs. Typically it reads a calculation
made earlier in the run:

```json
"level": { "type": "set", "name": "level", "expr": "round(clamp(70 - num('sensor.demo_luminosity_illuminance') / 600 * 25, 45, 70))" },
"on":    { "type": "call", "service": "light.turn_on", "entity": "light.demo_hall_light",
           "data": { "brightness_pct": { "expr": "var('level')" } } }
```

- It's checked like any other expression (§4): it must give a number, every `var()` it reads must
  be set on every path to the call, and what it reads is part of what the flow watches.
- The result is brought into the field's own range (1–100 for `brightness_pct`, 0–100 for a
  blind's `position`), and rounded where the field takes whole numbers; a thermostat's
  temperature keeps its half degree. The step's note says so ("brightness_pct 57.5 → 58"), and
  the call detail holds the number sent.
- Only a field that takes a number can be worked out. A mode or an option is written down.
- If it can't be worked out (an unavailable sensor, say), nothing is sent and the call fails:
  `error` if that's wired, the run ends with `error` if not.
- A number written down is stored as it always was, so flows saved before this keep their
  version.

### 2.3 How a run goes

1. A trigger fires (§3). The mode decides whether a run starts; if not, that's a near-miss
   (`dropped`, §6).
2. The run puts one **token** on each wire leaving the trigger's `out`, in wire order.
3. A token arriving at a node runs it. The node picks its port(s); one token goes onto each wire
   from that port. An unwired port ends that token.
4. A node with several incoming wires runs once per token, except a `join`:
   - `first`: the first token of the run passes; later ones end there (the trace says so).
   - `all`: waits until a token has come in on **every** incoming wire, then passes one token.
     `timeout` fires the `timeout` port instead. If every other token of the run has ended and
     the join can never complete, it gives up at once through `timeout` — the trace names the
     path that didn't arrive.
5. The run ends when no token is left: `completed`, or `error` if a token ended at an unwired
   `error` port, or `stopped` by a `stop` node. A run takes at most **256 steps**; the 257th ends
   it with `error` ("too many steps"), a guard against fan-out, since cycles can't exist.
6. `restart` supersedes a run (outcome `superseded`); disabling, editing or removing the flow
   aborts it (`aborted`, with `disabled`, `changed` or `removed`); `cancel` from the page aborts it
   with `cancelled`.

Tokens run in a deterministic order: by time, then by the order they were created.

### 2.4 Variables

`set` writes a run variable; `var('name')` reads it. Every node that reads `var('x')` must be
reached only through paths that set `x` first; otherwise it's a problem on that node. A variable
is set with one type (bool, number or text) everywhere in the flow.

---

## 3. Triggers

Same meaning as rules.md §5, restated for what the engine watches:

- `state`: fires when the entity's typed value changes and matches `from`/`to` (`to` may list
  several values: any one matches), only while available; `for` holds the match for that long
  before firing, resetting if the value changes or the entity becomes unavailable. On a numeric
  sensor, `above`/`below` fire instead when the reading crosses into that range (rules.md §5.1);
  with `for`, it has to stay in the range, and moving about inside it doesn't reset the timer.
- `startup`: fires once when the flow is armed after the engine starts.
- `time`, `sun`: need a timezone (and a location, for sun) in `irori.toml` (rules.md K13). Until
  then the flow is **unarmed** with that reason. `event`: accepted, never fires until protocols
  emit events (rules.md open question 3); a warning says so.

Any trigger can also be fired by hand (§7).

---

## 4. Validation

| Layer | Where | Checks | On failure |
|---|---|---|---|
| 1. JSON Schema | `schemas/flow.schema.json` (generated) | Shapes, enums, ids, unknown fields, counts | File rejected; last good stays |
| 2. Rust types | `irori-flow-types` | Invariants the schema can't hold (rules.md §13.2 for the reused shapes); layout coordinates finite | File rejected; last good stays |
| 3. Graph + home | `irori-flows` | Below | File adopted, flow **unarmed**, problems listed per node |

Layer 3 graph checks:

- every wire's nodes exist; its port exists on that node; no duplicate wires;
- no cycles; nothing wired into a trigger; at least one trigger;
- every non-trigger node is reachable from a trigger (warning otherwise: "never runs");
- a `join` has at least two incoming wires; a `join` with `all` has a `timeout`;
- variables: set on every path before being read (§2.4), one type each;
- **merge warning** (F4): a node, other than a `join`, reached by two paths of one run that
  forked at a port with several wires.

Layer 3 home checks reuse `irori-rules`: entities exist and are the right kind for `from`/`to`/`is`,
services fit the entity and its capabilities, expressions parse inside the allowed surface and
type-check, wait expressions read an entity and not the clock, time/sun gates.

Every problem has `node` (or `wire` index), `severity` (`error` or `warning`) and a sentence.
Errors unarm; warnings don't.

---

## 5. Versions

`version` = lowercase hex SHA-256 of the canonical JSON of the flow without `enabled` and
`layout`: keys sorted, no whitespace, `wires` sorted. Every save whose version is new keeps a
snapshot (definition + layout at the time). Runs record their version and the page draws them on
that snapshot. Restoring a version saves its snapshot as the current file.

---

## 6. Traces and near-misses

A **run record**:

| Field | |
|---|---|
| `run_id` | ULID, also the context `run_id` of every call it makes |
| `flow_id`, `version` | |
| `trigger` | Node id |
| `cause` | The context id of the state change that fired it, if any |
| `test` | absent, `live` or `dry` (§7) |
| `started_at`, `finished_at` | |
| `outcome` | `completed` \| `error` \| `stopped` \| `superseded` \| `aborted` (with `reason`) |
| `ended_at` | The nodes (and ports) its tokens ended at: the one-line summary ("ended at dark → no") |
| `steps` | In order: `seq`, `node`, `token`, `via` (the wire it came in on), `at`, `port`, `reads` (`entity_id`, `availability`, `value`), and per node kind: the expression and its result or error, the call and its answer, the wait's end (`matched` / `timeout` / `gave_up`), a join's arrivals |

A **near-miss**: `at`, `flow_id`, `version`, `node`, `kind`, a sentence, and the reads.
Deliberately narrow, so the log keeps the few that matter:

| `kind` | When |
|---|---|
| `dropped` | The mode refused a firing (`single` busy, queue full, parallel max) |
| `hold_reset` | A trigger's `for` was counting and the value changed back |
| `unavailable` | A watched entity became unavailable or unknown, so its trigger can't fire |
| `same_value` | The entity reported again with the value it already had, so a `to` trigger didn't fire |

Ordinary non-matching changes (`on → off` for a `to: true` trigger) aren't near-misses. They're on
the "why didn't it fire?" timeline, which the page builds from the watched entities' history.

The extension keeps the last **200 runs** and **100 near-misses** per flow, as JSON Lines in its
private state directory, plus the runs in progress in memory.

---

## 7. Testing and backtest

**Fire a trigger** (`test { flow, trigger, dry, overrides }`):

- *Live* (`dry: false`): the saved, armed flow runs for real from that trigger, as if it fired
  now. The mode applies. The run is marked `test: live`.
- *Dry* (`dry: true`): a throw-away engine runs the **draft** (saved or not) from that trigger
  against a snapshot of current states with `overrides` applied (`{ "sensor.x": 20 }`). Calls
  are recorded, not sent. Time fast-forwards through delays and waits. Nothing changes during a
  dry run, so a wait whose condition doesn't already hold ends in `timeout`, and the result says
  that it *assumed nothing changed* — "light off after 10m" there is not a prediction.

**Backtest** (`backtest { flow }`): replays the last day of history of every entity the draft
watches through a throw-away engine, in the order of the states' own timestamps, and returns the
runs it would have made. Next to them, the runs the saved flow actually made in that window. The
answer names the window it really covered: the core keeps at most 2,000 changes per entity, so a
chatty sensor may cover less than a day. Calls in a backtest don't feed back into state (the
history already holds what really happened).

---

## 8. The extension's methods

What the page calls through the bridge's `rpc`:

| Method | Params | Answer |
|---|---|---|
| `flows.list` | — | Each flow: id, name, enabled, armed, problems count, last run, near-miss count |
| `flows.get` | `id` | Definition, version, problems |
| `flows.validate` | `flow` | Problems |
| `flows.save` | `flow` | Version, problems. Writes the file atomically |
| `flows.enable` | `id`, `enabled` | |
| `flows.delete` | `id` | |
| `versions.list` / `versions.get` / `versions.restore` | `id`, `version` | |
| `runs.list` | `id` | Run summaries, newest first |
| `runs.get` | `run_id` | The run record |
| `runs.active` | `id?` | Runs in progress, each with where its tokens are and until when they wait |
| `runs.cancel` | `run_id` | |
| `nearmiss.list` | `id` | |
| `timeline` | `id`, `from`, `to` | Watched entities' changes, near-misses and runs in the window |
| `test` | §7 | The run record |
| `backtest` | §7 | Would-be runs, actual runs, covered window |
| `live` | `id`, `after?` | What the canvas plays while the flow is open: runs finished and near-misses since `after` (none without it; at most 5 each, oldest first), the triggers holding a `for` and when they'd fire, and `now` to ask with next time |
| `tests.get` / `tests.save` | `id`, `settings` | How the page last set up a test of this flow (up to 16 KB, kept as given), so it runs again the same way. Deleted with the flow |

---

## 9. Limits

128 nodes, 512 wires, 16 switch cases, 256 steps per run, `queued`/`parallel` max 32, expressions
512 characters, durations 1 ms – 7 days (rules.md §4.3).

## 10. Not in this spec

Flows calling flows; loops; custom protocol services; events from protocols; timezone and
location in `irori.toml`; backtests longer than a day (C18); push to the page (C17).
