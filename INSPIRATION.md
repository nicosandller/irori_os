# Irori — Inspiration

> *An irori is the sunken hearth at the center of a traditional Japanese home: one small fire that the whole household gathers around.*
> Irori wants to be that for the smart home: small, warm, central, and easy to understand.

Companion document: [ROADMAP.md](ROADMAP.md) (architecture, decisions, milestones).
Last revised: 2026-09-15.

---

## 1. The one-sentence pitch

**Irori is a lightning-fast, modular, single-binary smart home core written in Rust — barebones by default, extensible with anything from a new protocol to a whole automation engine — where the first-party sequential engine is structured data that can be generated, visualized, replayed, and explained, by you or by an AI.**

## 2. Why this should exist

Home Assistant (HA) is an extraordinary project: ~2,800 integrations and millions of homes. Irori does not try to out-build it. It targets a handful of pain points that are architectural in HA — things that are hard to fix in a 10+ year-old Python codebase but cheap to get right on day one of a new one.

| Pain point in HA today | Root cause | Irori's answer |
|---|---|---|
| Dashboards are tedious enough that people "vibe-code" them with chat AIs instead of using the editor | Dashboards are hand-assembled card configs; the AI workflow lives *outside* the product | AI dashboard generation is a first-class, in-product feature, targeting a tiny stable SDK |
| LLMs (and humans) produce broken automations | Not YAML itself, but an **open-ended language**: Jinja templates, stringly-typed state, service calls unchecked against the actual home | The **first-party sequential engine** is a closed, typed schema with a small expression language, validated against the *real* entity registry. Other engines (flows, LLMs) are other extensions. |
| "Why didn't my automation fire?" is hard to answer | Traces were retrofitted onto an engine not designed around them | **Traces are an output of the engine by construction** — every node records what it read, what it decided, and why |
| No way to know what a new rule *would* do | The engine is coupled to wall-clock time and live state | A **deterministic, replayable engine** (injected clock + state source) that can backtest a rule against recorded history |
| Install and upgrade are heavy (Supervisor, containers, OS images) | Python runtime + dependency graph | **One static binary**: `curl` → running setup wizard in seconds |
| Beyond defaults, everything depends on HACS with variable quality | Custom code runs in-process, touching internals; backend integrations, frontend cards, and add-ons each have their own separate mechanism | One **extension** system for everything: an extension declares what it *contributes* (integrations, dashboards, cards, apps) and what permissions it needs. One install flow, one permission prompt, typed contracts per contribution, and third-party code isolated in separate processes or sandboxes |
| Multi-user permissions and per-user dashboards are recurring forum complaints | Bolted on late | Designed into the API model early, shipped after the core loop is proven |

## 3. The core insight

The most important decision in Irori is not "Rust" and not "AI". It's this:

> **Everything a user or an LLM authors is schema-validated data, and everything the system does leaves a structured trace.**

Once that holds, the good features come almost for free:

- **Generation** — an LLM fills in a schema and gets told exactly what's wrong (unknown entity, invalid service, type mismatch) instead of producing plausible-looking garbage.
- **Visualization** — a structured rule *is* a graph; there's nothing to parse.
- **Debugging** — a structured trace maps 1:1 onto that graph; step through it.
- **Simulation** — a deterministic engine plus a recorder means "what would this rule have done last week?" is a query, not a guess.
- **Explanation** — rule + trace + history is exactly the context an LLM needs to answer "why didn't the hallway light turn on?"

HA has pieces of each of these. Irori's bet is that having *all of them, designed together, from the first commit* adds up to a noticeably better experience.

## 4. A day with Irori (the feeling we're building toward)

It's 11pm and the hallway light didn't come on when you walked past.

You open Irori on your phone and tap **Hallway motion light**. The rule shows as a small graph: *motion → lux below 30? → light on → wait for no motion 2m → light off*. Tonight's run is highlighted: the trigger fired, and the lux condition is red — it read **42**.

You tap **Explain**. Irori answers: *"The lux sensor reported 42 because the living room lamp was still on and it's in the sensor's line of sight. In the last 7 days this condition blocked the rule 5 times, all between 22:00 and 23:30."* It suggests a change — use the sun elevation instead of lux after sunset — and shows a **backtest**: with the change, the rule would have fired on all 5 of those nights and never during daylight.

You accept. The new version is saved, the old one stays in the history, and tomorrow's trace will point at the new graph.

Later you type *"a dashboard for the kitchen tablet: lights, the dishwasher status, and the fridge temperature with a 24h chart"*. Irori generates it, shows you a preview in a sandbox, and lists exactly which entities and actions the dashboard will be allowed to touch.

No YAML. No Jinja. No custom cards to install. And you installed the whole thing with one command three weeks ago, pointed it at the MQTT broker your Zigbee2MQTT was already using, and it found every device on its own.

## 5. Design tenets

**The core comes first.** Everything else in this document — visualizer, AI, dashboards — is built on top of a core that has to be excellent by itself. The first four tenets describe that core.

1. **Lightning fast and lightweight.** Rust, one static binary, no runtime dependencies. Performance is a *budget* measured in CI (memory, startup, event latency, binary size), not a hope. It should feel instant on a Raspberry Pi and barely register on anything bigger.
2. **Modular — everything beyond the core is an extension, even MQTT, even automations.** The core knows about devices, entities, state, and events; it does *not* know about any protocol, vendor, dashboard, app, or rule language. Everything else ships as an **extension** that declares what it *contributes*:
   - **Integrations** bring devices in: protocols (MQTT, Zigbee, Matter, Z-Wave, ESPHome) and vendor APIs (Tesla, SwitchBot, …).
   - **Automation engines** subscribe to those events, call services, and emit traces. The first-party sequential engine is one of them, installed like any other extension, not compiled into the OS.
   - **Dashboards** are pre-built views that bind to whatever matching devices your home has.
   - **Cards** are visualizations used inside dashboards.
   - **Apps** are whole tools with their own page: a terminal, a file explorer, a log viewer.

   First-party extensions can be compiled in and toggled on or off. Anyone can write their own in any language, as a separate process or a sandboxed web bundle, using the same contracts. Adding a protocol, a dashboard, or an app never requires touching the core.
