# Motion design

How the web UI (`crates/irori-ui`) moves, and the rules for adding to it. Motion in Irori is
there to say something — *that worked*, *still working*, *this is where it went* — never to be
looked at. If a change moves something, it goes through this page; if it changes a rule here,
it updates this page in the same PR.

---

## 1. Principles

1. **Quick and small.** A control panel is used every day. Nothing should make anyone wait for
   it: most motion is over in 120–180 ms and travels 2–8 px.
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
| `--dur-layout` | 240 ms | Something that moves the layout around it, or crosses the page: a drawer rolling down, the list-to-device page change. |
| `--ease-out` | `cubic-bezier(.2, .7, .3, 1)` | The default: starts quick, settles gently. |
| `--ease-spring` | `cubic-bezier(.34, 1.56, .64, 1)` | Only for what a finger pushes around: the toggle's knob, a slider's thumb, a marker leaning in, a chevron turning. A mild overshoot. |

Ambient loops (the Live dot, the Start page's ember) run slower, 2.4–3.2 s, and are the only
things that repeat forever. **Data arriving** is the one allowed exception to the durations: a
chart draws its line in over 560 ms, once, when its day arrives.

## 3. The catalogue

What moves today, and where to find it.

| Where | What | How |
|---|---|---|
| Every button | Eases into hover colours; gives a little (`scale: .97`) under a press. | `:where(button…)` base rule — zero specificity, so each button's own rules win. |
| Toggle | Knob springs across, stretches while held, breathes while a command waits on the device (`.pending`). | `devices.rs` `knob`; CSS `.toggle`. |
| Sliders | Drawn by the page: filled to the thumb (`--fill`), colour temperature as its colours; the thumb swells under the pointer; the label follows while dragging. | `devices.rs` `light_controls`, `floorplan.rs` inspector; CSS `input[type="range"]`. |
| Sidebar | The current page is a highlight that glides between entries; icons lean in on hover, the cog turns. | `glide.rs`; CSS `.glide`, `.sidebar > .glide`. |
| List → device | The list slides aside and the device's name travels from its row into the heading; back reverses it. | `transition.rs` (View Transition API through the router); CSS `::view-transition-*`. |
| Device page history | The reading is the button; a drawer rolls down to its content; a number's day is a chart that draws itself in, with a crosshair, a tooltip and arrow-key stepping. | `device.rs` `EntityRow`, `chart.rs`; CSS `.unroll`, `.drawer`, `.chart-*`. |
| Floorplan | Tools and floor highlight glide; the toolbar, pickers and inspector slide in; markers lean in; a lamp's pip warms up and rings once; the picked room and the line being drawn march. | `floorplan.rs`; CSS `.floorplan …`. |
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
the choice is a `<span class="glide">` that `glide.rs` moves to the chosen item. It lands
without travelling the first time, and fades out where nothing is chosen. Where the column
becomes a row (on a phone), CSS hides it and the chosen item draws its own highlight.

### Page changes are instant — except list ↔ device

The router asks the browser for a view transition on every page change; every one but the
device list ↔ a device is told to have no animation, which ends it at once. For that pair,
`transition.rs` sets `data-nav` on `<html>` so CSS knows the direction, and the device's name
carries the same `view-transition-name` on both pages. A new pair gets added there, with a test.

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
