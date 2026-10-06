# Motion design

How the web UI (`crates/irori-ui`) moves, and the rules for adding to it. Motion in Irori is
there to say something — *that worked*, *still working*, *this is where it went* — never to be
looked at. If a change moves something, it goes through this page; if it changes a rule here,
it updates this page in the same PR.

---

## 1. Principles

1. **Expressive, but snappy.** Irori moves like it means it — pages arrive from where they are
   in the sidebar, a tile grows into its page, light pools in the rooms, numbers count to their
   new value — but a control panel is used every day, so nothing makes anyone wait. Controls
   answer in 120–180 ms; the moments that are the point (a page, a floor, a count) take up to
   about 340 ms, arriving fast and settling long.
2. **Motion answers something.** Every animation here explains a change: a switch moved, a
   command is in flight, a page came from that row, the data just arrived. If it explains
   nothing, it doesn't ship.
3. **Follow the thing, not the frame.** Things move from where they were to where they are —
   the sidebar highlight slides to the new page, a device's name travels into its page's
   heading, a drawer opens to exactly its content. Nothing flies in from off-screen.
4. **Live data never replays.** Readings arrive every couple of seconds. An animation that
   plays when something *appears* must never be on something that is redrawn with each
   reading (see §4).
5. **Off means off.** The system's *reduce motion* setting, or Settings → Appearance → Motion,
   turns every animation and transition off. The page must say exactly the same things without
   motion — motion is never the only way something is shown.

## 2. Tokens

Defined on `:root` in `crates/irori-ui/index.html`. Use these rather than new numbers: the off
switch works by setting the durations to zero, so a hard-coded `200ms` escapes it.

| Token | Value | For |
|---|---|---|
| `--dur-fast` | 120 ms | Feedback on something touched: hover colours, a press, a tooltip appearing. |
| `--dur-base` | 180 ms | A small move: the toggle's knob, the gliding highlight, a panel sliding in, a chevron turning. |
| `--dur-layout` | 240 ms | Something that moves the layout around it: a drawer rolling down. |
| `--dur-expressive` | 340 ms | The moments that are the point: a page arriving, a tile growing into its page, a floor changing, a device's name travelling, a light pool fading. |
| `--ease-out` | `cubic-bezier(.2, .7, .3, 1)` | The default: starts quick, settles gently. |
| `--ease-spring` | `cubic-bezier(.34, 1.56, .64, 1)` | For what a finger pushes around: the toggle's knob, a marker leaning in, a chevron turning. A mild overshoot. |
| `--ease-emphasized` | `cubic-bezier(.2, 0, 0, 1)` | For things crossing the page: arrives fast, settles long. Page and floor changes. |
| `--ease-bounce` | `cubic-bezier(.34, 1.9, .5, 1)` | For things *let go of*: a slider's thumb released, a marker put down, a pulled handle springing back. |

Ambient loops (the Live dot, the Start page's ember, a motion sensor's ripples) run slower,
1.8–3.2 s, and are the only things that repeat forever — and only while what they show is true.
**Data arriving** has its own timing: a chart draws its line in over 560 ms, once, when its day
arrives; a number counts to its new value over 420 ms.

## 3. The catalogue

What moves today, and where to find it.

