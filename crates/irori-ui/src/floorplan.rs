//! The Floorplan page: the home as a drawing, and the tools to draw it.
//!
//! Two modes, one canvas. **Reading** is the plan with the devices live on it — a lamp that's on
//! glows, and clicking it switches it. **Editing** puts a toolbar over the same canvas and works
//! on a copy of the plan; Save sends the copy, Cancel drops it. Nothing reaches
//! `config/floorplan.toml` until Save, so a half-drawn room is never something the file — or
//! anyone else's browser — has to survive.
//!
//! Everything here is in whole centimetres of the home, and the only thing that turns them into
//! pixels is [`Viewport`]. Keeping that one conversion in one place is what lets the walls, the
//! grid and the device markers agree about where anything is at any zoom.

use std::collections::BTreeMap;

use irori_types::{
    AreaId, Capabilities, Device, DeviceId, EntityId, FloorId, Floorplan, Level, Opening,
    OpeningKind, PlacedArea, PlacedDevice, Point, Timestamp, Wall,
};
use leptos::ev;
use leptos::html::Div;
use leptos::prelude::*;
use leptos::task::spawn_local;
use web_sys::wasm_bindgen::JsCast;

use crate::api::{self, Home};
use crate::devices::Controls;
use crate::icons::icon;

mod ambience;
mod look;

use look::{Look, look_for};

/// What the editor rounds to by default, in centimetres. Fine enough to draw a real room,
/// coarse enough that two walls meant to meet actually do.
const SNAP: i32 = 10;

/// The range a custom snap can be set to, in centimetres. One centimetre is as fine as a plan
/// goes; a metre is as coarse as is still drawing rather than guessing.
const SNAP_RANGE: std::ops::RangeInclusive<i32> = 1..=100;

/// The spacing of the drawn grid, in centimetres: one square metre.
const GRID: i32 = 100;

/// How far apart the editor lets a point land.
///
/// Two settings rather than a number with a default, because they are two different intentions:
/// **Grid** is "I'm drawing a house and 10 cm is plenty", and never needs thinking about.
/// **Custom** is "this wall really is 137 cm", and is worth the slider it costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Snap {
    Grid,
    /// A step of somebody's own, in centimetres, inside [`SNAP_RANGE`].
    Custom(i32),
}

impl Snap {
    /// The step to round to, in centimetres. Never zero: a plan is whole centimetres either way,
    /// so the finest custom step is already no rounding at all.
    fn step(self) -> i32 {
        match self {
            Snap::Grid => SNAP,
            Snap::Custom(step) => step.clamp(*SNAP_RANGE.start(), *SNAP_RANGE.end()),
        }
    }

    fn is_custom(self) -> bool {
        matches!(self, Snap::Custom(_))
    }
}

/// How close, in screen pixels, a click has to be to something to count as a click *on* it.
/// In pixels rather than centimetres so it stays the same size under the pointer at any zoom.
const REACH: f64 = 12.0;

/// How near, in screen pixels, a wall's end has to be for a new wall to snap onto it. Bigger
/// than [`REACH`]: closing a room is the common move, and missing by two centimetres leaves a
/// gap that only shows up later.
const CORNER: f64 = 18.0;

/// How far from a corner the live angle label sits, in screen pixels, so it floats off the
/// corner it names rather than over it.
const ANGLE_OFFSET: f64 = 26.0;

/// How far from a corner the arc of the corner reaches, in the plan's own centimetres. Far
/// enough to read across the corner without reaching for the wall itself.
const ANGLE_ARC: f64 = 55.0;

/// The limits of the zoom, in screen pixels per centimetre. At the low end a 30-metre house
/// fits; at the high end a centimetre is a pixel and a half.
const MIN_SCALE: f64 = 0.06;
const MAX_SCALE: f64 = 1.5;

/// How much of the canvas the chrome around it takes up, in screen pixels, when a plan is being
/// framed into what's left.
#[derive(Debug, Clone, Copy)]
struct Insets {
    top: f64,
    bottom: f64,
    left: f64,
    right: f64,
}

/// How many steps back the editor can go. Far more than anyone reaches for, and still only a
/// few hundred kilobytes of plans.
const HISTORY: usize = 100;

/// Where the floor last looked at is remembered. A preference about this screen rather than
/// something about the home, so it belongs to the browser.
const FLOOR_KEY: &str = "irori.floorplan.floor";

/// What's drawn on one floor, or nothing at all — which is what a home with no floors has, and
/// what a floor nobody has drawn on has.
fn on_floor(plan: &Floorplan, floor: Option<&FloorId>) -> Level {
    floor
        .and_then(|floor| plan.level(floor))
        .cloned()
        .unwrap_or_default()
}

/// Changes the floor being drawn, making its plan if this is the first thing to go on it.
fn on_level(
    draft: RwSignal<Floorplan>,
    floor: RwSignal<Option<FloorId>>,
    change: impl FnOnce(&mut Level),
) {
    let Some(floor) = floor.get_untracked() else {
        return;
    };
    draft.update(|plan| change(plan.level_mut(&floor)));
}

/// Where the plan sits in the canvas: how many screen pixels one centimetre of home takes up,
/// and where the plan's origin has been pushed to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Viewport {
    scale: f64,
    pan: (f64, f64),
}

impl Default for Viewport {
    /// Six pixels to ten centimetres — a five-metre room is about 300 pixels across — with the
    /// origin off the top-left corner so a plan drawn from (0, 0) is on screen. Chosen so the
    /// default snap step is a grid somebody can see from the moment the page opens.
    fn default() -> Self {
        Self {
            scale: 0.6,
            pan: (90.0, 90.0),
        }
    }
}

impl Viewport {
    fn screen(self, point: Point) -> (f64, f64) {
        (
            self.pan.0 + f64::from(point.x) * self.scale,
            self.pan.1 + f64::from(point.y) * self.scale,
        )
    }

    fn world(self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.pan.0) / self.scale, (y - self.pan.1) / self.scale)
    }

    /// How many centimetres a screen pixel is worth here, for turning a reach in pixels into a
    /// distance on the plan.
    fn cm_per_pixel(self) -> f64 {
        1.0 / self.scale
    }
}

/// What a click on the canvas does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    /// Pick things up: select, move, and pan the view.
    Select,
    /// Click to start a wall, click again to end it — and again, and again, because walls come
    /// in runs and a room is four of them.
    Wall,
    Door,
    Window,
    /// Trace out a room. Needs one chosen from the list first, and its corners prefer the walls
    /// already drawn to the grid.
    Area,
    /// Put a device somewhere. Needs one chosen from the list first.
    Device,
}

impl Tool {
    fn opening(self) -> Option<OpeningKind> {
        match self {
            Tool::Door => Some(OpeningKind::Door),
            Tool::Window => Some(OpeningKind::Window),
            _ => None,
        }
    }

    /// What this tool is for, shown under the canvas so the page needn't be learned twice.
    fn hint(self) -> &'static str {
        match self {
            Tool::Select => {
                "Click something to pick it up. Drag it to move it, or press Delete to take it \
                 away. Drag the empty plan to move around, and scroll to zoom."
            }
            Tool::Wall => {
                "Click to start a wall, then click for each corner. Right-click or Escape ends \
                 the run. Ends snap to the corners already there."
            }
            Tool::Door => "Click a wall to cut a door into it.",
            Tool::Window => "Click a wall to cut a window into it.",
            Tool::Area => {
                "Choose a room, then click round its corners. Corners land on the walls you've \
                 drawn before they land on the grid. Click the first corner again, or \
                 right-click, to close it."
            }
            Tool::Device => "Choose a device, then click where it lives.",
        }
    }
}

/// The one thing the editor is working on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Wall(usize),
    /// A door or window: which wall, and which of its openings.
    Opening(usize, usize),
    /// A room traced out on this floor.
    Area(usize),
    Device(usize),
}

/// What the pointer is doing between pressing and letting go.
#[derive(Debug, Clone, Copy)]
enum Drag {
    /// Moving the view itself: where the pointer went down, and where the plan was then.
    Pan {
        from: (f64, f64),
        pan: (f64, f64),
    },
    /// Moving one end of a wall. `to_end` picks which.
    Corner {
        wall: usize,
        to_end: bool,
    },
    /// Moving a whole wall, keeping the offset it was grabbed at.
    Wall {
        wall: usize,
        grab: (f64, f64),
    },
    /// Sliding a door or window along the wall it's cut into. It can't leave the wall, which is
    /// the whole reason an opening is a distance rather than a point.
    Opening {
        wall: usize,
        opening: usize,
    },
    /// Moving a whole room, keeping the offset it was grabbed at.
    Area {
        area: usize,
        grab: (f64, f64),
    },
    /// Moving one corner of a room.
    AreaCorner {
        area: usize,
        corner: usize,
    },
    /// Moving a room's name, keeping where on it the pointer went down.
    AreaLabel {
        area: usize,
        grab: (f64, f64),
    },
    Device {
        device: usize,
    },
}

