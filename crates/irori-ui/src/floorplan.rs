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

use irori_types::{
    AreaId, Capabilities, Device, DeviceId, EntityId, FloorId, Floorplan, Level, Opening,
    OpeningKind, PlacedArea, PlacedDevice, Point, State, Wall,
};
use leptos::ev;
use leptos::html::Div;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, Home};
use crate::devices::Controls;

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
        pointer.set(None);
        tracing.set(Vec::new());
        arming.set(None);
        arming_area.set(None);
        drag.set(None);
    });

    // Everything below draws and edits one floor at a time.
    let level = Memo::new(move |_| on_floor(&shown.get(), floor.get().as_ref()));
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
                on_level(draft, floor, |level| {
                    if let Some(placed) = level.areas.get_mut(area) {
                        placed.label = Point::new(
                            placed.label.x.saturating_add(by.0),
                            placed.label.y.saturating_add(by.1),
                        );
                    }
                });
                // The grab moves with the label, so the rounding can't accumulate into a drift.
                drag.set(Some(Drag::AreaLabel {
                    area,
                    grab: (grab.0 + f64::from(by.0), grab.1 + f64::from(by.1)),
                }));
            }
        }
    };

    let on_up = move |_: ev::MouseEvent| drag.set(None);

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
                        wall.openings.push(Opening {
                            kind,
                            at: fit_opening(along, width, length),
                            width,
                        });
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
                        // is what the plan allows, and moving a wall is why somebody would.
                        level.areas.retain(|placed| placed.area != area);
                        level.areas.push(PlacedArea {
                            area: area.clone(),
                            points,
                            label: Point::new(0, 0),
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
                draft.update(|plan| {
                    for (id, level) in &mut plan.floors {
                        if Some(id) != here.as_ref() {
                            level.devices.retain(|placed| placed.device != device);
                        }
                    }
                });
                on_level(draft, floor, |level| {
                    level.devices.retain(|placed| placed.device != device);
                    level.devices.push(PlacedDevice { device, at });
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
                    level.areas.retain(|placed| placed.area != area);
                    level.areas.push(PlacedArea {
                        area: area.clone(),
                        points: corners,
                        label: Point::new(0, 0),
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
                    {move || {
                        let here = view.get();
                        let level = level.get();
                        let chosen = editing.get().then(|| picked.get()).flatten();
                        level
                            .walls
                            .iter()
                            .enumerate()
                            .map(|(w, wall)| {
                                drawn_wall(wall, w, joints(&level, w), here, chosen)
                            })
                            .collect_view()
                    }}
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
                                let at = label_at(placed)?;
                                let (x, y) = here.screen(at);
                                let grab = (f64::from(at.x), f64::from(at.y));
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

            {move || editing.get().then(|| view! {
                <div class="toolbar" role="toolbar" aria-label="Drawing tools">
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
                        "The devices on the plan show what they're doing. Click one to switch it."
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
    let next_level = move || {
        floors
            .get()
            .iter()
            .map(|floor| floor.level)
            .max()
            .map_or(0, |highest| highest.saturating_add(1))
    };
    let open = move || {
        name.set(String::new());
        above.set(next_level().to_string());
        adding.set(true);
    };

    let add = move || {
        let Ok(named) = name.get_untracked().trim().parse::<irori_types::Name>() else {
            trouble.set(Some("A floor needs a name.".into()));
            return;
        };
        let Ok(level) = above.get_untracked().trim().parse::<i8>() else {
            trouble.set(Some("A floor's level is a whole number: 0, 1, -1.".into()));
            return;
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
        Some(view! {
            <div class="floor-picker" role="group" aria-label="Floors">
                {floors
                    .into_iter()
                    .rev()
                    .map(|level| {
                        let id = level.id.clone();
                        let (chosen, pressed) = (id.clone(), id.clone());
                        let drawn = plan.level(&level.id).is_some_and(|level| !level.is_empty());
                        view! {
                            <button
                                type="button"
                                class:chosen=move || floor.get().as_ref() == Some(&chosen)
                                aria-pressed=move || {
                                    (floor.get().as_ref() == Some(&pressed)).to_string()
                                }
                                on:click=move |_| floor.set(Some(id.clone()))
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
    view! {
        <div class="snap">
            <span class="snap-label" id="snap-label">"Snap"</span>
            <div class="switcher" role="group" aria-labelledby="snap-label">
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
                // An opening can't be wider than the wall it's cut into, so the slider stops
                // where the wall does rather than letting a plan be made that can't be saved.
                let widest = wall.length().floor().max(1.0) as u32;
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
                Some(
                    view! {
                        <div class="inspector">
                            <h2>"Device"</h2>
                            <p class="name">{name}</p>
                            <p class="muted small">{placed.device.to_string()}</p>
                            <p class="muted small">"Drag it to move it."</p>
                        </div>
                    }
                    .into_any(),
                )
            }
        }
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

/// One device where it was put: what it's called, and what it's doing.
///
/// HTML rather than something drawn in the SVG, because a marker is a control — it has a label,
/// it can be focused, and in reading mode clicking it switches the device. It is also the one
/// thing on the canvas that must *not* scale with the zoom: a name is either readable or it
/// isn't.
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
    remember: Callback<()>,
) -> impl IntoView + use<> {
    let (x, y) = view.screen(placed.at);
    let look = looks(home, &device.id);
    let name = device.name.to_string();
    let (on, offline) = (look.on, look.offline);
    // A marker is a control only while reading the plan: a device that can't be switched, or
    // that isn't answering, must not look like one that can be. In edit mode it is a thing to
    // move instead, so it stays live whatever the device is doing.
    let switchable = look.switch.is_some() && !offline;
    let title = match (offline, on) {
        (true, _) => format!("{name} — not answering"),
        (false, Some(true)) => format!("{name} — on"),
        (false, Some(false)) => format!("{name} — off"),
        (false, None) => name.clone(),
    };
    let switch = look.switch.clone();

    view! {
        <button
            type="button"
            class="marker"
            class:on=move || on == Some(true)
            class:offline=offline
            class:chosen=chosen
            class:movable=editing
            class:switchable=!editing && switchable
            style=format!("left:{x}px;top:{y}px")
            title=title
            disabled=!editing && !switchable
            aria-pressed=(!editing && switchable).then(|| on.map(|on| on.to_string())).flatten()
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
                if editing || !switchable {
                    return;
                }
                let Some(entity) = switch.clone() else { return };
                // An entity that hasn't said yet is turned on, which is what asking for
                // "the other one" means when there is no current one.
                controls.set_on.run((entity, !on.unwrap_or(false)));
            }
        >
            <span class="pip"></span>
            <span class="marker-name">{name}</span>
        </button>
    }
}

/// What a device looks like on the plan right now.
struct Look {
    /// Whether the entity a click would switch is on. `None` when it can be switched but hasn't
    /// said yet — a device that has only just joined — which is still worth a click.
    on: Option<bool>,
    /// Whether *that* entity is unreachable. Not whether anything on the device is: a lamp with
    /// a flaky signal sensor is still a lamp you can switch.
    offline: bool,
    /// The entity a click switches, if there is one.
    switch: Option<EntityId>,
}

/// A device's state, as the plan shows it: the first light or switch it provides speaks for it.
///
/// Chosen by what the entity **can do**, not by what it has said. An entity that has reported
/// nothing yet still has a light's or a switch's capabilities, and a marker that refused to
/// switch it until it had spoken would be a dead control on a device that works — the Devices
/// page turns such an entity on from an unknown state, and so does this.
fn looks(home: &Home, device: &DeviceId) -> Look {
    let found = home
        .entities
        .iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
        .find(|entity| {
            matches!(
                entity.capabilities,
                Capabilities::Light(_) | Capabilities::Switch(_)
            )
        });
    let Some(entity) = found else {
        return Look {
            on: None,
            offline: false,
            switch: None,
        };
    };
    let state = home
        .states
        .iter()
        .find(|state| state.entity_id == entity.id);
    Look {
        on: match state.and_then(|state| state.state.as_ref()) {
            Some(State::Light(light)) => Some(light.on),
            Some(State::Switch(switch)) => Some(switch.on),
            _ => None,
        },
        offline: state
            .is_some_and(|state| state.availability == irori_types::Availability::Unavailable),
        switch: Some(entity.id.clone()),
    }
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
/// `joints` is how far each end runs past the point it was drawn to, so that a corner comes out
/// solid; see [`joints`].
fn drawn_wall(
    wall: &Wall,
    index: usize,
    joints: (f64, f64),
    view: Viewport,
    picked: Option<Pick>,
) -> impl IntoView + use<> {
    let thickness = f64::from(wall.thickness);
    let chosen = picked == Some(Pick::Wall(index));
    let length = wall.length();
    let runs = solid_runs(wall)
        .into_iter()
        .map(|(start, end)| {
            // Only the ends that really are the ends of the wall get the joint: the sides of a
            // doorway are the ends of a run too, and they must stay where the door is.
            let start = if start <= 0.0 { -joints.0 } else { start };
            let end = if end >= length {
                length + joints.1
            } else {
                end
            };
            let (a, _, _) = along(wall, start);
            let (b, _, _) = along(wall, end);
            view! {
                <line
                    class="wall"
                    class:chosen=chosen
                    x1=a.0 y1=a.1 x2=b.0 y2=b.1
                    stroke-width=thickness
                />
            }
        })
        .collect_view();
    let openings = wall
        .openings
        .iter()
        .enumerate()
        .map(|(o, opening)| drawn_opening(wall, opening, picked == Some(Pick::Opening(index, o))))
        .collect_view();

    view! {
        <g class="wall-group" transform=transform(view)>
            {runs}
            {openings}
        </g>
    }
}

/// A door or a window in the gap its wall left for it.
///
/// A door is drawn the way plans have always drawn one: the leaf standing open, and the arc it
/// sweeps. A window is the pane across the gap. Both get jambs, so the hole reads as a hole
/// rather than as a wall somebody forgot to finish.
fn drawn_opening(wall: &Wall, opening: &Opening, chosen: bool) -> impl IntoView + use<> {
    let half = f64::from(opening.width) / 2.0;
    let at = f64::from(opening.at);
    let thickness = f64::from(wall.thickness);
    let (near, unit, normal) = along(wall, at - half);
    let (far, _, _) = along(wall, at + half);
    let width = f64::from(opening.width);

    let jamb = |(x, y): (f64, f64)| {
        let (dx, dy) = (normal.0 * thickness / 2.0, normal.1 * thickness / 2.0);
        view! {
            <line
                class="jamb"
                x1=x - dx y1=y - dy x2=x + dx y2=y + dy
                stroke-width=thickness / 4.0
            />
        }
    };

    let inner = match opening.kind {
        OpeningKind::Door => {
            let tip = (near.0 + normal.0 * width, near.1 + normal.1 * width);
            let leaf = format!(
                "M {} {} L {} {} A {width} {width} 0 0 0 {} {}",
                near.0, near.1, tip.0, tip.1, far.0, far.1
            );
            view! {
                <path class="door" class:chosen=chosen d=leaf fill="none"
                    stroke-width=thickness / 4.0 />
            }
            .into_any()
        }
        OpeningKind::Window => view! {
            <line
                class="window" class:chosen=chosen
                x1=near.0 y1=near.1 x2=far.0 y2=far.1
                stroke-width=thickness / 3.0
            />
        }
        .into_any(),
    };
    // Silences the unused warning on `unit`, which only the jambs' direction needs.
    let _ = unit;

    view! {
        <>
            {jamb(near)}
            {jamb(far)}
            {inner}
        </>
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

/// How far each end of a wall has to run past the point it was drawn to for its corners to come
/// out solid, in centimetres: `(from, to)`.
///
/// Walls are drawn as thick lines with flat ends, so two of them meeting at a right angle stop
/// on the corner point and leave a square notch missing from the outside of it — small, but the
/// one thing that makes a plan look unfinished. Running each wall on by the **mitre distance**
/// fills it: half the thickness divided by the tangent of half the angle between them, which is
/// exactly half the thickness at a right angle, nothing at all where two walls carry straight
/// on, and more as the corner closes up.
///
/// A free end — one no other wall meets — gets nothing, because a wall that stops in the middle
/// of a room stops where it was drawn. Where several walls meet, the largest of the distances
/// wins: the overshoot lands inside the other walls, where it can't be seen.
fn joints(level: &Level, index: usize) -> (f64, f64) {
    let Some(wall) = level.walls.get(index) else {
        return (0.0, 0.0);
    };
    let half = f64::from(wall.thickness) / 2.0;
    // Past about 15° the mitre runs away to nothing useful, so it's cut off — a sliver at a very
    // sharp corner beats a spike shooting across the plan.
    let limit = half * 4.0;

    let reach = |corner: Point, away: Point| {
        // The wall's own direction, pointing away from this corner.
        let mine = direction(corner, away);
        let mut most = 0.0_f64;
        for (other, from, to) in level
            .walls
            .iter()
            .enumerate()
            .filter(|(which, _)| *which != index)
            .map(|(_, other)| (other, other.from, other.to))
        {
            let _ = other;
            let theirs = if from == corner {
                direction(corner, to)
            } else if to == corner {
                direction(corner, from)
            } else {
                continue;
            };
            // The angle at the corner, between the two walls running away from it.
            let cos = (mine.0 * theirs.0 + mine.1 * theirs.1).clamp(-1.0, 1.0);
            let angle = cos.acos();
            let tan = (angle / 2.0).tan();
            let distance = if tan.abs() < 1e-6 { limit } else { half / tan };
            most = most.max(distance.clamp(0.0, limit));
        }
        most
    };

    (reach(wall.from, wall.to), reach(wall.to, wall.from))
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
    let length = wall.length();
    let mut holes: Vec<(f64, f64)> = wall
        .openings
        .iter()
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
    document()
        .active_element()
        .is_some_and(|element| matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA"))
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
        wall.openings.push(Opening {
            kind: OpeningKind::Door,
            at: 100,
            width: 80,
        });
        wall.openings.push(Opening {
            kind: OpeningKind::Window,
            at: 300,
            width: 100,
        });
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
        wall.openings.push(Opening {
            kind: OpeningKind::Window,
            at: 220,
            width: 100,
        });
        wall.openings.push(Opening {
            kind: OpeningKind::Door,
            at: 200,
            width: 80,
        });
        assert_eq!(solid_runs(&wall), vec![(0.0, 160.0), (270.0, 400.0)]);
    }

    /// A wall with a door across its whole length has no wall left, not a run of negative length.
    #[test]
    fn a_wall_that_is_all_door_has_nothing_solid_in_it() {
        let mut wall = wall((0, 0), (100, 0));
        wall.openings.push(Opening {
            kind: OpeningKind::Door,
            at: 50,
            width: 200,
        });
        assert_eq!(solid_runs(&wall), Vec::<(f64, f64)>::new());
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
        short.openings.push(Opening {
            kind: OpeningKind::Door,
            at: 200,
            width: 80,
        });
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

    /// The gap this closes: two walls meeting at a right angle stop on the corner and leave a
    /// square notch out of the outside of it. Each has to run on by half its thickness.
    #[test]
    fn a_corner_runs_on_far_enough_to_come_out_solid() {
        let plan = Level {
            walls: vec![
                wall((0, 0), (400, 0)),
                wall((400, 0), (400, 300)),
                wall((600, 600), (900, 600)),
            ],
            ..Level::default()
        };
        let half = f64::from(Wall::DEFAULT_THICKNESS) / 2.0;

        let (from, to) = joints(&plan, 0);
        assert_eq!(from, 0.0, "a free end stops where it was drawn");
        assert!(
            (to - half).abs() < 1e-9,
            "a right angle runs on by half: {to}"
        );
        let (from, _) = joints(&plan, 1);
        assert!(
            (from - half).abs() < 1e-9,
            "and so does the other wall: {from}"
        );
        assert_eq!(
            joints(&plan, 2),
            (0.0, 0.0),
            "a wall on its own gets nothing"
        );
    }

    /// Two walls carrying straight on need nothing; a hairpin needs more than a right angle,
    /// but not without limit.
    #[test]
    fn how_far_a_corner_runs_on_follows_the_angle() {
        let straight = Level {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (800, 0))],
            ..Level::default()
        };
        assert!(joints(&straight, 0).1 < 1e-6, "nothing to fill");

        let sharp = Level {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (0, 40))],
            ..Level::default()
        };
        let half = f64::from(Wall::DEFAULT_THICKNESS) / 2.0;
        let run = joints(&sharp, 0).1;
        assert!(run > half, "a sharper corner needs more: {run}");
        assert!(run <= half * 4.0, "but the spike is cut off: {run}");
    }

    #[test]
    fn what_is_under_the_pointer_is_the_opening_before_the_wall() {
        let mut front = wall((0, 0), (400, 0));
        front.openings.push(Opening {
            kind: OpeningKind::Door,
            at: 200,
            width: 80,
        });
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
        assert_eq!(label_at(&kitchen), Some(Point::new(200, 150)), "in the middle");
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
            devices: vec![PlacedDevice {
                device: "demo_lamp".parse().expect("a valid device id"),
                at: Point::new(120, 420),
            }],
            ..Level::default()
        };
        assert_eq!(
            extent(&plan),
            Some((Point::new(-50, 0), Point::new(400, 420)))
        );
        assert_eq!(extent(&Level::default()), None);
    }
}