| Where | What | How |
|---|---|---|
| Every button | Eases into hover colours; gives a little (`scale: .97`) under a press. | `:where(button…)` base rule — zero specificity, so each button's own rules win. |
| Toggle | Knob springs across, stretches while held, breathes while a command waits on the device (`.pending`). | `devices/controls/mod.rs` `knob`; CSS `.toggle`. |
| Sliders | Drawn by the page: filled to the thumb (`--fill`), colour temperature as its colours; the thumb swells under the pointer; the label follows while dragging. | `devices/controls/light.rs` `light_controls`, `floorplan.rs` inspector; CSS `input[type="range"]`. |
| Sidebar | The current page is a highlight that glides between entries; icons lean in on hover, the cog turns. | `glide.rs`; CSS `.glide`, `.sidebar > .glide`. |
| Changing page | The page arrives from where it is in the sidebar — up from below for a page further down, down from above for one further up; between devices it crossfades. | `transition.rs` (View Transition API through the router); CSS `::view-transition-*`. |
| List → device | The list slides aside and the device's name travels from its row into the heading; back reverses it. | `transition.rs`; only the opened device's name carries a transition name. |
| Start tiles | A tile grows into the heading of the page it opens, while the page rises in behind. | `transition.rs` `expand`; the tile and the new `.page-head` are `hero`. |
| Rows that fold | A group of devices, a section of Settings, a floor: the chevron turns and a drawer rolls down to exactly what's inside, and back. Nothing arrives on its own — every row starts where it was left, Settings folded. | `fold.rs`; CSS `.fold-row`, `.chevron`, `.drawer`. |
| Switchers | Which view, what a list is grouped by, chart or table, which log lines: one highlight slides to the chosen one and takes its width. | `segmented.rs`, `glide.rs` `across`; CSS `.switcher > .glide`. |
| Regrouping a list | Devices and Entities, grouped another way: each row on show travels from where it was to where it now belongs, and the headings change behind them. Past 80 rows the list just changes. | `devices/tables.rs` `regroup`, `transition.rs` `around`; rows carry `--vt`, named only under `html[data-nav="regroup"]`. |
| Entity list history | The same drawer as a device's page, on every row of the Entities list. It stays down, and its chart stays drawn, while readings arrive. Regrouped, the drawer is still down and its day is not asked for again, though the chart draws its line in once more. | `history.rs`, `devices/tables.rs` `entity_row`, `device.rs` `history_panel`. |
| System meters | A bar grows to its figure when the machine answers and moves when the figure does; turns red when nearly full; opens, like any drawer, to what's behind the figure and its last day as a chart. | `machine.rs` `meter`; CSS `.meter-fill` (a transition on `scale`). |
| Floors and areas | "Add area" opens where the area will appear, its plus turning to a cross; Remove widens into "Remove?" before it does anything; while something is dragged, only what is under the pointer lights up, a folded floor opens when lingered over, and what was dropped travels to where it landed. | `places.rs`; CSS `.namer`, `.icon-button.asking`, `.over`, `html[data-nav="place"]`. |
| Controls that follow what a thing is doing | A player, a lock, a cover or a valve only offers what makes sense now, and its row is drawn once and kept, so the change happens in place: Play turns into Pause, Stop and the volume fold away when it's off, Lock gives way to Unlock, a padlock's shackle lifts and drops (works up and down while the bolt moves, shakes once if it jams). | `devices/controls/mod.rs` `kept`, `glyph`, `tuck`; `media_player.rs`, `lock.rs`, `opening.rs`; CSS `.tuck`, `.glyph`, `.padlock`. |
| Changing a name where it stands | A pencil beside a name, quiet until its row is pointed at; pressed, the name becomes a field in the same place with its own tick and cross, rising in once. | `inline.rs`; CSS `.editable`, `.pencil`, `.inline-edit`. |
| Extension cards | A card lifts a little under the pointer; its gear leans; a mark sits on the log button when the log has an error in it. | `extensions.rs` `card`; CSS `.ext-card`, `.ext-settings-btn`, `.ext-mark`. |
| Choices | A mode, a tone, an area: buttons in a row, the chosen one sunk with an ember edge. Hover and press are the button's. | `choices.rs`; CSS `.choices`. |
| Live numbers | A reading counts to its new value and lifts or drops into place in ember, the way it went. | `count.rs`, for anything marked `data-n`. |
| Device page history | The reading is the button — or pull it down; a drawer rolls down to its content; a number's day is a chart that draws itself in and then **grows with each new reading**, with a crosshair, a tooltip and arrow-key stepping. | `device.rs` `EntityRow`, `chart.rs`, `gesture.rs`; CSS `.unroll`, `.drawer`, `.chart-*`. |
| Swiping a toggle | The knob follows the finger; let go past halfway and it switches, springing home from where it was let go. | `gesture.rs`, `devices/controls/mod.rs` `knob`. |
| Floorplan | Tools and floor highlight glide; the toolbar, pickers and inspector slide in; markers lean in, lift when held and settle with a bounce when put down; a lamp's pip warms up and rings once; the picked room and the line being drawn march. | `floorplan.rs`; CSS `.floorplan …`. |
| Floorplan ambience | A lit light pools warm light, as bright as the light and clipped to its room's walls; a motion sensor ripples across its room while it senses someone; changing floor, the plan sinks away and the next floor comes down (or the reverse). | `floorplan/ambience.rs`, `transition.rs` `around`. |
| Windows | A window rises into place while the page dims behind it. | `modal.rs`; CSS `.modal`, `.modal-backdrop`. |
| Add device | The extension cards arrive one after another; picking one grows it into the next step's heading while the step slides aside (Back reverses it); found devices come in one after another, one that turns up later glows once; **Add** turns into a ring, then a check that draws itself, and the card folds away as the device appears under "In your home"; while it's listening and has found nothing, a ring pings to say so; the button's count pops in. | `devices.rs` `AddDevice`, `ProtocolStep`; `transition.rs` `step`, `name_target`; CSS `.protocol-card`, `.device-card`, `.add-one`, `.listening-ring`, `.count-badge`. |
| Permit joining | Between the press and the network's own word that it opened (or closed), a small ring turns in the button beside "Opening…" or "Closing…", so the wait reads as work, not as a press that missed. While the network is open the button is its countdown: a ring springs in and drains with the time left, which is also written beside it (`0:47`); hovering or focusing swaps the words to "Close now", and a press closes it. The ring eases between clock readings, so it glides; with motion off it steps and the written time carries it. Driven by how long the server says is left, never by a timer started on the press, so a reload or a second tab shows the same. | `devices.rs` `ProtocolActions`, `closing_times`; CSS `.add.join-open`, `.add.join-waiting`, `.join-wait`, `.join-ring`, `.join-words`. |
| Log window | A line that arrives while the window is open fades in where it lands; the copy button's clipboard gives way to a check that draws itself, or shakes when the browser refused. | `log_window.rs`; CSS `.log-row.fresh`, `.log-copy`. |
| Ambient | Live dot breathes (still when the core stops answering); the Start ember flickers; Start tiles stagger in; a banner drops in. | CSS `.dot.ok`, `.start-ember`, `.tiles .tile`, `.banner`. |