#[component]
pub fn Floorplan() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();

    let editing = RwSignal::new(false);
    // The plan being drawn. Only ever looked at while `editing`; the rest of the time the page
    // shows the live one, so a save from another browser shows up here like any other change.
    let draft = RwSignal::new(irori_types::Floorplan::default());
    let tool = RwSignal::new(Tool::Select);
    let picked = RwSignal::new(None::<Pick>);
    // Where the current run of wall has got to, and where the pointer is, both snapped.
    let running = RwSignal::new(None::<Point>);
    // The corner the run's last committed segment came from — `None` until the run has laid at
    // least one wall, so the live angle at `running` isn't guessed from some unrelated wall that
    // happens to end at the same point.
    let wall_behind = RwSignal::new(None::<Point>);
    let pointer = RwSignal::new(None::<Point>);
    let arming = RwSignal::new(None::<DeviceId>);
    // The room being traced, and which room it is. A shape only becomes part of the plan when
    // it closes, so backing out of one leaves nothing behind.
    let tracing = RwSignal::new(Vec::<Point>::new());
    let arming_area = RwSignal::new(None::<AreaId>);
    // Which floor is being looked at. `None` only while the home has no floors at all.
    let floor = RwSignal::new(None::<FloorId>);
    let snap = RwSignal::new(Snap::Grid);
    // What the next wall drawn will be. Changing a wall's thickness sets this too, so drawing
    // an outside wall, thickening it, and carrying on gives thick walls the rest of the way.
    let thickness = RwSignal::new(Wall::DEFAULT_THICKNESS);
    let view = RwSignal::new(Viewport::default());
    let drag = RwSignal::new(None::<Drag>);
    let dragged = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    // Something that went right and is worth saying anyway — a save that also wrote
    // `devices.toml`. Kept apart from `trouble` so good news never wears the colour of bad.
    let note = RwSignal::new(None::<String>);
    // Where the plan has been and where it can go again. Whole plans rather than a list of
    // changes: a plan is a few kilobytes, and a snapshot can't be wrong about what undoing it
    // means the way a replayed change can.
    let past = RwSignal::new(Vec::<Floorplan>::new());
    let future = RwSignal::new(Vec::<Floorplan>::new());
    let canvas = NodeRef::<Div>::new();
    // Measured when framing the plan, so the panels on the right don't end up sitting over it.
    let side = NodeRef::<Div>::new();

    // What's on screen: the copy while it's being drawn, the home's own plan otherwise.
    let shown = Memo::new(move |_| {
        if editing.get() {
            draft.get()
        } else {
            live.home.get().floorplan.clone()
        }
    });

    // The floors of the home, lowest first, as the core has them.
    let floors = Memo::new(move |_| live.home.get().floors);

    // Which floor to open on: the one remembered in this browser if it's still there, else the
    // lowest. Kept honest as floors come and go, so a floor deleted in Settings doesn't leave
    // the page drawing on something that no longer exists.
    Effect::new(move |_| {
        let floors = floors.get();
        let known = |id: &FloorId| floors.iter().any(|floor| &floor.id == id);
        if floor.get_untracked().is_some_and(|id| known(&id)) {
            return;
        }
        let remembered = crate::devices::stored(FLOOR_KEY)
            .and_then(|id| id.parse::<FloorId>().ok())
            .filter(known);
        floor.set(remembered.or_else(|| floors.first().map(|floor| floor.id.clone())));
    });
    Effect::new(move |_| {
        if let Some(id) = floor.get() {
            crate::devices::remember(FLOOR_KEY, id.as_ref());
        }
    });

    // What's picked up is an index into the floor it was picked up on, and a run of wall is a
    // point on it. Carrying either to another floor would be the editor pointing at whatever
    // happens to sit at that index over there — so changing floors puts everything down.
    Effect::new(move |_| {
        floor.track();
        picked.set(None);
        running.set(None);
        wall_behind.set(None);
        pointer.set(None);
        tracing.set(Vec::new());
        arming.set(None);
        arming_area.set(None);
        drag.set(None);
    });

    // Everything below draws and edits one floor at a time.
    let level = Memo::new(move |_| on_floor(&shown.get(), floor.get().as_ref()));
    // What each aimed sensor on this floor can see. Walls and aim decide it and readings don't,
    // so it is worked out when the plan changes rather than every time a sensor speaks.
    // The doors a sensor says are shut: a radar doesn't see through those. Its own memo, so
    // the fields are traced again when a door moves and not every time anything reports.
    let shut = Memo::new(move |_| {
        let level = level.get();
        live.home
            .with(|home| ambience::shut_doors(&level, &home.states))
    });
    let sightlines = Memo::new(move |_| ambience::sightlines(&level.get(), &shut.get()));
    // The floor under this one, drawn faintly so an upstairs can be lined up with it.
    let beneath = Memo::new(move |_| {
        let floors = floors.get();
        let here = floor.get()?;
        let index = floors.iter().position(|floor| floor.id == here)?;
        let below = floors.get(index.checked_sub(1)?)?;
        let plan = shown.get();
        plan.level(&below.id)
            .filter(|level| !level.walls.is_empty())
            .cloned()
    });

    // Where the canvas is in the window, so a pointer position can be turned into a place on the
    // plan. Read at the moment of the event rather than kept: the sidebar folds, and a canvas
    // that thinks it's still where it was would put walls a few centimetres out.
    let corner = move || {
        canvas.get_untracked().map(|node| {
            let rect = node.get_bounding_client_rect();
            (rect.left(), rect.top())
        })
    };
    let at = move |event: &ev::MouseEvent| -> Option<(f64, f64)> {
        let (left, top) = corner()?;
        Some((
            f64::from(event.client_x()) - left,
            f64::from(event.client_y()) - top,
        ))
    };

    // Frames the whole plan, with a margin. Used once when a drawn home first arrives, and by
    // the Fit button afterwards.
    let fit = move || {
        let Some((low, high)) = extent(&level.get_untracked()) else {
            view.set(Viewport::default());
            return;
        };
        let Some(node) = canvas.get_untracked() else {
            return;
        };
        let rect = node.get_bounding_client_rect();
        let (width, height) = (rect.width(), rect.height());
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        // Fitting means fitting into what's *left* of the canvas: the title sits over the top,
        // the tools down the left, the zoom and the hint along the bottom, and the floors and
        // the inspector down the right. A plan framed into the whole rectangle would come out
        // with a wall under the panels, which is exactly what "Fit" is pressed to undo.
        let margin = 24.0;
        let gutters = Insets {
            top: 56.0 + margin,
            bottom: 56.0 + margin,
            left: 56.0 + margin,
            // Measured rather than guessed: the column is as wide as its widest floor name, and
            // isn't there at all for a home with one floor and nothing picked up.
            right: side
                .get_untracked()
                .map_or(0.0, |node| node.get_bounding_client_rect().width())
                + margin * 2.0,
        };
        let room = (
            width - gutters.left - gutters.right,
            height - gutters.top - gutters.bottom,
        );
        if room.0 <= 0.0 || room.1 <= 0.0 {
            return;
        }
        // Every coordinate becomes a `f64` before any arithmetic: a plan can hold points at
        // opposite ends of `i32` (`Point::distance_to` says why), and framing one must give a
        // silly zoom rather than overflow.
        let (left, right) = (f64::from(low.x), f64::from(high.x));
        let (top, bottom) = (f64::from(low.y), f64::from(high.y));
        let (span_x, span_y) = ((right - left).max(100.0), (bottom - top).max(100.0));
        let scale = (room.0 / span_x)
            .min(room.1 / span_y)
            .clamp(MIN_SCALE, MAX_SCALE);
        // The middle of what's left, not the middle of the canvas.
        let middle = (gutters.left + room.0 / 2.0, gutters.top + room.1 / 2.0);
        view.set(Viewport {
            scale,
            pan: (
                middle.0 - (left + right) / 2.0 * scale,
                middle.1 - (top + bottom) / 2.0 * scale,
            ),
        });
    };

    // A home that was already drawn should open framed rather than at whatever the default zoom
    // happens to show. Once only, and never while somebody is drawing: a view that reframed
    // itself as the first wall appeared would move the plan out from under the pointer, and
    // every click after it would land somewhere else than it looked.
    let framed = RwSignal::new(false);
    Effect::new(move |_| {
        let drawn = !on_floor(&live.home.get().floorplan, floor.get().as_ref()).is_empty();
        if framed.get_untracked() || editing.get_untracked() || !drawn || canvas.get().is_none() {
            return;
        }
        framed.set(true);
        fit();
    });

    // Called once at the start of each thing a person does, not once per change: a wall dragged
    // across the room is one move to undo, however many times the pointer reported it.
    let remember = move || {
        past.update(|past| {
            past.push(draft.get_untracked());
            if past.len() > HISTORY {
                past.remove(0);
            }
        });
        // Going somewhere new is what ends a chain of redos, in every program that has them.
        future.update(Vec::clear);
    };

    let step_back = move || {
        past.update(|past| {
            let Some(before) = past.pop() else { return };
            future.update(|future| future.push(draft.get_untracked()));
            draft.set(before);
        });
        // What was picked up may not exist any more, and a half-drawn wall belongs to the state
        // that was undone.
        picked.set(None);
        running.set(None);
        wall_behind.set(None);
        pointer.set(None);
        tracing.set(Vec::new());
    };

    let step_forward = move || {
        future.update(|future| {
            let Some(next) = future.pop() else { return };
            past.update(|past| past.push(draft.get_untracked()));
            draft.set(next);
        });
        picked.set(None);
        running.set(None);
        wall_behind.set(None);
        pointer.set(None);
        tracing.set(Vec::new());
    };

    // The same thing, for the marker and the inspector, which are components rather than
    // closures over this state.
    let remember_cb = Callback::new(move |()| remember());

    // Puts down everything half-drawn: the run of wall, the room being traced, and whatever
    // was waiting to be placed. Nothing half-drawn is part of the plan, so this loses nothing
    // that was ever in it.
    let stop_drawing = move || {
        running.set(None);
        wall_behind.set(None);
        pointer.set(None);
        tracing.set(Vec::new());
    };

    let start_editing = move || {
        draft.set(live.home.get_untracked().floorplan.clone());
        past.set(Vec::new());
        future.set(Vec::new());
        picked.set(None);
        arming.set(None);
        arming_area.set(None);
        tool.set(Tool::Select);
        stop_drawing();
        trouble.set(None);
        note.set(None);
        editing.set(true);
    };

    let cancel = move || {
        editing.set(false);
        past.set(Vec::new());
        future.set(Vec::new());
        picked.set(None);
        arming.set(None);
        arming_area.set(None);
        stop_drawing();
        trouble.set(None);
        note.set(None);
    };

    let save = move || {
        // A floor somebody opened, drew on, and cleared again shouldn't leave a heading in the
        // file for a plan that isn't there.
        draft.update(Floorplan::tidy);
        let plan = draft.get_untracked();
        saving.set(true);
        spawn_local(async move {
            match api::save_floorplan(&plan).await {
                Ok(saved) => {
                    trouble.set(None);
                    // A plan that moved devices into rooms says so: it wrote `devices.toml` as
                    // well as the plan, and a config write nobody was told about is a surprise.
                    note.set(match saved.placed {
                        0 => None,
                        1 => Some("Saved. One device is now in the room it stands in.".into()),
                        many => Some(format!(
                            "Saved. {many} devices are now in the rooms they stand in."
                        )),
                    });
                    editing.set(false);
                    past.set(Vec::new());
                    future.set(Vec::new());
                    picked.set(None);
                    arming.set(None);
                    arming_area.set(None);
                    running.set(None);
                    wall_behind.set(None);
                    pointer.set(None);
                    tracing.set(Vec::new());
                    // The plan the page shows now comes from the home again, so fetch it rather
                    // than waiting up to two seconds to agree with what was just saved.
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
            saving.set(false);
        });
    };

    let remove_picked = move || {
        let Some(pick) = picked.get_untracked() else {
            return;
        };
        remember();
        on_level(draft, floor, |level| match pick {
            Pick::Wall(w) => {
                if w < level.walls.len() {
                    level.walls.remove(w);
                }
            }
            Pick::Opening(w, o) => {
                if let Some(wall) = level.walls.get_mut(w)
                    && o < wall.openings.len()
                {
                    wall.openings.remove(o);
                }
            }
            Pick::Area(a) => {
                if a < level.areas.len() {
                    level.areas.remove(a);
                }
            }
            Pick::Device(d) => {
                if d < level.devices.len() {
                    level.devices.remove(d);
                }
            }
        });
        picked.set(None);
    };

    // Escape backs out of whatever is in hand; Delete takes away what's picked up. Only while
    // editing, and never while a text field has the keyboard — there are none on this page
    // today, and the guard is what keeps that from becoming a bug when there are.
    let keys = window_event_listener(ev::keydown, move |event: ev::KeyboardEvent| {
        if !editing.get_untracked() || typing() {
            return;
        }
        match event.key().as_str() {
            "Escape" => {
                if running.get_untracked().is_some() || !tracing.get_untracked().is_empty() {
                    stop_drawing();
                } else if arming.get_untracked().is_some() || arming_area.get_untracked().is_some()
                {
                    arming.set(None);
                    arming_area.set(None);
                } else {
                    picked.set(None);
                }
            }
            "Delete" | "Backspace" if picked.get_untracked().is_some() => {
                event.prevent_default();
                remove_picked();
            }
            // Both spellings of redo, because both are somebody's habit.
            "z" | "Z" if event.meta_key() || event.ctrl_key() => {
                event.prevent_default();
                if event.shift_key() {
                    step_forward();
                } else {
                    step_back();
                }
            }
            "y" | "Y" if event.ctrl_key() => {
                event.prevent_default();
                step_forward();
            }
            _ => {}
        }
    });
    on_cleanup(move || keys.remove());

    let on_down = move |event: ev::MouseEvent| {
        let Some(screen) = at(&event) else { return };
        dragged.set(false);
        let here = view.get_untracked();
        if !editing.get_untracked() || tool.get_untracked() != Tool::Select {
            // Every other tool acts on the click, not the press; the view still pans.
            drag.set(Some(Drag::Pan {
                from: screen,
                pan: here.pan,
            }));
            return;
        }
        let world = here.world(screen.0, screen.1);
        let here_level = level.get_untracked();
        let reach = REACH * here.cm_per_pixel();
        // Handles are the smallest thing on the canvas to aim at, so they get a wider reach than
        // the things they belong to.
        let handle = reach * 1.5;
        let near = |point: Point| {
            (world.0 - f64::from(point.x)).hypot(world.1 - f64::from(point.y)) <= handle
        };

        // A corner of whatever is already picked up comes first: the handles are drawn on top
        // of everything else, so they should be caught before it.
        match picked.get_untracked() {
            Some(Pick::Wall(w)) => {
                if let Some(wall) = here_level.walls.get(w) {
                    for (to_end, end) in [(false, wall.from), (true, wall.to)] {
                        if near(end) {
                            remember();
                            drag.set(Some(Drag::Corner { wall: w, to_end }));
                            return;
                        }
                    }
                }
            }
            Some(Pick::Area(a)) => {
                if let Some(area) = here_level.areas.get(a)
                    && let Some(corner) = area.points.iter().position(|point| near(*point))
                {
                    remember();
                    drag.set(Some(Drag::AreaCorner { area: a, corner }));
                    return;
                }
            }
            _ => {}
        }

        match pick_at(&here_level, world, reach) {
            Some(Pick::Opening(w, o)) => {
                picked.set(Some(Pick::Opening(w, o)));
                remember();
                drag.set(Some(Drag::Opening {
                    wall: w,
                    opening: o,
                }));
            }
            Some(Pick::Wall(w)) => {
                picked.set(Some(Pick::Wall(w)));
                remember();
                drag.set(Some(Drag::Wall {
                    wall: w,
                    grab: world,
                }));
            }
            Some(Pick::Area(a)) => {
                picked.set(Some(Pick::Area(a)));
                remember();
                drag.set(Some(Drag::Area {
                    area: a,
                    grab: world,
                }));
            }
            Some(Pick::Device(_)) | None => {
                picked.set(None);
                drag.set(Some(Drag::Pan {
                    from: screen,
                    pan: here.pan,
                }));
            }
        }
    };

    let on_move = move |event: ev::MouseEvent| {
        let Some(screen) = at(&event) else { return };
        let here = view.get_untracked();
        let world = here.world(screen.0, screen.1);

        if editing.get_untracked() {
            let tool = tool.get_untracked();
            if running.get_untracked().is_some() {
                let to = place(
                    &level.get_untracked(),
                    world,
                    here,
                    snap.get_untracked(),
                    None,
                );
                pointer.set(Some(to));
            } else if tool == Tool::Area && !tracing.get_untracked().is_empty() {
                let to = trace_at(&level.get_untracked(), world, here, snap.get_untracked());
                pointer.set(Some(to));
            }
        }

        let Some(holding) = drag.get_untracked() else {
            return;
        };
        match holding {
            Drag::Pan { from, pan } => {
                let moved = (screen.0 - from.0, screen.1 - from.1);
                if moved.0.abs() + moved.1.abs() > 2.0 {
                    dragged.set(true);
                }
                view.set(Viewport {
                    scale: here.scale,
                    pan: (pan.0 + moved.0, pan.1 + moved.1),
                });
            }
            Drag::Corner { wall, to_end } => {
                dragged.set(true);
                let here_level = level.get_untracked();
                let Some(was) = here_level.walls.get(wall).map(|wall| ends(wall, to_end)) else {
                    return;
                };
                let to = place(&here_level, world, here, snap.get_untracked(), Some(was));
                on_level(draft, floor, |level| shift(level, &[(was, to)]));
            }
            Drag::Wall { wall, grab } => {
                dragged.set(true);
                let step = snap.get_untracked().step();
                let by = (round(world.0 - grab.0, step), round(world.1 - grab.1, step));
                if by == (0, 0) {
                    return;
                }
                // Saturating for the same reason `fit` converts before it subtracts: a plan
                // can hold a point at the end of `i32`, and dragging it further should stop
                // rather than panic.
                let moved = |point: Point| {
                    Point::new(point.x.saturating_add(by.0), point.y.saturating_add(by.1))
                };
                on_level(draft, floor, |level| {
                    let Some(wall) = level.walls.get(wall) else {
                        return;
                    };
                    let (from, to) = (wall.from, wall.to);
                    shift(level, &[(from, moved(from)), (to, moved(to))]);
                });
                // The grab moves with the wall, so the rounding can't accumulate into a drift.
                drag.set(Some(Drag::Wall {
                    wall,
                    grab: (grab.0 + f64::from(by.0), grab.1 + f64::from(by.1)),
                }));
            }
            Drag::Opening { wall, opening } => {
                dragged.set(true);
                on_level(draft, floor, |level| {
                    if let Some(wall) = level.walls.get_mut(wall) {
                        let length = wall.length();
                        if let Some(hole) = wall.openings.get_mut(opening) {
                            let (along, _) = on_wall_at(wall.from, wall.to, world);
                            hole.at = fit_opening(along, hole.width, length);
                        }
                    }
                });
            }
            Drag::Area { area, grab } => {
                dragged.set(true);
                let step = snap.get_untracked().step();
                let by = (round(world.0 - grab.0, step), round(world.1 - grab.1, step));
                if by == (0, 0) {
                    return;
                }
                on_level(draft, floor, |level| {
                    if let Some(placed) = level.areas.get_mut(area) {
                        for point in &mut placed.points {
                            *point = Point::new(
                                point.x.saturating_add(by.0),
                                point.y.saturating_add(by.1),
                            );
                        }
                    }
                });
                drag.set(Some(Drag::Area {
                    area,
                    grab: (grab.0 + f64::from(by.0), grab.1 + f64::from(by.1)),
                }));
            }
            Drag::AreaCorner { area, corner } => {
                dragged.set(true);
                // A corner being dragged mustn't catch on itself, so it is left out of what the
                // snapping looks at — the same rule a wall's own end is held to.
                let mut without = level.get_untracked();
                if let Some(placed) = without.areas.get_mut(area)
                    && corner < placed.points.len()
                {
                    placed.points.remove(corner);
                }
                let to = trace_at(&without, world, here, snap.get_untracked());
                on_level(draft, floor, |level| {
                    if let Some(point) = level
                        .areas
                        .get_mut(area)
                        .and_then(|placed| placed.points.get_mut(corner))
                    {
                        *point = to;
                    }
                });
            }
            Drag::Device { device } => {
                dragged.set(true);
                let step = snap.get_untracked().step();
                let to = Point::new(round(world.0, step), round(world.1, step));
                on_level(draft, floor, |level| {
                    if let Some(placed) = level.devices.get_mut(device) {
                        placed.at = to;
                    }
                });
            }
            Drag::AreaLabel { area, grab } => {
                dragged.set(true);
                let step = snap.get_untracked().step();
                let by = (round(world.0 - grab.0, step), round(world.1 - grab.1, step));
                if by == (0, 0) {
                    return;
                }
                // A label dragged out of its own room would be pointing at nothing, so a
                // candidate that lands outside the polygon is dropped rather than persisted.
                let mut moved = false;
                on_level(draft, floor, |level| {
                    if let Some(placed) = level.areas.get_mut(area) {
                        let candidate = Point::new(
                            placed.label.x.saturating_add(by.0),
                            placed.label.y.saturating_add(by.1),
                        );
                        let inside = placed.middle().is_some_and(|middle| {
                            placed.contains(Point::new(
                                middle.x.saturating_add(candidate.x),
                                middle.y.saturating_add(candidate.y),
                            ))
                        });
                        if inside {
                            placed.label = candidate;
                            moved = true;
                        }
                    }
                });
                if !moved {
                    return;
                }
                // The grab moves with the label, so the rounding can't accumulate into a drift.
                drag.set(Some(Drag::AreaLabel {
                    area,
                    grab: (grab.0 + f64::from(by.0), grab.1 + f64::from(by.1)),
                }));
            }
        }
    };

    // Buttons that have just been pressed and doorbells that have just rung, for the moment
    // their markers flash. Counted rather than flagged, so a second press while the first flash
    // is still fading starts it again instead of being lost in it.
    let flashes = RwSignal::new(BTreeMap::<DeviceId, u32>::new());
    // When each event entity last said something happened. Only a *change* is a happening: the
    // first sight of one is just the page arriving, and nothing plays as the page arrives.
    let heard = StoredValue::new(BTreeMap::<EntityId, Timestamp>::new());
    Effect::new(move |_| {
        let home = live.home.get();
        let mut rang = Vec::new();
        heard.update_value(|heard| {
            for entity in &home.entities {
                if !matches!(entity.capabilities, Capabilities::Event(_)) {
                    continue;
                }
                let Some(device) = entity.device_id.clone() else {
                    continue;
                };
                let Some(state) = home
                    .states
                    .iter()
                    .find(|state| state.entity_id == entity.id)
                    .filter(|state| state.state.is_some())
                else {
                    continue;
                };
                let before = heard.insert(entity.id.clone(), state.last_changed);
                if before.is_some_and(|before| before != state.last_changed) {
                    rang.push(device);
                }
            }
        });
        for device in rang {
            let mut count = 0;
            flashes.update(|flashes| {
                let entry = flashes.entry(device.clone()).or_insert(0);
                *entry += 1;
                count = *entry;
            });
            set_timeout(
                move || {
                    flashes.update(|flashes| {
                        if flashes.get(&device) == Some(&count) {
                            flashes.remove(&device);
                        }
                    });
                },
                std::time::Duration::from_millis(700),
            );
        }
    });

    // The marker just put down, for the moment it takes to settle.
    let dropped = RwSignal::new(None::<usize>);
    let on_up = move |_: ev::MouseEvent| {
        if let Some(Drag::Device { device }) = drag.get_untracked() {
            dropped.set(Some(device));
            set_timeout(
                move || {
                    if dropped.get_untracked() == Some(device) {
                        dropped.set(None);
                    }
                },
                std::time::Duration::from_millis(600),
            );
        }
        drag.set(None);
    };

    // A click that was really the end of a drag isn't a click: panning the view an inch and
    // then finding a new wall corner there would be maddening.
    let on_click = move |event: ev::MouseEvent| {
        let Some(screen) = at(&event) else { return };
        if !editing.get_untracked() || dragged.get_untracked() {
            return;
        }
        let here = view.get_untracked();
        let world = here.world(screen.0, screen.1);
        match tool.get_untracked() {
            Tool::Select => {}
            Tool::Wall => {
                let to = place(
                    &level.get_untracked(),
                    world,
                    here,
                    snap.get_untracked(),
                    None,
                );
                match running.get_untracked() {
                    Some(from) if from != to => {
                        remember();
                        let built = Wall {
                            thickness: thickness.get_untracked(),
                            ..Wall::new(from, to)
                        };
                        on_level(draft, floor, |level| level.walls.push(built));
                        wall_behind.set(Some(from));
                        running.set(Some(to));
                    }
                    // The first click of a run, or a second click in the same spot, which would
                    // be a wall with no length.
                    _ => running.set(Some(to)),
                }
                pointer.set(Some(to));
            }
            Tool::Door | Tool::Window => {
                let Some(kind) = tool.get_untracked().opening() else {
                    return;
                };
                let reach = REACH * here.cm_per_pixel() * 2.0;
                let Some(w) = nearest_wall(&level.get_untracked(), world, reach) else {
                    trouble.set(Some(format!(
                        "A {} goes in a wall. Click on one.",
                        kind.label().to_lowercase()
                    )));
                    return;
                };
                trouble.set(None);
                remember();
                on_level(draft, floor, |level| {
                    if let Some(wall) = level.walls.get_mut(w) {
                        let length = wall.length();
                        let (along, _) = on_wall_at(wall.from, wall.to, world);
                        // A wall too short for the usual door gets a door the width of the wall
                        // rather than a refusal: whoever drew it can widen the wall later.
                        let width = kind.default_width().min(length.floor().max(1.0) as u32);
                        wall.openings.push(Opening::new(
                            kind,
                            fit_opening(along, width, length),
                            width,
                        ));
                        picked.set(Some(Pick::Opening(w, wall.openings.len() - 1)));
                    }
                });
            }
            Tool::Area => {
                let Some(area) = arming_area.get_untracked() else {
                    trouble.set(Some("Choose a room first, then click round it.".into()));
                    return;
                };
                trouble.set(None);
                let to = trace_at(&level.get_untracked(), world, here, snap.get_untracked());
                let mut corners = tracing.get_untracked();
                // Clicking the first corner again closes the shape, which is how anybody who has
                // ever drawn a polygon expects to finish one.
                if corners.len() >= PlacedArea::FEWEST_POINTS && corners.first() == Some(&to) {
                    let points = corners;
                    remember();
                    on_level(draft, floor, |level| {
                        // Redrawing a room replaces its old shape: one shape per room per floor
                        // is what the plan allows, and moving a wall is why somebody would. The
                        // label is attached to the room, not the shape, so its offset survives.
                        let label = level
                            .areas
                            .iter()
                            .find(|placed| placed.area == area)
                            .map_or(Point::new(0, 0), |placed| placed.label);
                        level.areas.retain(|placed| placed.area != area);
                        level.areas.push(PlacedArea {
                            area: area.clone(),
                            points,
                            label,
                        });
                        picked.set(Some(Pick::Area(level.areas.len() - 1)));
                    });
                    stop_drawing();
                    arming_area.set(None);
                    return;
                }
                // Two clicks in the same place is one corner, not a corner with no length.
                if corners.last() != Some(&to) {
                    corners.push(to);
                    tracing.set(corners);
                }
                pointer.set(Some(to));
            }
            Tool::Device => {
                let Some(device) = arming.get_untracked() else {
                    trouble.set(Some(
                        "Choose a device first, then click where it is.".into(),
                    ));
                    return;
                };
                trouble.set(None);
                remember();
                let step = snap.get_untracked().step();
                let at = Point::new(round(world.0, step), round(world.1, step));
                // A device is in one place. Putting it down here takes it off wherever it was,
                // including another floor — the picker offers every device whatever floor is
                // showing, so this is the ordinary way to move one upstairs.
                let here = floor.get_untracked();
                // Where it points goes with it: moving a radar across the room, or upstairs,
                // isn't a reason to have to aim it again.
                let before = draft.with_untracked(|plan| {
                    plan.floors
                        .values()
                        .flat_map(|level| &level.devices)
                        .find(|placed| placed.device == device)
                        .cloned()
                });
                draft.update(|plan| {
                    for (id, level) in &mut plan.floors {
                        if Some(id) != here.as_ref() {
                            level.devices.retain(|placed| placed.device != device);
                        }
                    }
                });
                on_level(draft, floor, |level| {
                    level.devices.retain(|placed| placed.device != device);
                    level.devices.push(match before {
                        Some(before) => PlacedDevice { at, ..before },
                        None => PlacedDevice::new(device, at),
                    });
                    picked.set(Some(Pick::Device(level.devices.len() - 1)));
                });
                arming.set(None);
            }
        }
    };

    // Right-click backs out, the way it ends a run of points in every drawing program: it stops
    // the wall being drawn, and stops a device waiting to be put down. The browser's own menu is
    // held back only while editing — there's nothing to back out of while reading the plan, and
    // taking the menu away then would be taking something for nothing.
    let on_right_click = move |event: ev::MouseEvent| {
        if !editing.get_untracked() {
            return;
        }
        event.prevent_default();
        let corners = tracing.get_untracked();
        if !corners.is_empty() {
            // Enough corners and it is a room; too few and there was never a shape to keep.
            if corners.len() >= PlacedArea::FEWEST_POINTS
                && let Some(area) = arming_area.get_untracked()
            {
                remember();
                on_level(draft, floor, |level| {
                    // Same rule as the click-to-close path: keep the room's label offset across
                    // the shape it just replaced.
                    let label = level
                        .areas
                        .iter()
                        .find(|placed| placed.area == area)
                        .map_or(Point::new(0, 0), |placed| placed.label);
                    level.areas.retain(|placed| placed.area != area);
                    level.areas.push(PlacedArea {
                        area: area.clone(),
                        points: corners,
                        label,
                    });
                    picked.set(Some(Pick::Area(level.areas.len() - 1)));
                });
                arming_area.set(None);
            }
            stop_drawing();
        } else if running.get_untracked().is_some() {
            stop_drawing();
        } else if arming.get_untracked().is_some() || arming_area.get_untracked().is_some() {
            arming.set(None);
            arming_area.set(None);
        } else {
            picked.set(None);
        }
    };

    // Zoom around the pointer, so the thing being looked at stays under it.
    let on_wheel = move |event: ev::WheelEvent| {
        event.prevent_default();
        let Some((left, top)) = corner() else { return };
        let screen = (
            f64::from(event.client_x()) - left,
            f64::from(event.client_y()) - top,
        );
        view.update(|here| {
            let step = if event.delta_y() < 0.0 {
                1.12
            } else {
                1.0 / 1.12
            };
            let scale = (here.scale * step).clamp(MIN_SCALE, MAX_SCALE);
            let factor = scale / here.scale;
            here.pan = (
                screen.0 - (screen.0 - here.pan.0) * factor,
                screen.1 - (screen.1 - here.pan.1) * factor,
            );
            here.scale = scale;
        });
    };

    let zoom_by = move |step: f64| {
        let Some(node) = canvas.get_untracked() else {
            return;
        };
        let rect = node.get_bounding_client_rect();
        let middle = (rect.width() / 2.0, rect.height() / 2.0);
        view.update(|here| {
            let scale = (here.scale * step).clamp(MIN_SCALE, MAX_SCALE);
            let factor = scale / here.scale;
            here.pan = (
                middle.0 - (middle.0 - here.pan.0) * factor,
                middle.1 - (middle.1 - here.pan.1) * factor,
            );
            here.scale = scale;
        });
    };

    view! {
        <div class="floorplan" class:editing=move || editing.get()>
            <div
                class="canvas"
                class:drawing=move || editing.get() && tool.get() != Tool::Select
                node_ref=canvas
                on:mousedown=on_down
                on:mousemove=on_move
                on:mouseup=on_up
                on:mouseleave=on_up
                on:click=on_click
                on:contextmenu=on_right_click
                on:wheel=on_wheel
            >
                <svg class="plan" aria-hidden="true">
                    {move || editing.get().then(|| grid(view.get(), snap.get()))}
                    // The floor below, faintly, so an upstairs can be lined up with what holds
                    // it up. Only while drawing: reading a plan, it would just be clutter.
                    {move || {
                        let below = editing.get().then(|| beneath.get()).flatten()?;
                        Some(ghost(&below, view.get()))
                    }}
                    {move || {
                        let here = view.get();
                        let level = level.get();
                        let chosen = editing.get().then(|| picked.get()).flatten();
                        level
                            .areas
                            .iter()
                            .enumerate()
                            .map(|(a, placed)| {
                                drawn_area(placed, here, chosen == Some(Pick::Area(a)))
                            })
                            .collect_view()
                    }}
                    // Light and movement in the rooms, under the walls. Not while drawing,
                    // where it would only get in the way of the lines.
                    {move || {
                        (!editing.get()).then(|| {
                            ambience::ambience(
                                &level.get(),
                                &live.home.get(),
                                &sightlines.get(),
                                transform(view.get()),
                            )
                        })
                    }}
                    // While drawing, only what's being aimed: every radar's field at its full
                    // reach, so turning one shows what it will be looking at.
                    {move || {
                        editing.get().then(|| view! {
                            <g class="ambience" transform=transform(view.get())>
                                {ambience::fields(
                                    &level.get(),
                                    &live.home.get(),
                                    &sightlines.get(),
                                    true,
                                )}
                            </g>
                        })
                    }}
                    {move || {
                        let here = view.get();
                        let level = level.get();
                        let chosen = editing.get().then(|| picked.get()).flatten();
                        // Doors and windows follow their sensors while the plan is being read.
                        let home = (!editing.get()).then(|| live.home.get());
                        let states = home.as_ref().map(|home| home.states.as_slice());
                        level
                            .walls
                            .iter()
                            .enumerate()
                            .map(|(w, wall)| {
                                drawn_wall(wall, w, finishes(&level, w), here, chosen, states)
                            })
                            .collect_view()
                    }}
                    // The inside of each corner a single curve couldn't carry, rounded off.
                    {move || drawn_fillets(&fillets(&level.get()), view.get())}
                    {move || {
                        let (Some(from), Some(to)) = (running.get(), pointer.get()) else {
                            return None;
                        };
                        Some(pending(from, to, view.get()))
                    }}
                    {move || {
                        let corners = tracing.get();
                        (!corners.is_empty()).then(|| tracing_shape(&corners, pointer.get(), view.get()))
                    }}
                    {move || {
                        let (behind, node) = drawing_junction(
                            tool.get(),
                            running.get(),
                            wall_behind.get(),
                            &tracing.get(),
                        )?;
                        let to = pointer.get()?;
                        // The pointer back on the corner is no angle at all, just a line still
                        // at its start.
                        if node.distance_to(to) < 0.5 {
                            return None;
                        }
                        // The corner's own arc, between the wall already there and the line
                        // reaching for the pointer, tracing the angle the label says in words.
                        let points = arc_points(behind, node, to, ANGLE_ARC);
                        if points.is_empty() {
                            return None;
                        }
                        let joined = points
                            .iter()
                            .map(|[x, y]| format!("{x},{y}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        Some(view! {
                            <g class="angle-arc" transform=transform(view.get())>
                                <polyline
                                    points=joined
                                    vector-effect="non-scaling-stroke"
                                />
                            </g>
                        })
                    }}
                    {move || {
                        let chosen = editing.get().then(|| picked.get()).flatten();
                        let level = level.get();
                        let corners: Vec<Point> = match chosen? {
                            Pick::Wall(w) => {
                                let wall = level.walls.get(w)?;
                                vec![wall.from, wall.to]
                            }
                            Pick::Area(a) => level.areas.get(a)?.points.clone(),
                            _ => return None,
                        };
                        Some(handles(&corners, view.get()))
                    }}
                </svg>

                // Not while editing: once somebody has pressed Edit, the canvas should be the
                // empty surface they're drawing on.
                {move || (!editing.get() && level.get().is_empty()).then(|| view! {
                    <EmptyPlan floors=floors live=live trouble=trouble />
                })}

                <div class="markers">
                    // A room's name, in the middle of it unless it's been dragged somewhere else
                    // on the room. HTML rather than drawn, so its size doesn't follow the zoom;
                    // and while editing it is a thing to grab, which is why `.movable` (the
                    // style that turns a name's pointer-events back on) follows `editing`.
                    {move || {
                        let home = live.home.get();
                        let here = view.get();
                        let is_editing = editing.get();
                        level.get()
                            .areas
                            .iter()
                            .enumerate()
                            .filter_map(|(index, placed)| {
                                let area = home.area(&placed.area)?;
                                let label_point = label_at(placed)?;
                                let (x, y) = here.screen(label_point);
                                // Falls back to the label's own centre only if the event carries
                                // no usable position, so a click at either end of the text still
                                // grabs the point under the pointer rather than snapping the
                                // label's centre there on the first move.
                                let fallback = (f64::from(label_point.x), f64::from(label_point.y));
                                Some(view! {
                                    <span
                                        class="room-label"
                                        class:movable=is_editing
                                        style=format!("left:{x}px;top:{y}px")
                                        on:mousedown=move |event: ev::MouseEvent| {
                                            if !is_editing {
                                                return;
                                            }
                                            event.stop_propagation();
                                            dragged.set(false);
                                            picked.set(Some(Pick::Area(index)));
                                            remember();
                                            let grab = at(&event)
                                                .map(|screen| {
                                                    view.get_untracked().world(screen.0, screen.1)
                                                })
                                                .unwrap_or(fallback);
                                            drag.set(Some(Drag::AreaLabel { area: index, grab }));
                                        }
                                        on:click=move |event: ev::MouseEvent| {
                                            event.stop_propagation();
                                        }
                                    >
                                        {area.name.to_string()}
                                    </span>
                                })
                            })
                            .collect_view()
                    }}
                    // A door or window whose sensor has stopped answering wears the same
                    // struck-through signal a device does, in the middle of its gap. Only while
                    // reading the plan: drawing it, nothing follows its sensor.
                    {move || {
                        if editing.get() {
                            return None;
                        }
                        let home = live.home.get();
                        let here = view.get();
                        let level = level.get();
                        Some(
                            level
                                .walls
                                .iter()
                                .flat_map(|wall| {
                                    wall.openings.iter().map(move |opening| (wall, opening))
                                })
                                .filter(|(_, opening)| {
                                    standing(opening, &home.states) == Standing::Unknown
                                })
                                .map(|(wall, opening)| {
                                    let (middle, _, _) = along(wall, f64::from(opening.at));
                                    let (x, y) = (
                                        here.pan.0 + middle.0 * here.scale,
                                        here.pan.1 + middle.1 * here.scale,
                                    );
                                    let words = format!(
                                        "This {}'s sensor isn't answering",
                                        opening.kind.label().to_lowercase()
                                    );
                                    view! {
                                        <span
                                            class="lost on-plan"
                                            role="img"
                                            aria-label=words.clone()
                                            title=words
                                            style=format!("left:{x}px;top:{y}px")
                                        >
                                            {icon(crate::icons::Icon::NoSignal)}
                                        </span>
                                    }
                                })
                                .collect_view(),
                        )
                    }}
                    {move || {
                        let home = live.home.get();
                        let here = view.get();
                        let is_editing = editing.get();
                        let chosen = is_editing.then(|| picked.get()).flatten();
                        level.get()
                            .devices
                            .iter()
                            .enumerate()
                            .filter_map(|(index, placed)| {
                                let device = home
                                    .devices
                                    .iter()
                                    .find(|device| device.id == placed.device)?;
                                Some(marker(
                                    index,
                                    placed,
                                    device,
                                    &home,
                                    here,
                                    is_editing,
                                    chosen == Some(Pick::Device(index)),
                                    controls,
                                    picked,
                                    drag,
                                    dragged,
                                    dropped,
                                    flashes,
                                    remember_cb,
                                ))
                            })
                            .collect_view()
                    }}
                </div>

                {move || {
                    let to = pointer.get()?;
                    let from = running.get().or_else(|| tracing.get().last().copied())?;
                    let here = view.get();
                    let (x, y) = here.screen(to);
                    Some(view! {
                        <span class="measure" style=format!("left:{x}px;top:{y}px")>
                            {metres(from.distance_to(to))}
                        </span>
                    })
                }}

                // The angle of the corner the next line turns on, updating as it is dragged: at
                // the node the new segment shares with the one before it, between that wall — or
                // that side of a room — and the line reaching for the pointer. Straight through
                // reads as 180°, a square corner as 90°, and a line that ran back over the wall
                // as 0°.
                {move || {
                    if !editing.get() {
                        return None;
                    }
                    let to = pointer.get()?;
                    let (behind, node) = drawing_junction(
                        tool.get(),
                        running.get(),
                        wall_behind.get(),
                        &tracing.get(),
                    )?;
                    // The pointer back on the corner is no angle at all, just a line still at
                    // its start.
                    if node.distance_to(to) < 0.5 {
                        return None;
                    }
                    // Where the label goes: along the bisector of the corner, which for a square
                    // corner is the 45° line into the room, and for a straight-through wall
                    // reads as ahead of it. A corner that is a straight line has no bisector to
                    // speak of, so it gets a label to one side instead.
                    let (bx0, by0) = direction(node, behind);
                    let (bx1, by1) = direction(node, to);
                    let (mut bx, mut by) = (bx0 + bx1, by0 + by1);
                    let reach = bx.hypot(by);
                    if reach < 1e-6 {
                        (bx, by) = (-by0, bx0);
                    } else {
                        (bx, by) = (bx / reach, by / reach);
                    }
                    let here = view.get();
                    let (x, y) = here.screen(node);
                    let (lx, ly) = (x + bx * ANGLE_OFFSET, y + by * ANGLE_OFFSET);
                    Some(view! {
                        <span class="angle" style=format!("left:{lx}px;top:{ly}px")>
                            {format!("{:.0}°", angle_at(behind, node, to))}
                        </span>
                    })
                }}
            </div>

            <div class="plan-head">
                <h1>"Floorplan"</h1>
                <div class="plan-actions">
                    {move || if editing.get() {
                        view! {
                            <>
                                <button
                                    type="button"
                                    // Not while a save is in flight. Cancelling would let a new
                                    // edit start before the answer came back, and the answer —
                                    // which closes the editor and empties the history — would
                                    // land on that new edit instead of the one it belonged to.
                                    disabled=move || saving.get()
                                    on:click=move |_| cancel()
                                >
                                    "Cancel"
                                </button>
                                <button
                                    type="button"
                                    class="solid"
                                    disabled=move || saving.get()
                                    on:click=move |_| save()
                                >
                                    {move || if saving.get() { "Saving…" } else { "Save" }}
                                </button>
                            </>
                        }.into_any()
                    } else if floor.get().is_some() {
                        view! {
                            <button type="button" on:click=move |_| start_editing()>"Edit"</button>
                        }.into_any()
                    } else {
                        // Nowhere to draw yet. The canvas says why, and offers the fix.
                        ().into_any()
                    }}
                </div>
            </div>

            {move || editing.get().then(|| {
                let bar = NodeRef::<leptos::html::Div>::new();
                // The tool in hand is marked by a highlight that slides between tools.
                crate::glide::glide(bar, ":scope > button.chosen", move || tool.track());
                view! {
                <div class="toolbar" role="toolbar" aria-label="Drawing tools" node_ref=bar>
                    <span class="glide" aria-hidden="true"></span>
                    {TOOLS.iter().map(|(which, label, icon)| {
                        let which = *which;
                        view! {
                            <button
                                type="button"
                                title=*label
                                aria-label=*label
                                aria-pressed=move || (tool.get() == which).to_string()
                                class:chosen=move || tool.get() == which
                                on:click=move |_| {
                                    tool.set(which);
                                    // Whatever was half-drawn belongs to the tool being put
                                    // down, or coming back to it later would finish a shape
                                    // nobody remembers starting.
                                    stop_drawing();
                                    if which != Tool::Device {
                                        arming.set(None);
                                    }
                                    if which != Tool::Area {
                                        arming_area.set(None);
                                    }
                                }
                            >
                                <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=*icon></svg>
                            </button>
                        }
                    }).collect_view()}
                    <span class="tool-gap"></span>
                    <button
                        type="button"
                        title="Undo (⌘Z)"
                        aria-label="Undo"
                        disabled=move || past.get().is_empty()
                        on:click=move |_| step_back()
                    >
                        <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=UNDO></svg>
                    </button>
                    <button
                        type="button"
                        title="Redo (⇧⌘Z)"
                        aria-label="Redo"
                        disabled=move || future.get().is_empty()
                        on:click=move |_| step_forward()
                    >
                        <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=REDO></svg>
                    </button>
                    <span class="tool-gap"></span>
                    <button
                        type="button"
                        title="Remove what's picked up"
                        aria-label="Remove what's picked up"
                        disabled=move || picked.get().is_none()
                        on:click=move |_| remove_picked()
                    >
                        <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=BIN></svg>
                    </button>
                </div>
                }
            })}

            {move || (editing.get() && tool.get() == Tool::Device).then(|| view! {
                <DevicePicker plan=shown arming=arming live=live />
            })}

            {move || (editing.get() && tool.get() == Tool::Area).then(|| view! {
                <AreaPicker level=level floor=floor arming=arming_area live=live />
            })}

            // The right-hand column: which floor, and what's picked up under it. One column so
            // the two can't sit on top of each other, and so `fit` has one thing to measure.
            <div class="plan-side" node_ref=side>
                <FloorPicker floors=floors floor=floor plan=shown live=live trouble=trouble />
                {move || editing.get().then(|| view! {
                    <Inspector draft=draft floor=floor level=level picked=picked
                        thickness=thickness live=live remember=remember_cb />
                })}
            </div>

            <div class="plan-foot">
                <div class="zoom">
                    <button type="button" aria-label="Zoom out" on:click=move |_| zoom_by(1.0 / 1.25)>"−"</button>
                    <button type="button" aria-label="Zoom in" on:click=move |_| zoom_by(1.25)>"+"</button>
                    <button type="button" on:click=move |_| fit()>"Fit"</button>
                </div>
                {move || editing.get().then(|| view! { <SnapControl snap=snap /> })}
                <p class="hint">
                    {move || if floors.get().is_empty() {
                        ""
                    } else if editing.get() {
                        tool.get().hint()
                    } else if level.get().is_empty() {
                        ""
                    } else {
                        "The devices on the plan show what they're doing. Hover one for its name, \
                         and click a light, a plug or a player to switch it."
                    }}
                </p>
            </div>

            {move || trouble.get().map(|why| view! { <p class="plan-banner">{why}</p> })}
            {move || note.get().map(|said| view! {
                <p class="plan-banner note">{said}</p>
            })}
        </div>
    }
}

/// Which floor is being looked at, and how to add another.
///
/// Highest at the top, like the buttons in a lift, because that is the one arrangement of floors
/// nobody has to be taught. A floor with nothing drawn on it says so, so an upstairs that was
/// never traced doesn't look like one that failed to load.
///
/// Adding one from here writes `areas.toml`, the same as Settings does — floors are one thing
/// with one home, and this is the page where it becomes obvious that another is needed.
#[component]
fn FloorPicker(
    floors: Memo<Vec<irori_types::Floor>>,
    floor: RwSignal<Option<FloorId>>,
    plan: Memo<Floorplan>,
    live: crate::Live,
    trouble: RwSignal<Option<String>>,
) -> impl IntoView {
    let adding = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let above = RwSignal::new(String::new());

    // A new floor goes above the highest one unless told otherwise, because that is the one
    // people add.
    let next_level = move || crate::places::next_level(&floors.get());
    let open = move || {
        name.set(String::new());
        above.set(next_level().to_string());
        adding.set(true);
    };

    let add = move || {
        // The same rule, in the same words, as making a floor in Settings.
        let (named, level) =
            match crate::places::floor_draft(&name.get_untracked(), &above.get_untracked()) {
                Ok(floor) => floor,
                Err(why) => {
                    trouble.set(Some(why));
                    return;
                }
            };
        busy.set(true);
        spawn_local(async move {
            match api::add_floor(named, level).await {
                Ok(()) => {
                    trouble.set(None);
                    adding.set(false);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
            busy.set(false);
        });
    };

    move || {
        let floors = floors.get();
        // No floors at all has its own empty state on the canvas, which offers the same button
        // with more room to explain itself.
        if floors.is_empty() {
            return None;
        }
        let plan = plan.get();
        // Each floor's level, for telling up from down when the floor changes.
        let levels: Vec<(FloorId, i8)> = floors.iter().map(|f| (f.id.clone(), f.level)).collect();
        let picker = NodeRef::<leptos::html::Div>::new();
        // Changing floor slides the highlight up or down the list, the way the floors stack.
        crate::glide::glide(picker, ":scope > button.chosen", move || floor.track());
        Some(view! {
            <div class="floor-picker" role="group" aria-label="Floors" node_ref=picker>
                <span class="glide" aria-hidden="true"></span>
                {floors
                    .into_iter()
                    .rev()
                    .map(|level| {
                        let id = level.id.clone();
                        let (chosen, pressed) = (id.clone(), id.clone());
                        let (target, levels) = (level.level, levels.clone());
                        let drawn = plan.level(&level.id).is_some_and(|level| !level.is_empty());
                        view! {
                            <button
                                type="button"
                                class:chosen=move || floor.get().as_ref() == Some(&chosen)
                                aria-pressed=move || {
                                    (floor.get().as_ref() == Some(&pressed)).to_string()
                                }
                                on:click=move |_| {
                                    let now = floor.get_untracked();
                                    if now.as_ref() == Some(&id) {
                                        return;
                                    }
                                    // Up a floor, the plan sinks away and the one above comes
                                    // down into view; down a floor, the other way.
                                    let here = now.and_then(|now| {
                                        levels.iter().find(|(id, _)| *id == now).map(|(_, at)| *at)
                                    });
                                    let up = here.is_some_and(|here| target > here);
                                    let id = id.clone();
                                    crate::transition::around(
                                        if up { "floor-up" } else { "floor-down" },
                                        move || floor.set(Some(id)),
                                    );
                                }
                            >
                                <span class="floor-name">{level.name.to_string()}</span>
                                <span class="floor-level">
                                    {if drawn { "drawn" } else { "empty" }}
                                </span>
                            </button>
                        }
                    })
                    .collect_view()}

                {move || if adding.get() {
                    view! {
                        <form
                            class="add-floor"
                            on:submit=move |event: ev::SubmitEvent| {
                                event.prevent_default();
                                add();
                            }
                        >
                            <label>
                                <span class="visually-hidden">"What the floor is called"</span>
                                <input
                                    type="text"
                                    placeholder="Upstairs"
                                    autofocus
                                    prop:value=move || name.get()
                                    on:input=move |e| name.set(event_target_value(&e))
                                />
                            </label>
                            <label class="level">
                                <span>"Level"</span>
                                <input
                                    type="number"
                                    step="1"
                                    prop:value=move || above.get()
                                    on:input=move |e| above.set(event_target_value(&e))
                                />
                            </label>
                            <div class="add-floor-actions">
                                <button type="button" on:click=move |_| adding.set(false)>
                                    "Cancel"
                                </button>
                                <button type="submit" class="solid" disabled=move || busy.get()>
                                    {move || if busy.get() { "Adding…" } else { "Add" }}
                                </button>
                            </div>
                        </form>
                    }
                    .into_any()
                } else {
                    view! {
                        <button class="add-another" type="button" on:click=move |_| open()>
                            "+ Add a floor"
                        </button>
                    }
                    .into_any()
                }}
            </div>
        })
    }
}

/// The rooms that can be traced on this floor. An area is a thing the home already knows about
/// (Settings makes them); this only gives one a shape.
#[component]
fn AreaPicker(
    level: Memo<Level>,
    floor: RwSignal<Option<FloorId>>,
    arming: RwSignal<Option<AreaId>>,
    live: crate::Live,
) -> impl IntoView {
    view! {
        <div class="device-picker">
            <h2>"Rooms"</h2>
            <ul>
                {move || {
                    let home = live.home.get();
                    let here = floor.get();
                    let traced = level.get();
                    // The rooms of this floor, and the ones nobody has put on a floor at all —
                    // which are the ones somebody drawing this floor might mean.
                    let mut rooms: Vec<_> = home
                        .areas
                        .iter()
                        .filter(|area| area.floor_id.is_none() || area.floor_id == here)
                        .cloned()
                        .collect();
                    rooms.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
                    if rooms.is_empty() {
                        return view! {
                            <li class="muted">"No rooms on this floor yet. Settings makes them."</li>
                        }
                        .into_any();
                    }
                    rooms
                        .into_iter()
                        .map(|area| {
                            let id = area.id.clone();
                            let armed = id.clone();
                            let drawn = traced.areas.iter().any(|placed| placed.area == area.id);
                            view! {
                                <li>
                                    <button
                                        type="button"
                                        class:chosen=move || arming.get().as_ref() == Some(&armed)
                                        on:click=move |_| arming.update(|current| {
                                            *current = if current.as_ref() == Some(&id) {
                                                None
                                            } else {
                                                Some(id.clone())
                                            };
                                        })
                                    >
                                        <span class="name">{area.name.to_string()}</span>
                                        {drawn.then(|| view! {
                                            <span class="badge">"drawn"</span>
                                        })}
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </ul>
        </div>
    }
}

/// How far apart points may land: the grid, or a step of your own.
///
/// Two controls rather than one, because the two settings are asked for differently. Grid is the
/// answer almost always and should cost one glance; a step of your own is a deliberate thing to
/// want, and only then is a slider worth the room it takes.
#[component]
fn SnapControl(snap: RwSignal<Snap>) -> impl IntoView {
    // The switch's highlight slides between the two, as every switcher's does.
    let switcher = NodeRef::<leptos::html::Div>::new();
    crate::glide::across(switcher, "button.chosen", move || {
        snap.with(|snap| snap.is_custom());
    });
    view! {
        <div class="snap">
            <span class="snap-label" id="snap-label">"Snap"</span>
            <div class="switcher" role="group" aria-labelledby="snap-label" node_ref=switcher>
                <span class="glide" aria-hidden="true"></span>
                <button
                    type="button"
                    class:chosen=move || !snap.get().is_custom()
                    aria-pressed=move || (!snap.get().is_custom()).to_string()
                    on:click=move |_| snap.set(Snap::Grid)
                >
                    "Grid"
                </button>
                <button
                    type="button"
                    class:chosen=move || snap.get().is_custom()
                    aria-pressed=move || snap.get().is_custom().to_string()
                    // Starting from whatever the grid is keeps the plan still at the moment of
                    // switching: nothing already drawn moves, and the next point is the first
                    // thing the new step applies to.
                    on:click=move |_| snap.update(|snap| {
                        if !snap.is_custom() {
                            *snap = Snap::Custom(snap.step());
                        }
                    })
                >
                    "Custom"
                </button>
            </div>
            {move || snap.get().is_custom().then(|| view! {
                <label class="snap-step">
                    <span class="visually-hidden">"Snap step in centimetres"</span>
                    <input
                        type="range"
                        min=*SNAP_RANGE.start()
                        max=*SNAP_RANGE.end()
                        step="1"
                        prop:value=move || snap.get().step()
                        style:--fill=move || format!(
                            "{}%",
                            crate::devices::fill(snap.get().step(), *SNAP_RANGE.start(), *SNAP_RANGE.end())
                        )
                        on:input=move |event| {
                            if let Ok(step) = event_target_value(&event).parse::<i32>() {
                                snap.set(Snap::Custom(step));
                            }
                        }
                    />
                    <output>{move || format!("{} cm", snap.get().step())}</output>
                </label>
            })}
        </div>
    }
}

/// What's picked up, and the one or two numbers worth changing about it.
///
/// Only the things a number can say. Where a wall *is* is said by dragging it, which is quicker
/// than any field would be; how thick it is can't be dragged at all, so it lives here.
#[component]
fn Inspector(
    draft: RwSignal<Floorplan>,
    floor: RwSignal<Option<FloorId>>,
    level: Memo<Level>,
    picked: RwSignal<Option<Pick>>,
    thickness: RwSignal<u32>,
    live: crate::Live,
    remember: Callback<()>,
) -> impl IntoView {
    move || {
        let here = level.get();
        match picked.get()? {
            Pick::Wall(w) => {
                let wall = here.walls.get(w)?;
                let (length, thick) = (wall.length(), wall.thickness);
                Some(view! {
                    <div class="inspector">
                        <h2>"Wall"</h2>
                        <p class="measure-row">
                            <span>"Length"</span>
                            <span class="figure">{metres(length)}</span>
                        </p>
                        <label class="slider">
                            <span>"Thickness"</span>
                            <input
                                type="range"
                                min=*Wall::THICKNESS_RANGE.start()
                                max=*Wall::THICKNESS_RANGE.end()
                                step="1"
                                prop:value=thick
                                style:--fill=format!(
                                    "{}%",
                                    crate::devices::fill(
                                        thick,
                                        *Wall::THICKNESS_RANGE.start(),
                                        *Wall::THICKNESS_RANGE.end(),
                                    )
                                )
                                on:pointerdown=move |_| remember.run(())
                                on:keydown=move |_| remember.run(())
                                on:input=move |event| {
                                    let Ok(next) = event_target_value(&event).parse::<u32>() else {
                                        return;
                                    };
                                    let next = next.clamp(
                                        *Wall::THICKNESS_RANGE.start(),
                                        *Wall::THICKNESS_RANGE.end(),
                                    );
                                    // Remembered for the next wall drawn, so a run of outside
                                    // walls needs saying once.
                                    thickness.set(next);
                                    on_level(draft, floor, |level| {
                                        if let Some(wall) = level.walls.get_mut(w) {
                                            wall.thickness = next;
                                        }
                                    });
                                }
                            />
                            <output class="figure">{format!("{thick} cm")}</output>
                        </label>
                    </div>
                }.into_any())
            }
            Pick::Opening(w, o) => {
                let wall = here.walls.get(w)?;
                let opening = wall.openings.get(o)?;
                let (kind, width) = (opening.kind, opening.width);
                let (side, hinge, sensor) = (opening.side, opening.hinge, opening.sensor.clone());
                // An opening can't be wider than the wall it's cut into, so the slider stops
                // where the wall does rather than letting a plan be made that can't be saved.
                let widest = wall.length().floor().max(1.0) as u32;
                // Changes one thing about this opening, as one step to undo.
                let change = move |edit: &dyn Fn(&mut Opening)| {
                    remember.run(());
                    on_level(draft, floor, |level| {
                        if let Some(opening) = level
                            .walls
                            .get_mut(w)
                            .and_then(|wall| wall.openings.get_mut(o))
                        {
                            edit(opening);
                        }
                    });
                };
                // The sensors that could be on a door or a window. Read once per redraw of this
                // panel, not with every reading: a list that reshuffled under an open menu
                // would be worse than one a new sensor takes a click to appear in.
                let contacts = live
                    .home
                    .with_untracked(|home| contacts(home, sensor.as_ref()));
                let following = sensor.as_ref().map(ToString::to_string).unwrap_or_default();
                let following = Signal::derive(move || following.clone());
                Some(view! {
                    <div class="inspector">
                        <h2>{kind.label()}</h2>
                        <label class="slider">
                            <span>"Width"</span>
                            <input
                                type="range"
                                min=1
                                max=widest
                                step="1"
                                prop:value=width
                                style:--fill=format!("{}%", crate::devices::fill(width, 1, widest))
                                on:pointerdown=move |_| remember.run(())
                                on:keydown=move |_| remember.run(())
                                on:input=move |event| {
                                    let Ok(next) = event_target_value(&event).parse::<u32>() else {
                                        return;
                                    };
                                    on_level(draft, floor, |level| {
                                        let Some(wall) = level.walls.get_mut(w) else { return };
                                        let length = wall.length();
                                        let Some(opening) = wall.openings.get_mut(o) else {
                                            return;
                                        };
                                        opening.width =
                                            next.clamp(1, length.floor().max(1.0) as u32);
                                        // Widening one near the end of its wall slides it back
                                        // in rather than letting it hang off.
                                        opening.at = fit_opening(
                                            f64::from(opening.at),
                                            opening.width,
                                            length,
                                        );
                                    });
                                }
                            />
                            <output class="figure">{format!("{width} cm")}</output>
                        </label>
                        <div class="choice">
                            <span>"Opens to"</span>
                            {crate::segmented::segmented(
                                "Which side it opens to",
                                vec![
                                    (irori_types::Side::Left, "Left"),
                                    (irori_types::Side::Right, "Right"),
                                ],
                                Signal::derive(move || side),
                                move |to| change(&|opening| opening.side = to),
                            )}
                        </div>
                        // A window is hinged at both jambs; only a door has an end it hangs from.
                        {(kind == OpeningKind::Door).then(|| view! {
                            <div class="choice">
                                <span>"Hinge"</span>
                                {crate::segmented::segmented(
                                    "Which end it is hinged at",
                                    vec![
                                        (irori_types::Hinge::Near, "Near end"),
                                        (irori_types::Hinge::Far, "Far end"),
                                    ],
                                    Signal::derive(move || hinge),
                                    move |to| change(&|opening| opening.hinge = to),
                                )}
                            </div>
                        })}
                        <div class="choice">
                            <span>"Sensor"</span>
                            <irori_ui_kit::combo::Combo
                                choices=contacts
                                value=following
                                placeholder="Search contact sensors"
                                pick=Callback::new(move |picked: String| {
                                    let picked = picked.parse::<EntityId>().ok();
                                    change(&|opening| opening.sensor = picked.clone());
                                })
                            />
                        </div>
                        <p class="muted small">
                            {if sensor.is_some() {
                                "It opens and shuts on the plan as its sensor says."
                            } else {
                                "Give it a contact sensor and it opens and shuts on the plan."
                            }}
                        </p>
                    </div>
                }.into_any())
            }
            Pick::Area(a) => {
                let placed = here.areas.get(a)?;
                let home = live.home.get();
                let name = home
                    .area(&placed.area)
                    .map(|area| area.name.to_string())
                    // A room that has since been deleted from Settings keeps its shape; saying
                    // its id is more honest than showing nothing.
                    .unwrap_or_else(|| placed.area.to_string());
                let corners = placed.points.len();
                Some(
                    view! {
                        <div class="inspector">
                            <h2>"Room"</h2>
                            <p class="name">{name}</p>
                            <p class="measure-row">
                                <span>"Corners"</span>
                                <span class="figure">{corners.to_string()}</span>
                            </p>
                            <p class="muted small">"Drag it, drag a corner, or drag its name."</p>
                        </div>
                    }
                    .into_any(),
                )
            }
            Pick::Device(d) => {
                let placed = here.devices.get(d)?;
                let home = live.home.get();
                let name = home
                    .devices
                    .iter()
                    .find(|device| device.id == placed.device)
                    .map(|device| device.name.to_string())
                    .unwrap_or_else(|| placed.device.to_string());
                // Only a radar points anywhere, so only a radar is asked which way.
                let radar = looks(&home, &placed.device).radar;
                let (facing, wide) = (placed.facing, placed.view_angle());
                Some(
                    view! {
                        <div class="inspector">
                            <h2>"Device"</h2>
                            <p class="name">{name}</p>
                            <p class="muted small">{placed.device.to_string()}</p>
                            <p class="muted small">"Drag it to move it."</p>
                            {radar.then(|| aim(draft, floor, d, facing, wide, remember))}
                        </div>
                    }
                    .into_any(),
                )
            }
        }
    }
}

/// The contact sensors a door or window could follow: binary sensors that say they are on a
/// door, a window, a garage door or an opening, each with a name that says which device it is.
/// The one already chosen is always listed, even if it has since left the home or stopped being
/// a contact sensor — the menu has to be able to show what the plan says.
fn contacts(home: &Home, chosen: Option<&EntityId>) -> Vec<irori_ui_kit::combo::Choice> {
    use irori_types::BinarySensorClass as Class;
    use irori_ui_kit::combo::Choice;
    let mut found: Vec<Choice> = home
        .entities
        .iter()
        .filter(|entity| {
            matches!(
                &entity.capabilities,
                Capabilities::BinarySensor(sensor) if matches!(
                    sensor.device_class,
                    Some(Class::Door | Class::Window | Class::GarageDoor | Class::Opening)
                )
            )
        })
        .map(|entity| {
            let device = entity.device_id.as_ref().and_then(|id| {
                home.devices
                    .iter()
                    .find(|device| &device.id == id)
                    .map(|device| device.name.to_string())
            });
            // The device is what a person knows it by; the id is there to tell two apart.
            Choice::new(
                entity.id.to_string(),
                device.unwrap_or_else(|| entity.name.to_string()),
            )
            .detail(entity.id.to_string())
        })
        .collect();
    found.sort_by(|a, b| a.label.cmp(&b.label));
    if let Some(chosen) = chosen
        && !found
            .iter()
            .any(|choice| choice.value == chosen.to_string())
    {
        found.push(Choice::new(chosen.to_string(), chosen.to_string()).detail("not in the home"));
    }
    // First, and with an empty value: picking it takes the sensor away.
    found.insert(0, Choice::new("", "None"));
    found
}

/// Which way a radar points and how wide it sees: the two things about it no protocol reports
/// and nobody can drag onto a plan.
///
/// A dial to turn, because a direction is easier pointed than typed; the number beside it for
/// when it has to be exact, with a nudge either side; and a slider for the width. The field
/// itself is drawn on the plan as these change, at its full reach, so the thing being aimed at
/// is the room and not a number.
fn aim(
    draft: RwSignal<Floorplan>,
    floor: RwSignal<Option<FloorId>>,
    device: usize,
    facing: Option<u16>,
    wide: u16,
    remember: Callback<()>,
) -> impl IntoView {
    let turn_to = move |to: i32| {
        on_level(draft, floor, |level| {
            if let Some(placed) = level.devices.get_mut(device) {
                placed.facing = Some(to.rem_euclid(360) as u16);
            }
        });
    };
    // Where on the dial the pointer is, as a direction: clockwise from pointing right, the same
    // way round the plan measures it, to the nearest five degrees.
    let pointed = |event: &ev::PointerEvent| -> Option<i32> {
        let dial = event
            .current_target()?
            .dyn_into::<web_sys::Element>()
            .ok()?;
        let rect = dial.get_bounding_client_rect();
        let (dx, dy) = (
            f64::from(event.client_x()) - (rect.left() + rect.width() / 2.0),
            f64::from(event.client_y()) - (rect.top() + rect.height() / 2.0),
        );
        // Right on the hub there is no direction to speak of.
        (dx.hypot(dy) >= 4.0).then(|| (dy.atan2(dx).to_degrees() / 5.0).round() as i32 * 5)
    };
    let half = f64::from(wide).to_radians() / 2.0;
    let field = format!(
        "M 0 0 L {x:.2} {:.2} A 26 26 0 0 1 {x:.2} {:.2} Z",
        -26.0 * half.sin(),
        26.0 * half.sin(),
        x = 26.0 * half.cos()
    );
    let turned = format!("rotate:{}deg", facing.unwrap_or(0));
    let (least, most) = (
        *PlacedDevice::FIELD_OF_VIEW_RANGE.start(),
        *PlacedDevice::FIELD_OF_VIEW_RANGE.end(),
    );

    view! {
        <div class="aim">
            <div class="aim-head">
                <span id="aim-label">"Facing"</span>
                <span class="figure">
                    {facing.map_or_else(|| "not aimed".to_owned(), |facing| format!("{facing}°"))}
                </span>
            </div>
            <svg
                class="dial"
                class:unset=facing.is_none()
                viewBox="-32 -32 64 64"
                role="img"
                aria-label="Drag to turn the sensor"
                on:pointerdown=move |event: ev::PointerEvent| {
                    event.prevent_default();
                    remember.run(());
                    if let Some(dial) = event
                        .current_target()
                        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    {
                        // So the turn carries on when the pointer slides off the dial.
                        let _ = dial.set_pointer_capture(event.pointer_id());
                    }
                    if let Some(to) = pointed(&event) {
                        turn_to(to);
                    }
                }
                on:pointermove=move |event: ev::PointerEvent| {
                    if event.buttons() & 1 == 1
                        && let Some(to) = pointed(&event)
                    {
                        turn_to(to);
                    }
                }
            >
                <circle class="dial-ring" r="26" />
                <path class="dial-field" d=field style=turned.clone() />
                <line class="dial-needle" x1="0" y1="0" x2="24" y2="0" style=turned />
                <circle class="dial-hub" r="3" />
            </svg>
            <div class="aim-step">
                <button
                    type="button"
                    aria-label="Turn 15° anticlockwise"
                    on:click=move |_| {
                        remember.run(());
                        turn_to(i32::from(facing.unwrap_or(0)) - 15);
                    }
                >
                    "−15°"
                </button>
                <input
                    type="number"
                    min="0"
                    max="359"
                    step="1"
                    aria-labelledby="aim-label"
                    placeholder="—"
                    prop:value=facing.map(|facing| facing.to_string()).unwrap_or_default()
                    on:pointerdown=move |_| remember.run(())
                    on:keydown=move |_| remember.run(())
                    on:input=move |event| {
                        if let Ok(to) = event_target_value(&event).parse::<i32>() {
                            turn_to(to.clamp(0, 359));
                        }
                    }
                />
                <button
                    type="button"
                    aria-label="Turn 15° clockwise"
                    on:click=move |_| {
                        remember.run(());
                        turn_to(i32::from(facing.unwrap_or(0)) + 15);
                    }
                >
                    "+15°"
                </button>
            </div>
            {facing.is_some().then(|| view! {
                <button
                    type="button"
                    class="quiet"
                    on:click=move |_| {
                        remember.run(());
                        on_level(draft, floor, |level| {
                            if let Some(placed) = level.devices.get_mut(device) {
                                placed.facing = None;
                                placed.field_of_view = None;
                            }
                        });
                    }
                >
                    "Stop aiming it"
                </button>
            })}
        </div>
        <label class="slider">
            <span>"Field of view"</span>
            <input
                type="range"
                min=least
                max=most
                step="5"
                disabled=facing.is_none()
                prop:value=wide
                style:--fill=format!("{}%", crate::devices::fill(wide, least, most))
                on:pointerdown=move |_| remember.run(())
                on:keydown=move |_| remember.run(())
                on:input=move |event| {
                    let Ok(next) = event_target_value(&event).parse::<u16>() else {
                        return;
                    };
                    on_level(draft, floor, |level| {
                        if let Some(placed) = level.devices.get_mut(device) {
                            placed.field_of_view = Some(next.clamp(least, most));
                        }
                    });
                }
            />
            <output class="figure">{format!("{wide}°")}</output>
        </label>
    }
}

/// The whole of an empty plan: a small flat drawn faintly, what the page is for in a line, and
/// — for a home with no floors — the one button that gets it started.
///
/// The drawing is what this page is for, said in the shape of the thing rather than in a
/// sentence at the bottom of the screen, and it is drawn the way the editor draws things: walls
/// with holes in them, doors with their swing, devices as pips. In its own pixels rather than in
/// centimetres, so it sits in the middle at the same size whatever the zoom is — it is a picture
/// of a floorplan, not one.
#[component]
fn EmptyPlan(
    floors: Memo<Vec<irori_types::Floor>>,
    live: crate::Live,
    trouble: RwSignal<Option<String>>,
) -> impl IntoView {
    // A one-bedroom flat, 520 × 360 as drawn: living room on the left, bedroom top right,
    // bathroom below it. Openings are gaps in the wall paths, exactly as a real plan has them.
    let walls = concat!(
        // Outside: two windows along the top, one in the left wall, the front door at the bottom.
        "M0 0 H70 M150 0 H240 M320 0 H520 ",
        "M520 0 V360 ",
        "M520 360 H300 M220 360 H0 ",
        "M0 360 V240 M0 150 V0 ",
        // The wall between the living room and the bedroom, with its doorway.
        "M320 0 V110 M320 190 V360 ",
        // The wall between the bedroom and the bathroom, with its doorway.
        "M320 200 H400 M470 200 H520",
    );
    let making = RwSignal::new(false);
    let make_ground_floor = move || {
        making.set(true);
        spawn_local(async move {
            let name = "Ground floor".parse().expect("a name Irori itself wrote");
            match api::add_floor(name, 0).await {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
            making.set(false);
        });
    };

    view! {
        <div class="empty-plan">
            <svg class="watermark" viewBox="-8 -8 536 376" aria-hidden="true">
                <path class="wm-wall" d=walls fill="none" stroke-width="11"
                    stroke-linecap="square" />

                // The windows: a pane across each gap, with a jamb at either end.
                <path class="wm-glass" d="M70 0 H150 M240 0 H320 M0 150 V240" />
                <path class="wm-jamb" d="M70 -6 V6 M150 -6 V6 M240 -6 V6 M320 -6 V6
                                         M-6 150 H6 M-6 240 H6" />

                // The front door, standing open into the living room, and the two inside doors —
                // the way every plan since the drawing board has shown one.
                <path class="wm-swing" fill="none"
                    d="M220 360 V280 M220 280 A80 80 0 0 0 300 360" />
                <path class="wm-swing" fill="none"
                    d="M320 110 H400 M400 110 A80 80 0 0 0 320 190" />
                <path class="wm-swing" fill="none"
                    d="M470 200 V270 M470 270 A70 70 0 0 0 400 200" />

                // Enough furniture to read as somebody's home: a bed under the windows, a sofa
                // and a table in the living room, a bath and a basin.
                <rect class="wm-thing" x="350" y="25" width="140" height="105" rx="6" />
                <path class="wm-thing" d="M350 60 H490" />
                <rect class="wm-thing" x="35" y="65" width="48" height="130" rx="10" />
                <path class="wm-thing" d="M68 78 V182" />
                <circle class="wm-thing" cx="170" cy="150" r="42" />
                <rect class="wm-thing" x="340" y="230" width="60" height="110" rx="10" />
                <circle class="wm-thing" cx="480" cy="320" r="20" />

                // And the point of the page: devices, where they are.
                <g class="wm-pip">
                    <circle cx="170" cy="150" r="9" />
                    <circle cx="420" cy="78" r="9" />
                    <circle cx="120" cy="300" r="9" />
                </g>
            </svg>

            {move || if floors.get().is_empty() {
                // Not an error and not a dead end: a plan is the plan of a floor, and this home
                // hasn't got one. Settings is where floors are really managed; this is the one
                // click that saves a trip there on a first run.
                view! {
                    <>
                        <p class="empty-title">"A plan belongs to a floor"</p>
                        <p class="empty-lede">"and this home hasn't got one yet"</p>
                        <button
                            type="button"
                            class="solid"
                            disabled=move || making.get()
                            on:click=move |_| make_ground_floor()
                        >
                            {move || if making.get() { "Adding…" } else { "Add a ground floor" }}
                        </button>
                        <p class="empty-lede">"More of them in Settings."</p>
                    </>
                }
                .into_any()
            } else {
                view! {
                    <>
                        <p class="empty-title">"Nothing drawn on this floor yet"</p>
                        <p class="empty-lede">"Press Edit to draw its walls"</p>
                    </>
                }
                .into_any()
            }}
        </div>
    }
}

/// The devices of the home, to pick one to place.
///
/// Every device, whatever floor is showing, and already-placed ones stay in the list: a device
/// is in one place, so clicking one again moves it — to another spot, or to another floor —
/// rather than adding a second of the same thing.
#[component]
fn DevicePicker(
    plan: Memo<Floorplan>,
    arming: RwSignal<Option<DeviceId>>,
    live: crate::Live,
) -> impl IntoView {
    view! {
        <div class="device-picker">
            <h2>"Devices"</h2>
            <ul>
                {move || {
                    let home = live.home.get();
                    let placed = plan.get();
                    let mut devices = home.devices.clone();
                    devices.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
                    if devices.is_empty() {
                        return view! {
                            <li class="muted">"No devices yet."</li>
                        }.into_any();
                    }
                    devices
                        .into_iter()
                        .map(|device| {
                            let id = device.id.clone();
                            let armed = id.clone();
                            // Anywhere on the plan, not just this floor: a device is in one
                            // place, and putting it down here is how it moves between floors.
                            let on_plan = placed.floors.values().any(|level| {
                                level.devices.iter().any(|entry| entry.device == device.id)
                            });
                            view! {
                                <li>
                                    <button
                                        type="button"
                                        class:chosen=move || arming.get().as_ref() == Some(&armed)
                                        on:click=move |_| arming.update(|current| {
                                            *current = if current.as_ref() == Some(&id) {
                                                None
                                            } else {
                                                Some(id.clone())
                                            };
                                        })
                                    >
                                        <span class="name">{device.name.to_string()}</span>
                                        {on_plan.then(|| view! {
                                            <span class="badge">"on the plan"</span>
                                        })}
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </ul>
        </div>
    }
}

/// One device where it was put: what kind of thing it is, and what it's doing.
///
/// A round glyph in the tone of its kind, which opens out to the right into a short reading
/// only while it has something to say — a temperature, the watts a plug is drawing, what the TV
/// is playing. Its name is a hover away, and always shown while arranging the plan, when names
/// are what tell two lamps apart.
///
/// HTML rather than something drawn in the SVG, because a marker is a control — it has a label,
/// it can be focused, and in reading mode clicking it switches the device. It is also the one
/// thing on the canvas that must *not* scale with the zoom: a reading is either readable or it
/// isn't.
///
/// The page draws these again with every reading, in place. So the shape of what's returned
/// never depends on the device's state — only classes, text and styles do — which is what lets
/// a marker *grow* into its reading rather than be swapped for a wider one.
#[expect(
    clippy::too_many_arguments,
    reason = "the editor's state, passed along"
)]
fn marker(
    index: usize,
    placed: &PlacedDevice,
    device: &Device,
    home: &Home,
    view: Viewport,
    editing: bool,
    chosen: bool,
    controls: Controls,
    picked: RwSignal<Option<Pick>>,
    drag: RwSignal<Option<Drag>>,
    dragged: RwSignal<bool>,
    dropped: RwSignal<Option<usize>>,
    flashes: RwSignal<BTreeMap<DeviceId, u32>>,
    remember: Callback<()>,
) -> impl IntoView + use<> {
    let (x, y) = view.screen(placed.at);
    let look = looks(home, &device.id);
    let name = device.name.to_string();
    let (on, offline) = (look.on, look.offline);
    // What a click does, if anything. A marker is a control only while reading the plan, and
    // only for a device that is answering: one that isn't must not look like one that is. In
    // edit mode it is a thing to move instead, so it stays live whatever the device is doing.
    let click = if offline {
        None
    } else if let Some(entity) = look.switch.clone() {
        // An entity that hasn't said yet is turned on, which is what asking for "the other
        // one" means when there is no current one.
        Some(Click::Switch(entity, !on.unwrap_or(false)))
    } else if let Some(entity) = look.media.clone() {
        Some(Click::Act(entity, "media_play_pause"))
    } else if let Some(entity) = look.lock.clone() {
        Some(Click::Act(entity, "lock"))
    } else {
        look.press.clone().map(|entity| Click::Act(entity, "press"))
    };
    let live = click.is_some();
    // Arranging the plan, a marker says only which device it is; reading it, what it's doing.
    let said = if editing || look.saying.is_empty() {
        name.clone()
    } else {
        format!("{name} · {}", look.saying)
    };
    let label = match (&look.reading, look.saying.is_empty()) {
        (Some(reading), _) => format!("{name}, {reading}"),
        (None, false) => format!("{name}, {}", look.saying),
        (None, true) => name.clone(),
    };
    // Only a radar that has been aimed says which way it looks.
    let facing = placed.facing.filter(|_| look.radar);
    let open = look.reading.is_some();
    let reading = look.reading.clone().unwrap_or_default();
    let tip = said.clone();
    let (flashed, flashed_again) = (device.id.clone(), device.id.clone());

    view! {
        <button
            type="button"
            class="marker"
            data-tone=look.tone.as_str()
            class:on=look.active
            class:lit=look.lit
            class:open=open
            class:pulse=look.pulse
            class:aimed=facing.is_some()
            class:rings=look.rings
            class:offline=offline
            // A press or a ring: the same flash under two names, so one straight after another
            // starts it again.
            class:flash-a=move || flashes.with(|all| all.get(&flashed).is_some_and(|n| n % 2 == 1))
            class:flash-b=move || {
                flashes.with(|all| all.get(&flashed_again).is_some_and(|n| n % 2 == 0))
            }
            // Held, it lifts off the plan; put down, it settles with a bounce.
            class:lifted=move || matches!(drag.get(), Some(Drag::Device { device }) if device == index)
            class:dropped=move || dropped.get() == Some(index)
            class:chosen=chosen
            class:movable=editing
            class:switchable=!editing && live
            style=format!("left:{x}px;top:{y}px")
            title=tip
            aria-label=label
            // Not `disabled`: a marker with nothing to switch still has a name and a reading to
            // show to whoever tabs to it, and a disabled button can't be tabbed to.
            aria-disabled=(!editing && !live).then_some("true")
            aria-pressed=(!editing && live && look.switch.is_some())
                .then(|| on.map(|on| on.to_string()))
                .flatten()
            on:mousedown=move |event: ev::MouseEvent| {
                if !editing {
                    return;
                }
                event.stop_propagation();
                dragged.set(false);
                picked.set(Some(Pick::Device(index)));
                remember.run(());
                drag.set(Some(Drag::Device { device: index }));
            }
            on:click=move |event: ev::MouseEvent| {
                event.stop_propagation();
                if editing {
                    return;
                }
                match click.clone() {
                    Some(Click::Switch(entity, to)) => controls.set_on.run((entity, to)),
                    Some(Click::Act(entity, action)) => controls.act.run((entity, action, None)),
                    None => {}
                }
            }
        >
            <span class="glyph">{icon(look.glyph)}</span>
            <span class="reading">{reading}</span>
            // Which way a radar looks: a short arc on the marker's own ring, turned to face it.
            <svg
                class="facing"
                viewBox="-20 -20 40 40"
                aria-hidden="true"
                style=format!("rotate:{}deg", facing.unwrap_or(0))
            >
                <path d="M16.5 -9.5 A19 19 0 0 1 16.5 9.5" />
            </svg>
            // Not answering: a struck-through signal on the marker's shoulder. Always there,
            // and only shown while it's true.
            <span class="lost" aria-hidden="true">{icon(crate::icons::Icon::NoSignal)}</span>
            <span class="marker-name">{said}</span>
        </button>
    }
}

/// What a click on a marker asks for.
#[derive(Debug, Clone)]
enum Click {
    /// Turn an entity on or off.
    Switch(EntityId, bool),
    /// One of its kind's own actions: play or pause, press.
    Act(EntityId, &'static str),
}

/// A device's look on the plan, from the home as it is right now ([`look_for`] has the rules).
fn looks(home: &Home, device: &DeviceId) -> Look {
    let entities: Vec<_> = home
        .entities
        .iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
        .collect();
    look_for(&entities, &home.states)
}

// --- Drawing ------------------------------------------------------------------------------
//
// Everything below turns centimetres into the SVG. The walls live inside one transformed group
// so their thickness is real thickness; anything that has to keep its size on screen — the
// corner handles — works out its own size from the scale.

fn transform(view: Viewport) -> String {
    format!(
        "translate({} {}) scale({})",
        view.pan.0, view.pan.1, view.scale
    )
}

/// The grid, over enough of the plan that panning doesn't run off it.
///
/// Two of them. The heavy lines are metres, which is how anyone reads a room. The faint ones are
/// the **snap step** — the places a point can actually land — because a grid you can see and a
/// grid you snap to being different things is the one way a grid can lie. They come and go with
/// the zoom: closer together than a few pixels and they stop being a grid and start being a grey
/// wash, so below that only the metres are drawn.
fn grid(view: Viewport, snap: Snap) -> impl IntoView {
    let span = 6000;
    let step = snap.step();
    let fine = (step < GRID && f64::from(step) * view.scale >= 4.0).then_some(step);
    view! {
        <g class="grid" transform=transform(view)>
            <defs>
                {fine.map(|step| view! {
                    <pattern id="snap-step" width=step height=step patternUnits="userSpaceOnUse">
                        <path
                            class="fine-line"
                            d=format!("M {step} 0 H 0 V {step}")
                            fill="none"
                            vector-effect="non-scaling-stroke"
                        />
                    </pattern>
                })}
                <pattern id="metre" width=GRID height=GRID patternUnits="userSpaceOnUse">
                    <path
                        class="metre-line"
                        d=format!("M {GRID} 0 H 0 V {GRID}")
                        fill="none"
                        vector-effect="non-scaling-stroke"
                    />
                </pattern>
            </defs>
            {fine.map(|_| view! {
                <rect x=-span y=-span width=span * 2 height=span * 2 fill="url(#snap-step)" />
            })}
            <rect x=-span y=-span width=span * 2 height=span * 2 fill="url(#metre)" />
        </g>
    }
}

/// One wall: the stretches of it that are still solid, and the doors and windows in the gaps.
///
/// `finishes` is how each end meets its neighbours — flat, capped, or curving round into the
/// next wall; see [`finishes`]. Each stretch is one path, so a wall and the curve it ends in
/// are a single stroke with no seam between them.
fn drawn_wall(
    wall: &Wall,
    index: usize,
    finishes: (Finish, Finish),
    view: Viewport,
    picked: Option<Pick>,
    // What the home's sensors last said, for showing doors and windows open. `None` while the
    // plan is being drawn: then they are shut, except the one being worked on.
    states: Option<&[irori_types::EntityState]>,
) -> impl IntoView + use<> {
    let thickness = f64::from(wall.thickness);
    let chosen = picked == Some(Pick::Wall(index));
    let length = wall.length();
    let arc = |turn: Turn, clockwise: bool, to: (f64, f64)| {
        format!(
            "A {r:.2} {r:.2} 0 0 {} {:.2} {:.2}",
            u8::from(clockwise),
            to.0,
            to.1,
            r = turn.radius
        )
    };
    let runs = solid_runs(wall)
        .into_iter()
        .map(|(start, end)| {
            // Only the ends that really are the ends of the wall are finished: the sides of a
            // doorway are the ends of a run too, and they must stay where the door is.
            let from = (start <= 0.0).then_some(finishes.0);
            let to = (end >= length).then_some(finishes.1);
            let (start, lead) = match from {
                Some(Finish::Turn(turn)) => (turn.back, Some(turn)),
                _ => (start, None),
            };
            let (end, tail) = match to {
                Some(Finish::Turn(turn)) => (length - turn.back, Some(turn)),
                _ => (end, None),
            };
            let (a, _, _) = along(wall, start);
            let (b, _, _) = along(wall, end.max(start));
            let mut path = match lead {
                // Drawn from the middle of the corner back onto the wall, so the other way round.
                Some(turn) => format!(
                    "M {:.2} {:.2} {} ",
                    turn.to.0,
                    turn.to.1,
                    arc(turn, !turn.clockwise, a)
                ),
                None => format!("M {:.2} {:.2} ", a.0, a.1),
            };
            path.push_str(&format!("L {:.2} {:.2}", b.0, b.1));
            if let Some(turn) = tail {
                path.push(' ');
                path.push_str(&arc(turn, turn.clockwise, turn.to));
            }
            view! {
                <path
                    class="wall"
                    class:chosen=chosen
                    d=path
                    fill="none"
                    stroke-width=thickness
                />
            }
        })
        .collect_view();
    // A round cap where several walls meet: together they make the outside of the joint round
    // whatever the angles are.
    let caps = [(finishes.0, wall.from), (finishes.1, wall.to)]
        .into_iter()
        .filter(|(finish, _)| *finish == Finish::Capped)
        .map(|(_, at)| {
            view! {
                <circle class="wall-cap" class:chosen=chosen cx=at.x cy=at.y r=thickness / 2.0 />
            }
        })
        .collect_view();
    let openings = wall
        .openings
        .iter()
        .enumerate()
        .map(|(o, opening)| {
            let chosen = picked == Some(Pick::Opening(index, o));
            let standing = states.map_or(Standing::Shut, |states| standing(opening, states));
            drawn_opening(wall, opening, standing, chosen, chosen)
        })
        .collect_view();

    view! {
        <g class="wall-group" transform=transform(view)>
            {runs}
            {caps}
            {openings}
        </g>
    }
}

/// The insides of the corners that [`fillets`] rounds, filled in the walls' own colour.
fn drawn_fillets(fillets: &[Fillet], view: Viewport) -> impl IntoView + use<> {
    let patches = fillets
        .iter()
        .map(|fillet| {
            let path = format!(
                "M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} A {r:.2} {r:.2} 0 0 {} {:.2} {:.2} Z",
                fillet.from.0,
                fillet.from.1,
                fillet.corner.0,
                fillet.corner.1,
                fillet.to.0,
                fillet.to.1,
                u8::from(fillet.clockwise),
                fillet.from.0,
                fillet.from.1,
                r = fillet.radius
            );
            view! { <path class="wall-fillet" d=path /> }
        })
        .collect_view();
    view! { <g class="wall-group" transform=transform(view)>{patches}</g> }
}

/// Whether a door or window is open, as far as the plan knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    Shut,
    Open,
    /// It has a sensor, and the sensor isn't saying: never reported, or not answering. Drawn
    /// shut, and faded, because shut is a guess.
    Unknown,
}

/// How an opening stands, from the contact sensor it was given. Without one it is shut: a plan
/// shouldn't claim a door is open that nothing says is.
fn standing(opening: &Opening, states: &[irori_types::EntityState]) -> Standing {
    let Some(sensor) = &opening.sensor else {
        return Standing::Shut;
    };
    let Some(state) = states.iter().find(|state| &state.entity_id == sensor) else {
        return Standing::Unknown;
    };
    if state.availability == irori_types::Availability::Unavailable {
        return Standing::Unknown;
    }
    match &state.state {
        Some(irori_types::State::BinarySensor(contact)) if contact.on => Standing::Open,
        Some(irori_types::State::BinarySensor(_)) => Standing::Shut,
        _ => Standing::Unknown,
    }
}

/// The part of a door or window that moves: a slab or a pane, lying in the wall while it's
/// shut, and the turn about its hinge that opens it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Leaf {
    /// Its four corners while shut, in the plan's centimetres.
    corners: [(f64, f64); 4],
    /// The hinge it turns on.
    pivot: (f64, f64),
    /// The end furthest from the hinge, on the wall's middle line.
    latch: (f64, f64),
    /// How far it turns to stand open, in degrees. Positive is clockwise as the plan is drawn,
    /// which is the way CSS and SVG both count with y running down the page.
    swing: f64,
}

impl Leaf {
    /// Where a point of the shut leaf is once it stands open.
    fn opened(&self, point: (f64, f64)) -> (f64, f64) {
        let (sin, cos) = self.swing.to_radians().sin_cos();
        let (dx, dy) = (point.0 - self.pivot.0, point.1 - self.pivot.1);
        (
            self.pivot.0 + dx * cos - dy * sin,
            self.pivot.1 + dx * sin + dy * cos,
        )
    }
}

/// A window narrower than this, in centimetres, is a single pane: two sashes that small would
/// be slivers.
const ONE_PANE: u32 = 60;

/// The leaves of an opening: a door's one slab, hung from whichever end it was told; a window's
/// pair of panes, one hung from each jamb, or its single pane when it's a small one.
///
/// A leaf runs from its hinge along the wall, and opens by a quarter turn towards the side the
/// opening was told. Turning the wall's own direction clockwise gives its left-hand side, so a
/// leaf that runs *with* the wall turns clockwise to open left, and one that runs against it —
/// hung from the far end — turns the other way to end up on the same side.
fn leaves(wall: &Wall, opening: &Opening) -> Vec<Leaf> {
    let half = f64::from(opening.width) / 2.0;
    let at = f64::from(opening.at);
    let thickness = f64::from(wall.thickness);
    let (near, unit, normal) = along(wall, at - half);
    let (far, _, _) = along(wall, at + half);
    let towards = match opening.side {
        irori_types::Side::Left => 1.0,
        irori_types::Side::Right => -1.0,
    };
    // A leaf from `pivot`, `length` long, running with the wall (`way` 1) or against it (-1).
    let leaf = |pivot: (f64, f64), way: f64, length: f64, deep: f64| {
        let latch = (
            pivot.0 + unit.0 * way * length,
            pivot.1 + unit.1 * way * length,
        );
        let (ox, oy) = (normal.0 * deep / 2.0, normal.1 * deep / 2.0);
        Leaf {
            corners: [
                (pivot.0 - ox, pivot.1 - oy),
                (latch.0 - ox, latch.1 - oy),
                (latch.0 + ox, latch.1 + oy),
                (pivot.0 + ox, pivot.1 + oy),
            ],
            pivot,
            latch,
            swing: 90.0 * towards * way,
        }
    };
    let width = f64::from(opening.width);
    match opening.kind {
        OpeningKind::Door => vec![match opening.hinge {
            irori_types::Hinge::Near => leaf(near, 1.0, width, thickness * 0.7),
            irori_types::Hinge::Far => leaf(far, -1.0, width, thickness * 0.7),
        }],
        OpeningKind::Window if opening.width < ONE_PANE => {
            vec![leaf(near, 1.0, (width - 1.0).max(0.5), thickness * 0.6)]
        }
        OpeningKind::Window => {
            let pane = (width / 2.0 - 1.0).max(0.5);
            vec![
                leaf(near, 1.0, pane, thickness * 0.6),
                leaf(far, -1.0, pane, thickness * 0.6),
            ]
        }
    }
}

/// A door or a window in the gap its wall left for it.
///
/// The wall's two faces carry on across the gap as thin lines, so the wall still reads as one
/// wall. Shut, the gap holds a door's slab or a window's glass. Open, the slab or the panes
/// stand out from the wall on their hinges, and a door lights the floor it swept across. The
/// turn is a style rather than a redrawn shape, so a door *swings* when its sensor changes.
///
/// `previewing` is the editor's view of the one that's picked up: standing open, faintly, so
/// whoever is choosing its side and its hinge can see which way it goes.
fn drawn_opening(
    wall: &Wall,
    opening: &Opening,
    standing: Standing,
    chosen: bool,
    previewing: bool,
) -> impl IntoView + use<> {
    let half = f64::from(opening.width) / 2.0;
    let at = f64::from(opening.at);
    let thickness = f64::from(wall.thickness);
    let (near, _, normal) = along(wall, at - half);
    let (far, _, _) = along(wall, at + half);
    let width = f64::from(opening.width);
    let is_door = opening.kind == OpeningKind::Door;

    let edges = [1.0, -1.0]
        .into_iter()
        .map(|face| {
            let (dx, dy) = (
                normal.0 * thickness / 2.0 * face,
                normal.1 * thickness / 2.0 * face,
            );
            view! {
                <line
                    class="opening-edge"
                    x1=near.0 + dx y1=near.1 + dy x2=far.0 + dx y2=far.1 + dy
                    stroke-width=thickness * 0.15
                />
            }
        })
        .collect_view();

    let moving = leaves(wall, opening);
    // The floor a door passes over: the quarter circle between where its latch is shut and
    // where it is open.
    let sweeps = moving
        .iter()
        .filter(|_| is_door)
        .map(|leaf| {
            let open = leaf.opened(leaf.latch);
            let path = format!(
                "M {:.2} {:.2} L {:.2} {:.2} A {width} {width} 0 0 {} {:.2} {:.2} Z",
                leaf.pivot.0,
                leaf.pivot.1,
                leaf.latch.0,
                leaf.latch.1,
                u8::from(leaf.swing > 0.0),
                open.0,
                open.1
            );
            view! { <path class="door-sweep" d=path /> }
        })
        .collect_view();
    let panels = moving
        .iter()
        .map(|leaf| {
            let points = leaf
                .corners
                .iter()
                .map(|(x, y)| format!("{x:.2},{y:.2}"))
                .collect::<Vec<_>>()
                .join(" ");
            view! {
                <polygon
                    class=if is_door { "door-slab" } else { "window-pane" }
                    points=points
                    style=format!(
                        "transform-origin:{:.2}px {:.2}px;--swing:{}deg",
                        leaf.pivot.0, leaf.pivot.1, leaf.swing
                    )
                />
            }
        })
        .collect_view();

    view! {
        <g
            class="opening"
            class:open=previewing || standing == Standing::Open
            class:unknown=!previewing && standing == Standing::Unknown
            class:chosen=chosen
            class:previewing=previewing
        >
            {sweeps}
            {edges}
            {panels}
        </g>
    }
}

/// The wall being drawn right now, from the last corner to the pointer.
fn pending(from: Point, to: Point, view: Viewport) -> impl IntoView {
    view! {
        <g class="pending" transform=transform(view)>
            <line x1=from.x y1=from.y x2=to.x y2=to.y vector-effect="non-scaling-stroke" />
            <circle cx=from.x cy=from.y r=4.0 / view.scale vector-effect="non-scaling-stroke" />
        </g>
    }
}

/// The corners of whatever is picked up, to drag: the two ends of a wall, or every corner of a
/// room. Sized in screen pixels, which at this zoom means dividing by the scale.
fn handles(corners: &[Point], view: Viewport) -> impl IntoView + use<> {
    let radius = 6.0 / view.scale;
    let corners: Vec<Point> = corners.to_vec();
    view! {
        <g class="handles" transform=transform(view)>
            {corners
                .into_iter()
                .map(|point| view! {
                    <circle cx=point.x cy=point.y r=radius vector-effect="non-scaling-stroke" />
                })
                .collect_view()}
        </g>
    }
}

/// A room, as a shape under the walls with its floor tinted.
///
/// The tint is picked from the room's id, so the same room is the same colour every time the
/// page is opened and two rooms side by side are almost never the same. Low enough that it reads
/// as a wash over the paper rather than as a block of colour — a plan is drawn in lines.
fn drawn_area(placed: &PlacedArea, view: Viewport, chosen: bool) -> impl IntoView + use<> {
    let points = placed
        .points
        .iter()
        .map(|point| format!("{},{}", point.x, point.y))
        .collect::<Vec<_>>()
        .join(" ");
    let tint = tint_of(placed.area.as_str());
    view! {
        <g class="areas" transform=transform(view)>
            <polygon
                class=format!("area tint-{tint}")
                class:chosen=chosen
                points=points
                vector-effect="non-scaling-stroke"
            />
        </g>
    }
}

/// Which of the tints a room gets. A sum of its id's bytes: stable across restarts and across
/// browsers, which a hash with a random seed would not be.
fn tint_of(id: &str) -> u32 {
    const TINTS: u32 = 6;
    id.bytes().fold(0u32, |sum, byte| {
        sum.wrapping_mul(31).wrapping_add(u32::from(byte))
    }) % TINTS
}

/// The floor below, as an outline. Something to line an upstairs up with, and nothing more: no
/// openings, no rooms, no devices, and nothing on it can be clicked.
fn ghost(below: &Level, view: Viewport) -> impl IntoView + use<> {
    let lines = below
        .walls
        .iter()
        .map(|wall| {
            view! {
                <line
                    x1=wall.from.x y1=wall.from.y x2=wall.to.x y2=wall.to.y
                    stroke-width=wall.thickness
                />
            }
        })
        .collect_view();
    view! { <g class="ghost" transform=transform(view)>{lines}</g> }
}

/// The room being traced: the corners so far, and the line out to the pointer.
fn tracing_shape(
    corners: &[Point],
    pointer: Option<Point>,
    view: Viewport,
) -> impl IntoView + use<> {
    let mut points: Vec<Point> = corners.to_vec();
    if let Some(pointer) = pointer {
        points.push(pointer);
    }
    let path = points
        .iter()
        .map(|point| format!("{},{}", point.x, point.y))
        .collect::<Vec<_>>()
        .join(" ");
    let radius = 4.0 / view.scale;
    let first = corners.first().copied();
    view! {
        <g class="tracing" transform=transform(view)>
            <polygon points=path vector-effect="non-scaling-stroke" />
            {corners
                .iter()
                .map(|point| view! {
                    <circle cx=point.x cy=point.y r=radius vector-effect="non-scaling-stroke" />
                })
                .collect_view()}
            // The corner a click closes the shape on, marked so it can be aimed at.
            {first.map(|point| view! {
                <circle class="close" cx=point.x cy=point.y r=radius * 2.0
                    vector-effect="non-scaling-stroke" />
            })}
        </g>
    }
}

// --- Geometry -----------------------------------------------------------------------------

/// Rounds a measurement to the nearest multiple of `step` centimetres.
///
/// Panning is unbounded, so `value` really can be enormous — far off the plan, at the lowest
/// zoom, is a number with ten digits in it. The count of steps is therefore clamped to the
/// largest **whole multiple** an `i32` can hold before it is multiplied back up: casting alone
/// isn't enough, because a float-to-int cast saturates to `i32::MAX` and multiplying *that* by
/// the step is the overflow. Clamping the count rather than the product also means a point at
/// the edge of the world still lands on the grid rather than just inside it.
fn round(value: f64, step: i32) -> i32 {
    let step = step.max(1);
    let span = f64::from(step);
    let most = (f64::from(i32::MAX) / span).trunc();
    let steps = (value / span).round().clamp(-most, most) as i32;
    steps * step
}

/// Where a new point goes: onto a corner that's already there if one is within reach, and onto
/// the grid otherwise.
///
/// `except` leaves one corner out — the one being dragged. Without it a corner could never be
/// moved off the grid square it started on, because it would keep snapping to itself.
fn place(
    level: &Level,
    world: (f64, f64),
    view: Viewport,
    snap: Snap,
    except: Option<Point>,
) -> Point {
    let reach = CORNER * view.cm_per_pixel();
    let mut nearest: Option<(f64, Point)> = None;
    for corner in level.walls.iter().flat_map(|wall| [wall.from, wall.to]) {
        if Some(corner) == except {
            continue;
        }
        let away = (world.0 - f64::from(corner.x)).hypot(world.1 - f64::from(corner.y));
        if away <= reach && nearest.is_none_or(|(best, _)| away < best) {
            nearest = Some((away, corner));
        }
    }
    match nearest {
        Some((_, corner)) => corner,
        None => {
            let step = snap.step();
            Point::new(round(world.0, step), round(world.1, step))
        }
    }
}

/// Which end of a wall a drag has hold of.
fn ends(wall: &Wall, to_end: bool) -> Point {
    if to_end { wall.to } else { wall.from }
}

/// Moves corners of the plan, carrying **every** wall end standing on one of them.
///
/// Corners weld when a room is drawn — a wall's end snaps onto the corner already there — and
/// they have to stay welded when one is dragged, or pulling a room straight would open a gap in
/// it. One pass over a fixed list of moves rather than a move at a time, so a corner dragged
/// onto another corner can't be moved twice.
///
/// A move that would leave some wall with no length is dropped rather than applied: such a plan
/// can't be drawn and the core would refuse to save it, and the drag has a next frame anyway.
fn shift(level: &mut Level, moves: &[(Point, Point)]) {
    let mut next = level.clone();
    let moved = |point: &mut Point| {
        if let Some((_, to)) = moves.iter().find(|(was, _)| was == point) {
            *point = *to;
        }
    };
    for wall in &mut next.walls {
        moved(&mut wall.from);
        moved(&mut wall.to);
    }
    // Rooms traced onto those corners come too. A room is a note about where the walls are, so
    // a wall that moves and leaves its room behind has left the note pointing at nothing. The
    // other way round is not true: nudging a room's outline is somebody saying the room ends
    // somewhere other than the middle of the wall, and the wall should stay where it was built.
    for area in &mut next.areas {
        for corner in &mut area.points {
            moved(corner);
        }
    }
    if next.walls.iter().any(|wall| wall.from == wall.to) {
        return;
    }
    // A wall dragged shorter can leave its own door hanging off the end of it.
    for wall in &mut next.walls {
        trim(wall);
    }
    *level = next;
}

/// How far along a segment a point falls, and how far off it — both in centimetres, with the
/// distance along clamped to the segment.
fn on_wall_at(from: Point, to: Point, world: (f64, f64)) -> (f64, f64) {
    let (ax, ay) = (f64::from(from.x), f64::from(from.y));
    let (dx, dy) = (f64::from(to.x) - ax, f64::from(to.y) - ay);
    let square = dx * dx + dy * dy;
    if square <= f64::EPSILON {
        return (0.0, (world.0 - ax).hypot(world.1 - ay));
    }
    let share = (((world.0 - ax) * dx + (world.1 - ay) * dy) / square).clamp(0.0, 1.0);
    let (px, py) = (ax + dx * share, ay + dy * share);
    (share * square.sqrt(), (world.0 - px).hypot(world.1 - py))
}

/// How much rounder than the wall is thick a corner's curve is: the radius of its middle line,
/// in wall thicknesses. A little over one, so the inside of the corner is visibly round too —
/// soft, not a bend in a pipe.
const ROUNDING: f64 = 1.2;

/// How sharp a corner can be, in radians, and still be rounded. Sharper than this the curve
/// would have to be so tight it isn't one.
const SHARPEST: f64 = 0.35;

/// How one end of a wall is finished where it meets the others.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Finish {
    /// Stops flat, where it was drawn: an end no other wall's end meets. A wall that stops in
    /// the middle of a room stops there.
    Flat,
    /// Ends in a round cap. Where walls meet in a way one curve can't carry — three of them, or
    /// two of different thicknesses, or a hairpin — each is capped, and the caps together make
    /// the outside of the joint round. Two walls carrying straight on are capped too, and it
    /// doesn't show.
    Capped,
    /// Curves round into its one neighbour.
    Turn(Turn),
}

/// A wall's half of a rounded corner: where it stops being straight, and the arc that carries
/// it on to meet the other wall half-way round.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Turn {
    /// How far short of the corner the straight part stops, in centimetres.
    back: f64,
    /// The radius of the arc down the middle of the wall.
    radius: f64,
    /// Where this wall's share of the arc ends: the middle of the corner and a touch beyond,
    /// so the two halves overlap rather than leave a hairline between them.
    to: (f64, f64),
    /// Which way the arc bends going from the wall into the corner, as SVG's sweep flag has it.
    clockwise: bool,
}

/// How each end of a wall meets its neighbours: `(from, to)`.
///
/// Walls are drawn as thick lines with flat ends, so two of them meeting at a corner would
/// stop on the corner point: a notch missing from the outside and a hard edge on the inside.
/// Instead a corner of two walls is **one curve** — each wall stops short of the corner and an
/// arc carries it round, so the inside and the outside come out as two concentric curves, the
/// way a line drawn with a round pen turns.
///
/// The curve is as round as [`ROUNDING`] says, less wherever that wouldn't fit: it never
/// reaches past the middle of either wall or into a doorway, and at a sharp corner it tightens
/// so that the point the corner was drawn at stays inside the wall — a room traced to that
/// point must not poke out of its own walls.
fn finishes(level: &Level, index: usize) -> (Finish, Finish) {
    let Some(wall) = level.walls.get(index) else {
        return (Finish::Flat, Finish::Flat);
    };
    let at = |corner: Point, away: Point| {
        let mut meeting = level
            .walls
            .iter()
            .enumerate()
            .filter(|(which, other)| {
                *which != index && (other.from == corner || other.to == corner)
            })
            .map(|(_, other)| other);
        let Some(other) = meeting.next() else {
            return Finish::Flat;
        };
        if meeting.next().is_some()
            || other.thickness != wall.thickness
            || runs_through(level, corner)
        {
            return Finish::Capped;
        }
        let far = if other.from == corner {
            other.to
        } else {
            other.from
        };
        let room = room_at(wall, corner).min(room_at(other, corner));
        turn(corner, away, far, f64::from(wall.thickness), room)
            .map_or(Finish::Capped, Finish::Turn)
    };
    (at(wall.from, wall.to), at(wall.to, wall.from))
}

/// The curve a wall takes round a corner into another of the same thickness, or `None` where
/// there isn't a curve to draw: the two carry straight on, double back, or have no room.
fn turn(corner: Point, away: Point, far: Point, thickness: f64, room: f64) -> Option<Turn> {
    let (mine, theirs) = (direction(corner, away), direction(corner, far));
    // The angle at the corner between the two walls running away from it: a half turn when
    // they carry straight on, nothing when one doubles back along the other.
    let angle = (mine.0 * theirs.0 + mine.1 * theirs.1)
        .clamp(-1.0, 1.0)
        .acos();
    if angle < SHARPEST {
        return None;
    }
    let (sin, tan) = ((angle / 2.0).sin(), (angle / 2.0).tan());
    // The drawn corner is this far outside the arc's middle line for each centimetre of
    // radius; it has to stay within half a thickness of it to stay inside the wall.
    let outside = 1.0 / sin - 1.0;
    let radius = if outside > 1e-9 {
        (thickness * ROUNDING).min(thickness / 2.0 / outside)
    } else {
        thickness * ROUNDING
    };
    let back = (radius / tan).min(room);
    if back < 0.5 {
        return None;
    }
    let radius = back * tan;
    // The arc's centre is on the line that halves the corner, on the inside of it.
    let (bx, by) = (mine.0 + theirs.0, mine.1 + theirs.1);
    let reach = bx.hypot(by);
    let (cx, cy) = (
        f64::from(corner.x) + bx / reach * radius / sin,
        f64::from(corner.y) + by / reach * radius / sin,
    );
    let start = (
        f64::from(corner.x) + mine.0 * back,
        f64::from(corner.y) + mine.1 * back,
    );
    let end = (
        f64::from(corner.x) + theirs.0 * back,
        f64::from(corner.y) + theirs.1 * back,
    );
    let from = (start.1 - cy).atan2(start.0 - cx);
    let sweep = short_way((end.1 - cy).atan2(end.0 - cx) - from);
    // Half-way, and a little over.
    let share = (sweep.abs() / 2.0 + 0.06).min(sweep.abs()) * sweep.signum();
    let stop = from + share;
    Some(Turn {
        back,
        radius,
        to: (cx + radius * stop.cos(), cy + radius * stop.sin()),
        // With y running down the page, a growing angle is a clockwise one.
        clockwise: sweep > 0.0,
    })
}

/// An angle brought into the half turn either side of nothing: the short way round.
fn short_way(mut angle: f64) -> f64 {
    while angle > std::f64::consts::PI {
        angle -= std::f64::consts::TAU;
    }
    while angle < -std::f64::consts::PI {
        angle += std::f64::consts::TAU;
    }
    angle
}

/// How much of a wall there is to round a corner with at one of its ends, in centimetres: up to
/// the first doorway, and never past the wall's middle — the other end may want its half.
fn room_at(wall: &Wall, corner: Point) -> f64 {
    let length = wall.length();
    let runs = solid_runs(wall);
    let solid = if wall.from == corner {
        runs.first()
            .filter(|(start, _)| *start <= 0.0)
            .map_or(0.0, |(start, end)| end - start)
    } else {
        runs.last()
            .filter(|(_, end)| *end >= length)
            .map_or(0.0, |(start, end)| end - start)
    };
    solid.min(length / 2.0)
}

/// Whether some wall carries on through a point rather than ending at it: the top of a T.
fn runs_through(level: &Level, corner: Point) -> bool {
    let at = (f64::from(corner.x), f64::from(corner.y));
    level.walls.iter().any(|wall| {
        if wall.from == corner || wall.to == corner {
            return false;
        }
        let (along, off) = on_wall_at(wall.from, wall.to, at);
        off < 0.5 && along > 0.5 && along < wall.length() - 0.5
    })
}

/// The inside of one corner, rounded: the sliver between the two walls' faces and the arc that
/// joins them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fillet {
    /// Where the two faces meet.
    corner: (f64, f64),
    /// Where the arc leaves one face and where it lands on the other.
    from: (f64, f64),
    to: (f64, f64),
    radius: f64,
    /// Which way the arc bends from `to` back to `from`, as SVG's sweep flag has it.
    clockwise: bool,
}

/// One wall as seen from a joint: which way it leaves, how thick, and how far it stays solid.
struct Arm {
    toward: (f64, f64),
    half: f64,
    room: f64,
}

/// The inside corners that want rounding and that [`finishes`] can't reach.
///
/// A corner of two walls is one curve, inside and out. Everything else that joins — a T, a
/// cross, two walls of different thicknesses, a wall that ends against the side of another —
/// keeps its straight walls and has each inside corner filled with a small curve instead, the
/// way plaster rounds the foot of one.
fn fillets(level: &Level) -> Vec<Fillet> {
    let mut joints: Vec<Point> = level
        .walls
        .iter()
        .flat_map(|wall| [wall.from, wall.to])
        .collect();
    joints.sort_by_key(|point| (point.x, point.y));
    joints.dedup();

    let mut fillets = Vec::new();
    for joint in joints {
        let at = (f64::from(joint.x), f64::from(joint.y));
        let mut arms = Vec::new();
        let mut curved = false;
        for (index, wall) in level.walls.iter().enumerate() {
            let half = f64::from(wall.thickness) / 2.0;
            if wall.from == joint || wall.to == joint {
                let away = if wall.from == joint {
                    wall.to
                } else {
                    wall.from
                };
                let (from, to) = finishes(level, index);
                let finish = if wall.from == joint { from } else { to };
                curved |= matches!(finish, Finish::Turn(_));
                arms.push(Arm {
                    toward: direction(joint, away),
                    half,
                    room: room_at(wall, joint),
                });
                continue;
            }
            let length = wall.length();
            let (along, off) = on_wall_at(wall.from, wall.to, at);
            if off >= 0.5 || along <= 0.5 || along >= length - 0.5 {
                continue;
            }
            // A wall running through the joint leaves it both ways.
            let (ahead, behind) = solid_around(wall, along);
            let forward = direction(wall.from, wall.to);
            arms.push(Arm {
                toward: forward,
                half,
                room: ahead,
            });
            arms.push(Arm {
                toward: (-forward.0, -forward.1),
                half,
                room: behind,
            });
        }
        if curved || arms.len() < 2 {
            continue;
        }
        arms.sort_by(|a, b| {
            let (a, b) = (a.toward.1.atan2(a.toward.0), b.toward.1.atan2(b.toward.0));
            a.total_cmp(&b)
        });
        for (index, one) in arms.iter().enumerate() {
            let next = &arms[(index + 1) % arms.len()];
            if let Some(fillet) = fillet(at, one, next) {
                fillets.push(fillet);
            }
        }
    }
    fillets
}

/// The curve inside the corner between one wall and the next one round the joint, if the gap
/// between them is a corner at all: not two walls lying along each other, and not a wall
/// carrying straight on.
fn fillet(joint: (f64, f64), one: &Arm, next: &Arm) -> Option<Fillet> {
    let gap = (next.toward.1.atan2(next.toward.0) - one.toward.1.atan2(one.toward.0))
        .rem_euclid(std::f64::consts::TAU);
    if !(SHARPEST..=std::f64::consts::PI - 0.17).contains(&gap) {
        return None;
    }
    let (sin, tan) = (gap.sin(), (gap / 2.0).tan());
    // Where the two faces meet: each wall's face is the other wall's half-thickness along it.
    let (along_one, along_next) = (next.half / sin, one.half / sin);
    let corner = (
        joint.0 + one.toward.0 * along_one + next.toward.0 * along_next,
        joint.1 + one.toward.1 * along_one + next.toward.1 * along_next,
    );
    let radius = (ROUNDING - 0.5) * 2.0 * one.half.min(next.half);
    let reach = (radius / tan)
        .min(one.room - along_one)
        .min(next.room - along_next);
    if reach < 0.5 {
        return None;
    }
    let radius = reach * tan;
    let from = (
        corner.0 + one.toward.0 * reach,
        corner.1 + one.toward.1 * reach,
    );
    let to = (
        corner.0 + next.toward.0 * reach,
        corner.1 + next.toward.1 * reach,
    );
    // The arc's centre, out along the line that halves the gap.
    let (bx, by) = (one.toward.0 + next.toward.0, one.toward.1 + next.toward.1);
    let length = bx.hypot(by);
    let out = radius / (gap / 2.0).sin();
    let (cx, cy) = (corner.0 + bx / length * out, corner.1 + by / length * out);
    let sweep = short_way((from.1 - cy).atan2(from.0 - cx) - (to.1 - cy).atan2(to.0 - cx));
    Some(Fillet {
        corner,
        from,
        to,
        radius,
        clockwise: sweep > 0.0,
    })
}

/// How far a wall stays solid either side of a point along it: `(ahead, behind)`, in
/// centimetres, each stopping at a doorway or at the end of the wall.
fn solid_around(wall: &Wall, at: f64) -> (f64, f64) {
    solid_runs(wall)
        .into_iter()
        .find(|(start, end)| *start <= at && at <= *end)
        .map_or((0.0, 0.0), |(start, end)| (end - at, at - start))
}

/// The unit vector from one point towards another, or a default when they're the same place.
fn direction(from: Point, to: Point) -> (f64, f64) {
    // Each coordinate first, then the subtraction: see `Point::distance_to`.
    let dx = f64::from(to.x) - f64::from(from.x);
    let dy = f64::from(to.y) - f64::from(from.y);
    let length = dx.hypot(dy);
    if length <= f64::EPSILON {
        return (1.0, 0.0);
    }
    (dx / length, dy / length)
}

/// The angle of the corner the two lines make at the node — the side the wall and the line
/// being drawn actually enclose, which is the angle a corner of a room is meant to be read as,
/// so it is what the live label says and what the arc of the corner traces. Straight through is
/// 180°, a square corner 90°, doubling back 0°.
fn angle_at(behind: Point, node: Point, onward: Point) -> f64 {
    let (bx, by) = direction(node, behind);
    let (ox, oy) = direction(node, onward);
    let cos = (bx * ox + by * oy).clamp(-1.0, 1.0);
    cos.acos().to_degrees()
}

/// The line already drawn and the corner it ends at, for the line being drawn to turn on: the
/// wall the run has just laid and the run's node, or a room's last corner and the one before it.
/// Walls and rooms both, while a run or a trace is in progress. Takes the run's own last corner
/// rather than searching the level's walls for one that happens to end at the node: a run that
/// hasn't laid a segment yet has no corner to turn on, even if some unrelated wall in the plan
/// happens to end at the same point.
fn drawing_junction(
    tool: Tool,
    running: Option<Point>,
    wall_behind: Option<Point>,
    tracing: &[Point],
) -> Option<(Point, Point)> {
    match tool {
        Tool::Wall => Some((wall_behind?, running?)),
        Tool::Area => {
            let node = *tracing.last()?;
            let behind = *tracing.get(tracing.len().checked_sub(2)?)?;
            Some((behind, node))
        }
        _ => None,
    }
}

/// Points along the arc of a corner, in the plan's centimetres, ready for a `<polyline>` to
/// trace: from the point on the wall already drawn, back along it to the corner, then round to
/// the line reaching for the pointer. The two ends sit on the two drawn lines — that is what
/// makes it read as the angle between them — and it sweeps the side the corner actually bends
/// to. A square corner sweeps a quarter circle; a wall that runs straight on, or doubles back,
/// has no arc worth drawing.
fn arc_points(behind: Point, node: Point, onward: Point, radius: f64) -> Vec<[f64; 2]> {
    let (bx, by) = direction(node, behind);
    let (ox, oy) = direction(node, onward);
    // A short snapped run can be closer to the corner than the arc's usual radius; clamped to
    // both segments, the arc still lands on the two lines rather than floating past one of them.
    let radius = radius
        .min(node.distance_to(behind))
        .min(node.distance_to(onward));
    let start = by.atan2(bx);
    let mut sweep = oy.atan2(ox) - start;
    // The shorter way around, signed: the corner bends one way or the other, and the sign of
    // the sweep is the side its inside is on.
    while sweep > std::f64::consts::PI {
        sweep -= std::f64::consts::TAU;
    }
    while sweep < -std::f64::consts::PI {
        sweep += std::f64::consts::TAU;
    }
    // Straight through and doubling back are the two degenerate corners: either way the two
    // lines are the same line, and there is no wedge to trace.
    if sweep.abs() < 0.05 || sweep.abs() > std::f64::consts::PI - 0.05 {
        return Vec::new();
    }
    let steps = 24;
    let (nx, ny) = (f64::from(node.x), f64::from(node.y));
    // The arc opens from the wall itself — the first point is back along it from the node — so
    // it is rooted in the drawing rather than floating off the wall's far end.
    (0..=steps)
        .map(|i| {
            let angle = start + sweep * (i as f64 / steps as f64);
            [nx + radius * angle.cos(), ny + radius * angle.sin()]
        })
        .collect()
}

/// A point a given distance along a wall, with the wall's direction and its left-hand normal.
fn along(wall: &Wall, at: f64) -> ((f64, f64), (f64, f64), (f64, f64)) {
    point_along(wall.from, wall.to, at)
}

/// The same, for a segment that isn't a wall yet.
fn point_along(from: Point, to: Point, at: f64) -> ((f64, f64), (f64, f64), (f64, f64)) {
    let (ax, ay) = (f64::from(from.x), f64::from(from.y));
    let (dx, dy) = (f64::from(to.x) - ax, f64::from(to.y) - ay);
    let length = dx.hypot(dy);
    if length <= f64::EPSILON {
        return ((ax, ay), (1.0, 0.0), (0.0, 1.0));
    }
    let unit = (dx / length, dy / length);
    (
        (ax + unit.0 * at, ay + unit.1 * at),
        unit,
        (-unit.1, unit.0),
    )
}

/// The stretches of a wall that are still wall, in centimetres from its `from` end. Overlapping
/// openings are one hole, which is what makes a door dragged over a window survive being drawn.
fn solid_runs(wall: &Wall) -> Vec<(f64, f64)> {
    runs(wall, |_| true)
}

/// The stretches of a wall left once the openings `is_hole` picks are cut out of it. A door is
/// a hole to something looking through it; a window isn't.
fn runs(wall: &Wall, is_hole: impl Fn(&Opening) -> bool) -> Vec<(f64, f64)> {
    let length = wall.length();
    let mut holes: Vec<(f64, f64)> = wall
        .openings
        .iter()
        .filter(|opening| is_hole(opening))
        .map(|opening| {
            let half = f64::from(opening.width) / 2.0;
            let at = f64::from(opening.at);
            ((at - half).max(0.0), (at + half).min(length))
        })
        .filter(|(start, end)| end > start)
        .collect();
    holes.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut runs = Vec::new();
    let mut cursor = 0.0_f64;
    for (start, end) in holes {
        if start > cursor {
            runs.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < length {
        runs.push((cursor, length));
    }
    runs
}

/// Where an opening's middle has to be for it to sit inside its wall.
fn fit_opening(wanted: f64, width: u32, length: f64) -> i32 {
    let half = f64::from(width) / 2.0;
    if length <= f64::from(width) {
        return (length / 2.0).round() as i32;
    }
    wanted.clamp(half, length - half).round() as i32
}

/// The wall nearest a point, if one is within reach.
fn nearest_wall(level: &Level, world: (f64, f64), reach: f64) -> Option<usize> {
    level
        .walls
        .iter()
        .enumerate()
        .filter_map(|(index, wall)| {
            let (_, off) = on_wall_at(wall.from, wall.to, world);
            (off <= reach).then_some((off, index))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, index)| index)
}

/// What's under a point on the plan. An opening wins over the wall it's cut into: it is the
/// smaller, more specific thing, and it's drawn on top.
fn pick_at(level: &Level, world: (f64, f64), reach: f64) -> Option<Pick> {
    let mut best: Option<(f64, Pick)> = None;
    for (index, wall) in level.walls.iter().enumerate() {
        let (at, off) = on_wall_at(wall.from, wall.to, world);
        if off > reach + f64::from(wall.thickness) / 2.0 {
            continue;
        }
        let hole = wall.openings.iter().position(|opening| {
            (at - f64::from(opening.at)).abs() <= f64::from(opening.width) / 2.0
        });
        let (found, weight) = match hole {
            Some(hole) => (Pick::Opening(index, hole), off - reach),
            None => (Pick::Wall(index), off),
        };
        if best.is_none_or(|(best, _)| weight < best) {
            best = Some((weight, found));
        }
    }
    // A room only wins where no wall does: it is the big thing underneath, and picking one up by
    // clicking the wall along its edge would make the walls impossible to get at.
    best.map(|(_, found)| found).or_else(|| {
        level
            .areas
            .iter()
            .position(|placed| inside(&placed.points, world))
            .map(Pick::Area)
    })
}

/// Whether a point falls inside a shape, by the crossing-number rule: count the edges a ray cast
/// from the point crosses, and an odd count means inside. Works for the L-shaped and worse rooms
/// that real homes have, which is why it isn't a bounding box.
fn inside(points: &[Point], world: (f64, f64)) -> bool {
    let mut within = false;
    let mut previous = match points.last() {
        Some(point) => (f64::from(point.x), f64::from(point.y)),
        None => return false,
    };
    for point in points {
        let current = (f64::from(point.x), f64::from(point.y));
        if (current.1 > world.1) != (previous.1 > world.1) {
            let span = previous.1 - current.1;
            if span.abs() > f64::EPSILON {
                let crossing = current.0 + (world.1 - current.1) / span * (previous.0 - current.0);
                if world.0 < crossing {
                    within = !within;
                }
            }
        }
        previous = current;
    }
    within
}

/// Where a corner of a room goes, in order of preference: onto a corner something already has,
/// then onto the line of a wall, and only then onto the grid.
///
/// Rooms are bounded by walls, so a corner of one almost always wants to be *on* a wall rather
/// than near it — and the grid can't be relied on for that, because a wall put down before the
/// snap step was changed needn't sit on it. Walls win over the grid; they don't replace it, so a
/// room can still be traced across open floor.
fn trace_at(level: &Level, world: (f64, f64), view: Viewport, snap: Snap) -> Point {
    let reach = CORNER * view.cm_per_pixel();
    let corners = level
        .walls
        .iter()
        .flat_map(|wall| [wall.from, wall.to])
        .chain(
            level
                .areas
                .iter()
                .flat_map(|placed| placed.points.iter().copied()),
        );
    let mut nearest: Option<(f64, Point)> = None;
    for corner in corners {
        let away = (world.0 - f64::from(corner.x)).hypot(world.1 - f64::from(corner.y));
        if away <= reach && nearest.is_none_or(|(best, _)| away < best) {
            nearest = Some((away, corner));
        }
    }
    if let Some((_, corner)) = nearest {
        return corner;
    }
    // Not on a corner, but perhaps along a wall: the nearest point on its line, rounded to the
    // centimetre so the file still holds whole numbers.
    let mut along: Option<(f64, Point)> = None;
    for wall in &level.walls {
        let (at, off) = on_wall_at(wall.from, wall.to, world);
        if off > reach {
            continue;
        }
        let (point, _, _) = point_along(wall.from, wall.to, at);
        let on = Point::new(point.0.round() as i32, point.1.round() as i32);
        if along.is_none_or(|(best, _)| off < best) {
            along = Some((off, on));
        }
    }
    if let Some((_, on)) = along {
        return on;
    }
    let step = snap.step();
    Point::new(round(world.0, step), round(world.1, step))
}

/// A dragged wall end can push an opening off the end of the shortened wall, which the core
/// would refuse to save. Pulling them back in is kinder than refusing the drag.
fn trim(wall: &mut Wall) {
    let length = wall.length();
    for opening in &mut wall.openings {
        opening.width = opening.width.min(length.floor().max(1.0) as u32);
        opening.at = fit_opening(f64::from(opening.at), opening.width, length);
    }
}

/// Where a room's name is drawn, in centimetres: the middle of the room, shifted by however far
/// the label was dragged.
fn label_at(placed: &PlacedArea) -> Option<Point> {
    placed.middle().map(|middle| {
        Point::new(
            middle.x.saturating_add(placed.label.x),
            middle.y.saturating_add(placed.label.y),
        )
    })
}

/// The furthest corners of everything drawn, for framing it.
fn extent(level: &Level) -> Option<(Point, Point)> {
    let points = level
        .walls
        .iter()
        .flat_map(|wall| [wall.from, wall.to])
        .chain(
            level
                .areas
                .iter()
                .flat_map(|placed| placed.points.iter().copied()),
        )
        .chain(level.devices.iter().map(|placed| placed.at));
    let mut bounds: Option<(Point, Point)> = None;
    for point in points {
        bounds = Some(match bounds {
            None => (point, point),
            Some((low, high)) => (
                Point::new(low.x.min(point.x), low.y.min(point.y)),
                Point::new(high.x.max(point.x), high.y.max(point.y)),
            ),
        });
    }
    bounds
}

/// A length in the words a plan uses: metres once it's a metre.
fn metres(cm: f64) -> String {
    if cm < 100.0 {
        format!("{} cm", cm.round())
    } else {
        format!("{:.2} m", cm / 100.0)
    }
}

/// Whether the keyboard belongs to something being typed into, so Delete doesn't remove a wall
/// while somebody is editing a field.
fn typing() -> bool {
    document().active_element().is_some_and(|element| {
        matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
    })
}

/// The tools, in the order they're used: pick things up, draw walls, put things in them.
/// Icons are 24×24 strokes written here, so `inner_html` only ever holds these literals.
const TOOLS: [(Tool, &str, &str); 6] = [
    (
        Tool::Select,
        "Select",
        r#"<path d="M5 3l6 16 2.2-6.2L19.5 10z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/>"#,
    ),
    (
        Tool::Wall,
        "Wall",
        r#"<path d="M3 17h18" stroke="currentColor" stroke-width="3.2" stroke-linecap="round"/><circle cx="3" cy="17" r="2.2" fill="none" stroke="currentColor" stroke-width="1.6"/><circle cx="21" cy="17" r="2.2" fill="none" stroke="currentColor" stroke-width="1.6"/>"#,
    ),
    (
        Tool::Door,
        "Door",
        r#"<path d="M4 20h4M16 20h4" stroke="currentColor" stroke-width="3" stroke-linecap="round"/><path d="M8 20V6h8" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M16 6a14 14 0 0 1 0 14" fill="none" stroke="currentColor" stroke-width="1.4" stroke-dasharray="2.5 2"/>"#,
    ),
    (
        Tool::Window,
        "Window",
        r#"<path d="M3 12h3M18 12h3" stroke="currentColor" stroke-width="3.2" stroke-linecap="round"/><path d="M6 9v6M18 9v6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/><path d="M6 12h12" stroke="currentColor" stroke-width="1.6"/>"#,
    ),
    (
        Tool::Area,
        "Room",
        r#"<path d="M3.5 20.5V8l8.5-4.5L20.5 8v12.5z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M3.5 14h7V20.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/>"#,
    ),
    (
        Tool::Device,
        "Device",
        r#"<rect x="6" y="6" width="12" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="12" cy="12" r="2.4" fill="currentColor"/>"#,
    ),
];

/// Back a step, and forward again.
const UNDO: &str = r#"<path d="M4 9h10a5 5 0 0 1 0 10H8" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><path d="M8 5 4 9l4 4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;
const REDO: &str = r#"<path d="M20 9H10a5 5 0 0 0 0 10h6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><path d="m16 5 4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;

/// Take away what's picked up.
const BIN: &str = r#"<path d="M4 7h16M10 7V5h4v2M6 7l1 13h10l1-13" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(from: (i32, i32), to: (i32, i32)) -> Wall {
        Wall::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
    }

    fn points(corners: &[(i32, i32)]) -> Vec<Point> {
        corners.iter().map(|(x, y)| Point::new(*x, *y)).collect()
    }

    fn room(id: &str, corners: &[(i32, i32)]) -> PlacedArea {
        PlacedArea {
            area: id.parse().expect("a valid area id"),
            points: points(corners),
            label: Point::new(0, 0),
        }
    }

    #[test]
    fn the_screen_and_the_plan_agree_both_ways() {
        let view = Viewport {
            scale: 0.7,
            pan: (40.0, 12.5),
        };
        let (x, y) = view.screen(Point::new(300, -150));
        let (wx, wy) = view.world(x, y);
        assert!(
            (wx - 300.0).abs() < 1e-9 && (wy + 150.0).abs() < 1e-9,
            "{wx} {wy}"
        );
    }

    /// The gaps a wall's openings leave, which is how a door becomes a hole rather than a
    /// decoration drawn over solid wall.
    #[test]
    fn a_wall_is_drawn_around_the_holes_in_it() {
        let mut wall = wall((0, 0), (400, 0));
        wall.openings.push(Opening::new(OpeningKind::Door, 100, 80));
        wall.openings
            .push(Opening::new(OpeningKind::Window, 300, 100));
        assert_eq!(
            solid_runs(&wall),
            vec![(0.0, 60.0), (140.0, 250.0), (350.0, 400.0)]
        );
    }

    /// Two openings that overlap are one hole. Dragging a door over a window makes this happen,
    /// and a wall drawn from unsorted, overlapping gaps would come out inside out.
    #[test]
    fn overlapping_holes_are_one_hole() {
        let mut wall = wall((0, 0), (400, 0));
        wall.openings
            .push(Opening::new(OpeningKind::Window, 220, 100));
        wall.openings.push(Opening::new(OpeningKind::Door, 200, 80));
        assert_eq!(solid_runs(&wall), vec![(0.0, 160.0), (270.0, 400.0)]);
    }

    /// A wall with a door across its whole length has no wall left, not a run of negative length.
    #[test]
    fn a_wall_that_is_all_door_has_nothing_solid_in_it() {
        let mut wall = wall((0, 0), (100, 0));
        wall.openings.push(Opening::new(OpeningKind::Door, 50, 200));
        assert_eq!(solid_runs(&wall), Vec::<(f64, f64)>::new());
    }

    /// A door opens to the side it was told whichever end it hangs from, on a wall running
    /// either way: its latch ends up a door's width out from the wall, on that side.
    #[test]
    fn a_door_swings_to_the_side_it_was_told() {
        use irori_types::{Hinge, Side};
        for (from, to) in [((0, 0), (400, 0)), ((0, 0), (0, 400)), ((400, 300), (0, 0))] {
            let mut wall = wall(from, to);
            for side in [Side::Left, Side::Right] {
                for hinge in [Hinge::Near, Hinge::Far] {
                    wall.openings = vec![Opening {
                        side,
                        hinge,
                        ..Opening::new(OpeningKind::Door, 200, 80)
                    }];
                    let slab = leaves(&wall, &wall.openings[0]);
                    assert_eq!(slab.len(), 1, "a door has one leaf");
                    let (near, _, normal) = along(&wall, 160.0);
                    let (far, _, _) = along(&wall, 240.0);
                    let pivot = if hinge == Hinge::Near { near } else { far };
                    assert!(
                        (slab[0].pivot.0 - pivot.0).hypot(slab[0].pivot.1 - pivot.1) < 1e-9,
                        "hung from the {hinge:?} end"
                    );
                    let open = slab[0].opened(slab[0].latch);
                    let out = (open.0 - pivot.0) * normal.0 + (open.1 - pivot.1) * normal.1;
                    let wanted = if side == Side::Left { 80.0 } else { -80.0 };
                    assert!(
                        (out - wanted).abs() < 1e-6,
                        "{from:?}→{to:?} {side:?} {hinge:?}: {out}"
                    );
                }
            }
        }
    }

    /// A window's two panes both stand out on the side it opens to; a small window has one.
    #[test]
    fn a_window_has_two_panes_unless_it_is_a_small_one() {
        let mut wall = wall((0, 0), (400, 0));
        wall.openings = vec![Opening {
            side: irori_types::Side::Right,
            ..Opening::new(OpeningKind::Window, 200, 100)
        }];
        let panes = leaves(&wall, &wall.openings[0]);
        assert_eq!(panes.len(), 2);
        assert_eq!(
            (panes[0].pivot, panes[1].pivot),
            ((150.0, 0.0), (250.0, 0.0))
        );
        for pane in &panes {
            let open = pane.opened(pane.latch);
            assert!((open.1 + 49.0).abs() < 1e-6, "out to the right: {open:?}");
        }
        wall.openings = vec![Opening::new(OpeningKind::Window, 200, 59)];
        assert_eq!(leaves(&wall, &wall.openings[0]).len(), 1);
    }

    #[test]
    fn an_opening_is_kept_inside_its_wall() {
        assert_eq!(fit_opening(0.0, 80, 400.0), 40, "pushed off the near end");
        assert_eq!(fit_opening(400.0, 80, 400.0), 360, "off the far end");
        assert_eq!(fit_opening(150.0, 80, 400.0), 150, "already fits");
        assert_eq!(fit_opening(10.0, 80, 60.0), 30, "wider than the wall");
    }

    /// Shortening a wall under its own door shrinks the door rather than leaving a plan the
    /// core would refuse to save.
    #[test]
    fn dragging_a_wall_short_pulls_its_door_in_with_it() {
        let mut short = wall((0, 0), (60, 0));
        short
            .openings
            .push(Opening::new(OpeningKind::Door, 200, 80));
        trim(&mut short);
        let level = Level {
            walls: vec![short],
            ..Level::default()
        };
        assert!(level.walls[0].openings[0].at <= 60, "{level:?}");
    }

    /// Corners are what rooms are made of: a new point within reach of one lands exactly on it,
    /// rather than on the grid square next to it.
    #[test]
    fn a_new_point_lands_on_a_corner_that_is_already_there() {
        let view = Viewport {
            scale: 0.5,
            pan: (0.0, 0.0),
        };
        let plan = Level {
            walls: vec![wall((0, 0), (403, 0))],
            ..Level::default()
        };
        assert_eq!(
            place(&plan, (399.0, 4.0), view, Snap::Grid, None),
            Point::new(403, 0),
            "close to the far corner"
        );
        assert_eq!(
            place(&plan, (252.0, 97.0), view, Snap::Grid, None),
            Point::new(250, 100),
            "nowhere near one: the grid"
        );
        assert_eq!(
            place(
                &plan,
                (399.0, 4.0),
                view,
                Snap::Grid,
                Some(Point::new(403, 0))
            ),
            Point::new(400, 0),
            "the corner being dragged doesn't catch itself"
        );
    }

    /// The point of welding: a room drawn closed stays closed when a corner is pulled about.
    #[test]
    fn dragging_a_corner_carries_every_wall_that_meets_there() {
        let mut plan = Level {
            walls: vec![
                wall((0, 0), (600, 0)),
                wall((600, 0), (600, 450)),
                wall((600, 450), (0, 450)),
                wall((0, 450), (0, 0)),
            ],
            ..Level::default()
        };
        shift(&mut plan, &[(Point::new(600, 0), Point::new(700, -50))]);

        assert_eq!(plan.walls[0].to, Point::new(700, -50), "the wall dragged");
        assert_eq!(
            plan.walls[1].from,
            Point::new(700, -50),
            "and its neighbour"
        );
        assert_eq!(
            plan.walls[2].from,
            Point::new(600, 450),
            "the far corner stays"
        );
        assert!(
            plan.walls.iter().all(|wall| wall.from != wall.to),
            "and nothing was flattened"
        );
    }

    /// A room is a note about where the walls are, so a wall that moves takes the room with it.
    /// The other way round is deliberately not true, and the second half of this says so.
    #[test]
    fn moving_a_wall_takes_the_room_traced_on_it() {
        let mut plan = Level {
            walls: vec![wall((0, 0), (600, 0)), wall((600, 0), (600, 450))],
            areas: vec![room("kitchen", &[(0, 0), (600, 0), (600, 450), (0, 450)])],
            ..Level::default()
        };
        shift(&mut plan, &[(Point::new(600, 0), Point::new(700, -50))]);

        assert_eq!(plan.walls[0].to, Point::new(700, -50), "the wall");
        assert_eq!(
            plan.areas[0].points[1],
            Point::new(700, -50),
            "and the room's corner standing on it"
        );
        assert_eq!(
            plan.areas[0].points[2],
            Point::new(600, 450),
            "but only that corner"
        );
    }

    /// Moving a wall stretches the walls joined to it rather than tearing the room open.
    #[test]
    fn dragging_a_whole_wall_takes_its_neighbours_with_it() {
        let mut plan = Level {
            walls: vec![
                wall((0, 0), (600, 0)),
                wall((600, 0), (600, 450)),
                wall((0, 450), (0, 0)),
            ],
            ..Level::default()
        };
        shift(
            &mut plan,
            &[
                (Point::new(0, 0), Point::new(0, 50)),
                (Point::new(600, 0), Point::new(600, 50)),
            ],
        );

        assert_eq!(plan.walls[0], wall((0, 50), (600, 50)), "moved");
        assert_eq!(plan.walls[1], wall((600, 50), (600, 450)), "shortened");
        assert_eq!(plan.walls[2], wall((0, 450), (0, 50)), "shortened");
    }

    /// A corner dragged onto the far end of its own wall would leave a wall with no length,
    /// which is a plan that can't be drawn. The frame is dropped instead.
    #[test]
    fn a_drag_that_would_flatten_a_wall_is_dropped() {
        let before = Level {
            walls: vec![wall((0, 0), (100, 0))],
            ..Level::default()
        };
        let mut plan = before.clone();
        shift(&mut plan, &[(Point::new(0, 0), Point::new(100, 0))]);
        assert_eq!(plan, before);
    }

    /// Panning has no end, so a point can be asked for a very long way from anywhere anyone
    /// would draw. Rounding one has to answer with a number rather than overflow on the way.
    #[test]
    fn rounding_a_point_at_the_edge_of_the_world_stays_inside_it() {
        for step in [1, 10, 25, 100] {
            for value in [
                f64::from(i32::MAX),
                f64::from(i32::MIN),
                1e18,
                -1e18,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ] {
                let rounded = round(value, step);
                assert_eq!(
                    rounded % step,
                    0,
                    "{value} to the nearest {step} is still on the grid"
                );
                // The multiplication that used to overflow, done again where a panic would show.
                assert!(
                    i64::from(rounded).abs() <= i64::from(i32::MAX),
                    "{value} to the nearest {step} fits"
                );
            }
        }
        assert_eq!(round(137.4, 10), 140, "and it still rounds");
        assert_eq!(round(-137.4, 10), -140);
        assert_eq!(round(137.4, 0), 137, "a step of nothing is a step of one");
    }

    /// The point of a step of your own: a wall that really is 137 cm long.
    #[test]
    fn a_custom_step_is_what_points_round_to() {
        let view = Viewport {
            scale: 0.5,
            pan: (0.0, 0.0),
        };
        let empty = Level::default();
        let at = |snap| place(&empty, (137.4, 62.6), view, snap, None);

        assert_eq!(at(Snap::Grid), Point::new(140, 60));
        assert_eq!(
            at(Snap::Custom(1)),
            Point::new(137, 63),
            "to the centimetre"
        );
        assert_eq!(at(Snap::Custom(25)), Point::new(125, 75));
        assert_eq!(
            at(Snap::Custom(0)),
            at(Snap::Custom(1)),
            "a step below the range is the finest one, never a division by zero"
        );
        assert_eq!(
            at(Snap::Custom(10_000)),
            at(Snap::Custom(100)),
            "and above it"
        );
    }

    /// Switching to a custom step starts from whatever was in force, so nothing on the plan
    /// moves at the moment of switching.
    #[test]
    fn a_custom_step_starts_where_the_grid_left_off() {
        assert_eq!(Snap::Grid.step(), SNAP);
        assert_eq!(Snap::Custom(Snap::Grid.step()).step(), SNAP);
        assert!(!Snap::Grid.is_custom());
        assert!(Snap::Custom(SNAP).is_custom());
    }

    fn turned(finish: Finish) -> Turn {
        match finish {
            Finish::Turn(turn) => turn,
            other => panic!("expected a curve, got {other:?}"),
        }
    }

    /// A corner of two walls is one curve: each stops short of the corner by the same distance
    /// and an arc carries it round, so the inside and the outside both come out round.
    #[test]
    fn a_corner_of_two_walls_is_one_curve() {
        let plan = Level {
            walls: vec![
                wall((0, 0), (400, 0)),
                wall((400, 0), (400, 300)),
                wall((600, 600), (900, 600)),
            ],
            ..Level::default()
        };
        let thickness = f64::from(Wall::DEFAULT_THICKNESS);

        let (from, to) = finishes(&plan, 0);
        assert_eq!(from, Finish::Flat, "a free end stops where it was drawn");
        let mine = turned(to);
        assert!(
            (mine.radius - thickness * ROUNDING).abs() < 1e-9,
            "as round as asked: {mine:?}"
        );
        assert!(
            (mine.back - mine.radius).abs() < 1e-9,
            "at a right angle it stops a radius short: {mine:?}"
        );
        assert!(
            mine.radius > thickness / 2.0,
            "round enough that the inside is a curve too"
        );
        let theirs = turned(finishes(&plan, 1).0);
        assert!(
            (theirs.back - mine.back).abs() < 1e-9,
            "and so does the other wall"
        );
        assert_ne!(
            mine.clockwise, theirs.clockwise,
            "each bends the opposite way as it's drawn, into the same corner"
        );
        // The two halves meet in the middle of the corner, on the inside of the drawn point.
        for half in [mine, theirs] {
            assert!(half.to.0 < 400.0 && half.to.1 > 0.0, "{half:?}");
            assert!((half.to.0 - 400.0).hypot(half.to.1) < thickness, "{half:?}");
        }
        assert_eq!(
            finishes(&plan, 2),
            (Finish::Flat, Finish::Flat),
            "a wall on its own gets nothing"
        );
        assert!(fillets(&plan).is_empty(), "and a curve needs no filling in");
    }

    /// Two walls carrying straight on have nothing to round; a sharp corner still curves, but
    /// tighter, so the point it was drawn at stays inside the wall.
    #[test]
    fn how_round_a_corner_is_follows_the_angle() {
        let straight = Level {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (800, 0))],
            ..Level::default()
        };
        assert_eq!(finishes(&straight, 0).1, Finish::Capped, "nothing to curve");
        assert!(fillets(&straight).is_empty());

        let sharp = Level {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (0, 300))],
            ..Level::default()
        };
        let thickness = f64::from(Wall::DEFAULT_THICKNESS);
        let tight = turned(finishes(&sharp, 0).1);
        assert!(tight.radius < thickness * ROUNDING, "tighter: {tight:?}");
        // The drawn corner is no further from the arc than the wall is thick either side of it.
        let half_angle = (300.0_f64).atan2(400.0) / 2.0;
        let outside = tight.radius * (1.0 / half_angle.sin() - 1.0);
        assert!(outside <= thickness / 2.0 + 1e-9, "{outside}");

        let hairpin = Level {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (0, 40))],
            ..Level::default()
        };
        assert_eq!(
            finishes(&hairpin, 0).1,
            Finish::Capped,
            "too sharp to curve"
        );
    }

    /// A curve never eats a doorway, and never takes more than its half of a short wall.
    #[test]
    fn a_curve_stays_out_of_a_doorway() {
        let mut front = wall((0, 0), (400, 0));
        front.openings.push(Opening::new(OpeningKind::Door, 395, 6));
        let plan = Level {
            walls: vec![front, wall((400, 0), (400, 300))],
            ..Level::default()
        };
        let near = turned(finishes(&plan, 0).1);
        assert!((near.back - 2.0).abs() < 1e-9, "up to the door: {near:?}");
        assert!(
            (turned(finishes(&plan, 1).0).back - 2.0).abs() < 1e-9,
            "and the other wall agrees, or the two halves wouldn't meet"
        );

        let stub = Level {
            walls: vec![wall((0, 0), (10, 0)), wall((10, 0), (10, 300))],
            ..Level::default()
        };
        assert!(turned(finishes(&stub, 0).1).back <= 5.0);
    }

    /// Where one curve can't carry the joint — three walls, or two of different thicknesses —
    /// the walls are capped and the inside corners are filled in round instead.
    #[test]
    fn a_joint_one_curve_cannot_carry_is_capped_and_filled() {
        let tee = Level {
            walls: vec![
                wall((0, 0), (400, 0)),
                wall((400, 0), (800, 0)),
                wall((400, 0), (400, 300)),
            ],
            ..Level::default()
        };
        assert_eq!(finishes(&tee, 0).1, Finish::Capped);
        assert_eq!(finishes(&tee, 2).0, Finish::Capped);
        let filled = fillets(&tee);
        assert_eq!(filled.len(), 2, "either side of the stem: {filled:?}");
        let half = f64::from(Wall::DEFAULT_THICKNESS) / 2.0;
        for fillet in &filled {
            assert!(
                ((fillet.corner.0 - 400.0).abs() - half).abs() < 1e-9
                    && (fillet.corner.1 - half).abs() < 1e-9,
                "where the faces meet: {fillet:?}"
            );
            assert!(fillet.radius > 0.0);
        }

        // The same T with the top drawn as one wall: the stem ends against its side.
        let butted = Level {
            walls: vec![wall((0, 0), (800, 0)), wall((400, 0), (400, 300))],
            ..Level::default()
        };
        assert_eq!(
            finishes(&butted, 1).0,
            Finish::Flat,
            "nothing ends there but itself"
        );
        assert_eq!(fillets(&butted).len(), 2);

        let mut thick = wall((0, 0), (400, 0));
        thick.thickness = 30;
        let uneven = Level {
            walls: vec![thick, wall((400, 0), (400, 300))],
            ..Level::default()
        };
        assert_eq!(finishes(&uneven, 0).1, Finish::Capped);
        assert_eq!(fillets(&uneven).len(), 1, "only the inside of the corner");
    }

    /// The angle a corner is read as — the one its two lines actually enclose: a wall running
    /// straight through a node is 180°, a square corner is 90°, and the side the corner bends
    /// to doesn't matter because the label says the size of it.
    #[test]
    fn the_angle_reads_the_corner_the_wall_and_line_enclose() {
        let angle = |node: (i32, i32), onward: (i32, i32)| {
            angle_at(
                Point::new(0, 0),
                Point::new(node.0, node.1),
                Point::new(onward.0, onward.1),
            )
        };
        assert!(
            (angle((100, 0), (200, 0)) - 180.0).abs() < 1e-9,
            "straight through"
        );
        assert!(
            (angle((100, 0), (100, 100)) - 90.0).abs() < 1e-9,
            "a square corner one way"
        );
        assert!(
            (angle((100, 0), (100, -100)) - 90.0).abs() < 1e-9,
            "and the other"
        );
        assert!(
            (angle((100, 0), (150, 50)) - 135.0).abs() < 1e-9,
            "a skirt of a corner"
        );
        assert!(
            (angle((100, 0), (0, 0)) - 0.0).abs() < 1e-9,
            "doubling straight back"
        );
    }

    /// The arc of a corner traces the corner itself: it opens from a point back on the wall and
    /// sweeps round to the line reaching for the pointer, so its two ends sit on the two lines
    /// and it curves the side the corner bends to. Straight through and doubling back are the
    /// degenerate corners — the two lines are the one line — and draw nothing at all.
    #[test]
    fn the_arc_of_a_corner_sits_between_the_two_lines() {
        let arc = |node: (i32, i32), onward: (i32, i32)| {
            arc_points(
                Point::new(0, 0),
                Point::new(node.0, node.1),
                Point::new(onward.0, onward.1),
                100.0,
            )
        };

        let right = arc((100, 0), (100, 100));
        assert_eq!(right.len(), 25, "one point per step, and both ends");
        let (first, last) = (
            *right.first().expect("the arc always has a first point"),
            *right.last().expect("and a last"),
        );
        let begin = |[x, y]: [f64; 2]| x.abs() < 1e-9 && y.abs() < 1e-9;
        let done = |[x, y]: [f64; 2]| (x - 100.0).abs() < 1e-9 && (y - 100.0).abs() < 1e-9;
        assert!(
            begin(first),
            "starts back along the east-west wall: {first:?}"
        );
        assert!(done(last), "ends on the north-south line: {last:?}");
        let middle = right[12];
        assert!(
            middle[0] < 100.0 && middle[1] > 0.0,
            "sweeps through the corner itself: {middle:?}"
        );

        let left = arc((100, 0), (100, -100));
        let left_middle = left[12];
        assert!(
            left_middle[0] < 100.0 && left_middle[1] < 0.0,
            "curves the other way: {left_middle:?}"
        );

        assert!(
            arc((100, 0), (200, 0)).is_empty(),
            "straight through has nothing to draw"
        );
        assert!(
            arc((100, 0), (0, 0)).is_empty(),
            "and neither has a line doubling back over the wall"
        );
    }

    #[test]
    fn what_is_under_the_pointer_is_the_opening_before_the_wall() {
        let mut front = wall((0, 0), (400, 0));
        front
            .openings
            .push(Opening::new(OpeningKind::Door, 200, 80));
        let plan = Level {
            walls: vec![front, wall((0, 300), (400, 300))],
            ..Level::default()
        };
        assert_eq!(
            pick_at(&plan, (200.0, 2.0), 20.0),
            Some(Pick::Opening(0, 0))
        );
        assert_eq!(pick_at(&plan, (50.0, 2.0), 20.0), Some(Pick::Wall(0)));
        assert_eq!(pick_at(&plan, (50.0, 298.0), 20.0), Some(Pick::Wall(1)));
        assert_eq!(pick_at(&plan, (200.0, 150.0), 20.0), None, "empty room");
    }

    /// An L-shaped room is the point of counting crossings rather than testing a box: the
    /// notch has to be outside the room even though it's inside its bounds.
    #[test]
    fn what_is_inside_a_room_counts_crossings() {
        let ell = points(&[
            (0, 0),
            (400, 0),
            (400, 200),
            (200, 200),
            (200, 400),
            (0, 400),
        ]);
        assert!(inside(&ell, (100.0, 100.0)), "the top-left");
        assert!(inside(&ell, (300.0, 100.0)), "the arm");
        assert!(inside(&ell, (100.0, 300.0)), "the leg");
        assert!(!inside(&ell, (300.0, 300.0)), "the notch is outside");
        assert!(!inside(&ell, (500.0, 100.0)), "and so is the garden");
        assert!(
            !inside(&[], (0.0, 0.0)),
            "a shape with no corners holds nothing"
        );
    }

    /// A room's corners want to be on the walls, which is why walls beat the grid. The grid is
    /// still there for tracing across open floor.
    #[test]
    fn a_rooms_corner_prefers_a_wall_to_the_grid() {
        let view = Viewport {
            scale: 0.5,
            pan: (0.0, 0.0),
        };
        // A wall whose ends are nowhere near the 10 cm grid.
        let level = Level {
            walls: vec![wall((3, 7), (403, 7))],
            ..Level::default()
        };
        let at = |world| trace_at(&level, world, view, Snap::Grid);

        assert_eq!(at((8.0, 12.0)), Point::new(3, 7), "a corner of the wall");
        assert_eq!(
            at((200.0, 14.0)),
            Point::new(200, 7),
            "along the wall, off the grid"
        );
        assert_eq!(
            at((198.0, 402.0)),
            Point::new(200, 400),
            "out in the open: the grid"
        );
    }

    /// Rooms share their corners with each other as well as with the walls, so a second room
    /// traced beside the first sits flush against it.
    #[test]
    fn a_rooms_corner_also_catches_another_rooms() {
        let view = Viewport {
            scale: 0.5,
            pan: (0.0, 0.0),
        };
        let level = Level {
            areas: vec![room("kitchen", &[(0, 0), (403, 0), (403, 307), (0, 307)])],
            ..Level::default()
        };
        assert_eq!(
            trace_at(&level, (400.0, 304.0), view, Snap::Grid),
            Point::new(403, 307)
        );
    }

    /// Walls are the thing a click is usually after; a room is the big shape underneath.
    #[test]
    fn a_click_on_a_wall_gets_the_wall_not_the_room_behind_it() {
        let level = Level {
            walls: vec![wall((0, 0), (400, 0))],
            areas: vec![room("kitchen", &[(0, 0), (400, 0), (400, 300), (0, 300)])],
            ..Level::default()
        };
        assert_eq!(pick_at(&level, (200.0, 2.0), 20.0), Some(Pick::Wall(0)));
        assert_eq!(pick_at(&level, (200.0, 150.0), 20.0), Some(Pick::Area(0)));
        assert_eq!(pick_at(&level, (600.0, 150.0), 20.0), None);
    }

    /// The same room is the same colour every time, whoever opens the page.
    #[test]
    fn a_rooms_tint_is_its_own_and_stays_put() {
        assert_eq!(tint_of("kitchen"), tint_of("kitchen"));
        assert!(tint_of("kitchen") < 6);
        assert!(tint_of("") < 6, "even an id that is somehow empty");
    }

    #[test]
    fn lengths_are_said_the_way_a_plan_says_them() {
        assert_eq!(metres(40.0), "40 cm");
        assert_eq!(metres(100.0), "1.00 m");
        assert_eq!(metres(425.0), "4.25 m");
    }

    /// A room's name sits at the middle until it's dragged, and then where it was put.
    #[test]
    fn a_rooms_name_goes_where_it_was_dragged() {
        let mut kitchen = room("kitchen", &[(0, 0), (400, 0), (400, 300), (0, 300)]);
        assert_eq!(
            label_at(&kitchen),
            Some(Point::new(200, 150)),
            "in the middle"
        );
        kitchen.label = Point::new(40, -60);
        assert_eq!(
            label_at(&kitchen),
            Some(Point::new(240, 90)),
            "shifted by the drag"
        );
        assert_eq!(
            label_at(&room("empty", &[])),
            None,
            "a room with no corners has no name to put"
        );
    }

    #[test]
    fn the_whole_plan_can_be_framed() {
        let plan = Level {
            walls: vec![wall((-50, 0), (400, 0)), wall((400, 0), (400, 300))],
            devices: vec![PlacedDevice::new(
                "demo_lamp".parse().expect("a valid device id"),
                Point::new(120, 420),
            )],
            ..Level::default()
        };
        assert_eq!(
            extent(&plan),
            Some((Point::new(-50, 0), Point::new(400, 420)))
        );
        assert_eq!(extent(&Level::default()), None);
    }
}