3. **Barebones by default.** The default build ships the core, a CLI, and a minimal web UI with just what's needed: **Devices, Extensions**, and Settings. Nothing else is installed or running unless you turn it on — including automations. Robust at its smallest; capabilities are added, never assumed.
4. **Nerd friendly.** Plain-text config you can put in git. Everything the UI can do, the CLI can do (with `--json` output). Structured logs, a metrics endpoint, shell completions, a documented API, extension templates you can copy and hack. No magic, no hidden state.
5. **Structured over textual.** Rules, dashboard metadata, extension manifests and configs: typed, schema-validated, versioned. No template-string escape hatches.
6. **Observable by construction.** Every automation run produces a trace (the engine's job). Every state change carries a *context* (what caused it: a device, a user, an installed engine's run).
7. **Deterministic and replayable.** The first-party sequential engine never reads the wall clock or global state directly; both are injected. Replay is a feature, not a test trick. Other engines may choose otherwise.
8. **Small core, public API.** Core = registry, state, event bus, extension host, recorder, API. Extensions (including automation engines), AI features, and the UI use only public contracts — enforced by crate boundaries, not good intentions.
9. **Least privilege for extensions.** Every extension declares its permissions (which devices it may control, network hosts, serial ports, host shell or files) and the owner approves them at install. A card can't reach what it didn't declare; a terminal app is labeled for what it is (full access to the machine).
10. **Local-first.** The core never needs the internet. Cloud integrations are clearly labeled as such. AI is an optional add-on and bring-your-own: a cloud API key or a local model. A hosted tier may exist later as a convenience, never a requirement.
11. **Install in seconds.** `curl` → running setup wizard, sensible defaults.
12. **Borrow ecosystems, don't rebuild them.** Speak HA's MQTT Discovery protocol, wrap `zwave-js-server`, use `rs-matter`. Use HA-familiar domain/service vocabulary where it fits — LLMs already know it.
13. **Rust end to end.** Core and web UI share the same type definitions. An engine that ships a WASM editor shares *its* types with *its* page — not with the OS.

## 6. Who it's for (first)

**Tinkerers with MQTT gear.** People running Zigbee2MQTT, Tasmota, ESPHome, or DIY devices; comfortable with a terminal; possibly already running HA and frustrated by YAML, templates, and dashboard editing.

Crucially, these people can try Irori **without migrating**: point it at the same MQTT broker HA already uses, and it discovers the same devices from retained discovery messages. Irori can run in *shadow mode* next to HA for weeks before anyone switches anything off.

Not (yet) for: non-technical households, people whose homes are mostly cloud-API devices, anyone who needs Z-Wave/Matter on day one.

## 7. What Irori is not

- **Not an operating system.** "IroriOS" is a brand. Irori is a userspace process on stock Linux (an appliance image may exist later as packaging only).
- **Not an HA replacement at launch.** No attempt to match integration coverage. The MQTT integration is the v1 device story; the extension system is what makes the rest possible.
- **Not batteries-included.** The default install is deliberately minimal; features are opt-in.
- **Not reimplementing radio stacks** (Zigbee/Z-Wave/Matter) in the core.
- **Not a template language.** If a rule needs arbitrary code, write an extension that offers it as a typed service.
- **Not cloud-dependent.** Ever, for core operation.

## 8. Landscape and prior art

Assume the gap narrows over time. Irori wins by coherence, not by any one feature.

- **Home Assistant** — actively improving dashboards and automation UX release after release, and adding AI surfaces (LLM conversation agents, AI Task, an MCP server integration). The benchmark and the ecosystem we borrow from. *(Verify the specifics of recent 2026.x releases before public positioning.)*
- **VS Code and Grafana** — not smart home tools, but the models for Irori's extension system: VS Code has one extension package that declares "contribution points"; Grafana has typed plugin kinds (data source, panel, app) that map closely onto integration, card, and app.
- **Node-RED** — the best-known visual, flow-based automation tool, often used alongside HA. Prior art for the visualizer and debug UX; its weakness (flows as the programming model get unwieldy, weak typing) is instructive.
- **openHAB** — Java, rules DSL + Blockly; prior art for typed items and rule languages.
- **Homey (Athom)** — consumer-grade "Flows"; a good reference for approachable automation UI.
- **`ai_agent_ha`** (GitHub, sbenodiz) — open-source natural-language dashboard/automation generation for HA. Review for overlap and UX lessons.
- **`sync`** (GitHub, mossida/sync) — Rust home automation project with similar ambitions. Review for architecture and for a reality check on solo-project velocity.
- **Nabu Casa** — HA's commercial arm; the reference business model (open, free local operation; paid hosted convenience).

## 9. Sustainability (direction, not a pitch)

A solo side project aiming at a real product, not a VC-scale company. The likely model mirrors Nabu Casa:

- The core and local features are free.
- Bring-your-own AI key or local model always works.
- A paid, hosted tier offers convenience: AI generation credits without managing keys, maybe remote access and backups.

Licensing is undecided; see the roadmap for the deadline on that decision.