## 4. Rules

### Nothing that is redrawn animates on appearing

The device list, the Floorplan's markers and drawing, and the inspector are rebuilt when the
home changes — every reading, and on the Floorplan every pan. An entrance animation there would
replay every couple of seconds. So:

- **On redrawn things, use transitions on a state change** — a class or attribute that flips
  (`aria-pressed`, `.on`, `.chosen`). Leptos updates those in place, so the transition plays
  once, when the state really changes.
- **Entrance animations only go on things that appear once** per visit or per action: a page's
  fixed markup, a panel that opens on Edit, a banner that appears when trouble starts.
- **Use `animation-fill-mode: backwards`**, not `both`, for entrances: once it has played, an
  entrance shouldn't hold `transform` or `opacity` over the element's own hover styles.

### A control that changes shape is drawn once

Most controls are drawn again from each reading, which is fine for a reading and a switch.
One whose buttons depend on what the thing is doing can't be: a button that is drawn again
can't turn into another, and a slider drawn again leaves the finger. Those go through `kept`
(`devices/controls/mod.rs`): drawn once, with the state arriving as a signal, and everything
that changes is a class or an attribute that flips. A button that only sometimes applies is
wrapped in `tuck`, which folds it to nothing and makes it `inert`, rather than being left out.

### Move with the individual properties

Many elements are already placed by their `transform` (the Floorplan's markers are
`translate(-50%, -50%)`). Animate `translate`, `scale` and `rotate` — the individual properties —
so motion adds to the placement instead of replacing it.

### Keep layout motion to the layout's own size

A drawer opens with `grid-template-rows: 0fr → 1fr`, so it opens to exactly its content and
closes the same way. Keep it mounted, so it can roll up as well as down, and make it `inert`
while closed so nobody tabs into what they can't see. Where content is still loading, hold its
final height (the chart's waiting shimmer), so nothing jumps when it arrives.

### Choosing highlights glide

Where one item in a column is chosen — the sidebar's page, the Floorplan's tool, its floor —
or one in a row of a switcher (`segmented.rs`), the choice is a `<span class="glide">` that
`glide.rs` moves to the chosen item (`glide` down a column, `across` along a row). It lands
without travelling the first time, and fades out where nothing is chosen. Where the column
becomes a row (on a phone), CSS hides it and the chosen item draws its own highlight.

### Every page change says where it's going

The router asks the browser for a view transition on every page change; `transition.rs` says
which change it is as `data-nav` on `<html>`, and CSS animates by it:

| `data-nav` | When | What moves |
|---|---|---|
| `down` / `up` | To a page further down / up the sidebar | The page rises from below / drops from above; the shell stays put, its highlight gliding on its own. |
| `into-device` / `to-list` | The device list ↔ a device | Sideways, and the device's name travels between its row and the heading. |
| `tile` | A Start tile, clicked | The tile grows into the new page's heading row. |
| `fade` | Between two devices, or anywhere the sidebar doesn't list | A crossfade. |
| `floor-up` / `floor-down` | Changing floor on the Floorplan (`around`) | Only the plan. |
| `step-in` / `step-out` | Into an extension's screen in the Add device window, and back (`around`, `step`) | Only the window's step; the picked card and the step's heading (`add-hero`) become each other. |
| `regroup` | A list grouped another way (`around`) | Only its rows, each to its new place. The page itself changes at once. |
| `place` | A device dropped on an area, an area on a floor (`around`) | Only chips and areas, the dropped one to where it landed. |

A new kind of change gets a row here, a case in `navigation`, and a test.

Rows that travel (`regroup`, `place`) each need a name of their own, so they carry one as
`--vt` and CSS turns it into a `view-transition-name` only while that change is under way —
never on a page change, where every row would lift out of the page. Only rows that are on show
are named: one inside a folded group would be pictured where it is laid out and fly out of a
group that isn't showing it.

### Numbers that change count, through `data-n`

Give an element `data-n="<value>"` and the number inside it (its first text node) counts from
its old value to its new one when it changes, then lands on exactly the text Leptos wrote
(`count.rs`). It also gets `data-moved` for the ember lift or drop. Nothing else is needed; a
number seen for the first time just appears. Keep the markup the same shape for numbers and
words (`data-n` is simply absent for words), so the element is updated in place.

### What a gesture is in the middle of lives on the element

Rows are redrawn as readings arrive — in place, but with fresh handlers — so a drag in progress
keeps its state in `data-*` attributes on the element, not in Rust (`gesture.rs`). A drag is
never also the click that follows it (`swallow_click`). Where a gesture belongs to an element,
set `touch-action` so the page doesn't take it for a scroll.

### Always draw what fades both ways

Something that should fade out as well as in is always drawn and made transparent, never
removed: every light has its pool, at opacity 0 when off. Removing it would make it blink out.

### A view transition's update never waits for a frame

`transition::around` wraps any change that isn't a page change (a floor) in a view transition.
The browser holds frames back until the update answers, so the update waits for Leptos with a
zero-length timeout — waiting for an animation frame there waits forever. Two elements may never
share a transition name in the same picture, or the browser skips the transition: that is why a
Start tile is `hero`, not `page`.

### Charts

Charts follow the dataviz rules (`dataviz` skill): one series, so no legend; a 2 px line in the
ember; solid hairline gridlines on round numbers; the latest value labelled at the line's end;
a crosshair and tooltip on hover, the same on keyboard focus (arrow keys, Home, End); and a table
of the same data a click away. A step line, not a slope, for readings that hold until the next.
Motion: the line draws in once, the area settles in behind it, now's dot lands and rings once.

### Off, and reduced motion

Both the media query and `data-motion="off"` (on `.shell`, and on `<html>` for page changes)
zero the tokens and set `animation: none; transition: none` everywhere, view transitions
included. Anything that uses the tokens is covered for free; anything that draws its own
motion (a script-driven animation, a new pseudo-element tree) needs adding to those rules.

## 5. Adding motion — a checklist

1. Which of §1's questions does it answer? If none, stop.
2. Pick a token from §2. No new durations or easings without changing this page.
3. Is the element redrawn when readings arrive or the view pans? Then it transitions on a state
   change, never animates on appearing (§4).
4. Placed by `transform` already? Use `translate` / `scale` / `rotate` instead.
5. Turn Motion off in Settings and check the page still says the same thing.
6. Check it in the browser (§6). The download budget is 5 MB, so there's room — but CSS is still
   cheaper than code, and a new reactive type in Rust costs more than it looks.
7. Add it to §3's catalogue.

## 6. Checking it

What we've used to prove motion does what it should, in headless Chrome over the DevTools
protocol:

- **Nothing replays:** listen for `animationstart` on `document` (capturing) for a few seconds
  while readings arrive, while panning, while editing — the list should stay empty.
- **Things update in place:** keep a reference to an element, cause the change, and check it's
  still `isConnected` — if the node was replaced, its transition never played.
- **Catching the middle of a movement:** `element.getAnimations()`, then `pause()` and set
  `currentTime`, then screenshot. `Animation.setPlaybackRate` slows everything, but doesn't
  reach every transition.
- **The off switch:** with Motion off, and with `prefers-reduced-motion: reduce` emulated,
  `getComputedStyle(el).animationName` is `none` and transition durations are `0s`.
