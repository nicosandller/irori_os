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
    Device, DeviceId, EntityId, Floorplan, Opening, OpeningKind, PlacedDevice, Point, State, Wall,
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
    let snap = RwSignal::new(Snap::Grid);
    // What the next wall drawn will be. Changing a wall's thickness sets this too, so drawing
    // an outside wall, thickening it, and carrying on gives thick walls the rest of the way.
    let thickness = RwSignal::new(Wall::DEFAULT_THICKNESS);
    let view = RwSignal::new(Viewport::default());
    let drag = RwSignal::new(None::<Drag>);
    let dragged = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    let canvas = NodeRef::<Div>::new();

    // What's on screen: the copy while it's being drawn, the home's own plan otherwise.
    let shown = Memo::new(move |_| {
        if editing.get() {
            draft.get()
        } else {
            live.home.get().floorplan.clone()
        }
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
        let plan = shown.get_untracked();
        let Some((low, high)) = extent(&plan) else {
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
        let margin = 64.0;
        let (span_x, span_y) = (
            f64::from(high.x - low.x).max(100.0),
            f64::from(high.y - low.y).max(100.0),
        );
        let scale = ((width - margin * 2.0) / span_x)
            .min((height - margin * 2.0) / span_y)
            .clamp(MIN_SCALE, MAX_SCALE);
        view.set(Viewport {
            scale,
            pan: (
                width / 2.0 - f64::from(low.x + high.x) / 2.0 * scale,
                height / 2.0 - f64::from(low.y + high.y) / 2.0 * scale,
            ),
        });
    };

    // A home that was already drawn should open framed rather than at whatever the default zoom
    // happens to show. Once only, and never while somebody is drawing: a view that reframed
    // itself as the first wall appeared would move the plan out from under the pointer, and
    // every click after it would land somewhere else than it looked.
    let framed = RwSignal::new(false);
    Effect::new(move |_| {
        let plan = live.home.get().floorplan;
        if framed.get_untracked()
            || editing.get_untracked()
            || plan.is_empty()
            || canvas.get().is_none()
        {
            return;
        }
        framed.set(true);
        fit();
    });

    let stop_drawing = move || {
        running.set(None);
        pointer.set(None);
    };

    let start_editing = move || {
        draft.set(live.home.get_untracked().floorplan.clone());
        picked.set(None);
        arming.set(None);
        tool.set(Tool::Select);
        stop_drawing();
        trouble.set(None);
        editing.set(true);
    };

    let cancel = move || {
        editing.set(false);
        picked.set(None);
        arming.set(None);
        stop_drawing();
        trouble.set(None);
    };

    let save = move || {
        let plan = draft.get_untracked();
        saving.set(true);
        spawn_local(async move {
            match api::save_floorplan(&plan).await {
                Ok(()) => {
                    trouble.set(None);
                    editing.set(false);
                    picked.set(None);
                    arming.set(None);
                    running.set(None);
                    pointer.set(None);
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
        draft.update(|plan| match pick {
            Pick::Wall(w) => {
                if w < plan.walls.len() {
                    plan.walls.remove(w);
                }
            }
            Pick::Opening(w, o) => {
                if let Some(wall) = plan.walls.get_mut(w)
                    && o < wall.openings.len()
                {
                    wall.openings.remove(o);
                }
            }
            Pick::Device(d) => {
                if d < plan.devices.len() {
                    plan.devices.remove(d);
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
                if running.get_untracked().is_some() {
                    stop_drawing();
                } else if arming.get_untracked().is_some() {
                    arming.set(None);
                } else {
                    picked.set(None);
                }
            }
            "Delete" | "Backspace" if picked.get_untracked().is_some() => {
                event.prevent_default();
                remove_picked();
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
        let plan = draft.get_untracked();
        let reach = REACH * here.cm_per_pixel();

        // A corner of the wall already picked up comes first: its handles are drawn on top, and
        // they're the smallest thing on the canvas to aim at.
        if let Some(Pick::Wall(w)) = picked.get_untracked()
            && let Some(wall) = plan.walls.get(w)
        {
            for (to_end, end) in [(false, wall.from), (true, wall.to)] {
                let away = (world.0 - f64::from(end.x)).hypot(world.1 - f64::from(end.y));
                if away <= reach * 1.5 {
                    drag.set(Some(Drag::Corner { wall: w, to_end }));
                    return;
                }
            }
        }

        match pick_at(&plan, world, reach) {
            Some(Pick::Opening(w, o)) => {
                picked.set(Some(Pick::Opening(w, o)));
                drag.set(Some(Drag::Opening {
                    wall: w,
                    opening: o,
                }));
            }
            Some(Pick::Wall(w)) => {
                picked.set(Some(Pick::Wall(w)));
                drag.set(Some(Drag::Wall {
                    wall: w,
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

        if editing.get_untracked() && running.get_untracked().is_some() {
            let to = place(
                &draft.get_untracked(),
                world,
                here,
                snap.get_untracked(),
                None,
            );
            pointer.set(Some(to));
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
                let plan = draft.get_untracked();
                let Some(was) = plan.walls.get(wall).map(|wall| ends(wall, to_end)) else {
                    return;
                };
                let to = place(&plan, world, here, snap.get_untracked(), Some(was));
                draft.update(|plan| shift(plan, &[(was, to)]));
            }
            Drag::Wall { wall, grab } => {
                dragged.set(true);
                let step = snap.get_untracked().step();
                let by = (round(world.0 - grab.0, step), round(world.1 - grab.1, step));
                if by == (0, 0) {
                    return;
                }
                let moved = |point: Point| Point::new(point.x + by.0, point.y + by.1);
                draft.update(|plan| {
                    let Some(wall) = plan.walls.get(wall) else {
                        return;
                    };
                    let (from, to) = (wall.from, wall.to);
                    shift(plan, &[(from, moved(from)), (to, moved(to))]);
                });
                // The grab moves with the wall, so the rounding can't accumulate into a drift.
                drag.set(Some(Drag::Wall {
                    wall,
                    grab: (grab.0 + f64::from(by.0), grab.1 + f64::from(by.1)),
                }));
            }
            Drag::Opening { wall, opening } => {
                dragged.set(true);
                draft.update(|plan| {
                    if let Some(wall) = plan.walls.get_mut(wall) {
                        let length = wall.length();
                        if let Some(hole) = wall.openings.get_mut(opening) {
                            let (along, _) = on_wall_at(wall.from, wall.to, world);
                            hole.at = fit_opening(along, hole.width, length);
                        }
                    }
                });
            }
            Drag::Device { device } => {
                dragged.set(true);
                let step = snap.get_untracked().step();
                let to = Point::new(round(world.0, step), round(world.1, step));
                draft.update(|plan| {
                    if let Some(placed) = plan.devices.get_mut(device) {
                        placed.at = to;
                    }
                });
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
                    &draft.get_untracked(),
                    world,
                    here,
                    snap.get_untracked(),
                    None,
                );
                match running.get_untracked() {
                    Some(from) if from != to => {
                        let built = Wall {
                            thickness: thickness.get_untracked(),
                            ..Wall::new(from, to)
                        };
                        draft.update(|plan| plan.walls.push(built));
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
                let plan = draft.get_untracked();
                let Some(w) = nearest_wall(&plan, world, reach) else {
                    trouble.set(Some(format!(
                        "A {} goes in a wall. Click on one.",
                        kind.label().to_lowercase()
                    )));
                    return;
                };
                trouble.set(None);
                draft.update(|plan| {
                    if let Some(wall) = plan.walls.get_mut(w) {
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
            Tool::Device => {
                let Some(device) = arming.get_untracked() else {
                    trouble.set(Some(
                        "Choose a device first, then click where it is.".into(),
                    ));
                    return;
                };
                trouble.set(None);
                let step = snap.get_untracked().step();
                let at = Point::new(round(world.0, step), round(world.1, step));
                draft.update(|plan| {
                    plan.devices.retain(|placed| placed.device != device);
                    plan.devices.push(PlacedDevice { device, at });
                    picked.set(Some(Pick::Device(plan.devices.len() - 1)));
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
        if running.get_untracked().is_some() {
            stop_drawing();
        } else if arming.get_untracked().is_some() {
            arming.set(None);
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
                    // Not while editing: its caption says to press Edit, and once somebody
                    // has, the canvas should be the empty surface they're drawing on.
                    {move || (!editing.get() && shown.get().is_empty()).then(watermark)}
                    {move || {
                        let here = view.get();
                        let plan = shown.get();
                        let chosen = editing.get().then(|| picked.get()).flatten();
                        plan.walls
                            .iter()
                            .enumerate()
                            .map(|(w, wall)| {
                                drawn_wall(wall, w, joints(&plan, w), here, chosen)
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
                        let chosen = editing.get().then(|| picked.get()).flatten();
                        let Some(Pick::Wall(w)) = chosen else { return None };
                        let plan = shown.get();
                        let wall = plan.walls.get(w)?;
                        Some(handles(wall, view.get()))
                    }}
                </svg>

                <div class="markers">
                    {move || {
                        let home = live.home.get();
                        let plan = shown.get();
                        let here = view.get();
                        let is_editing = editing.get();
                        let chosen = is_editing.then(|| picked.get()).flatten();
                        plan.devices
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
                                ))
                            })
                            .collect_view()
                    }}
                </div>

                {move || {
                    let (Some(from), Some(to)) = (running.get(), pointer.get()) else {
                        return None;
                    };
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
                                <button type="button" on:click=move |_| cancel()>"Cancel"</button>
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
                    } else {
                        view! {
                            <button type="button" on:click=move |_| start_editing()>"Edit"</button>
                        }.into_any()
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
                                    running.set(None);
                                    pointer.set(None);
                                    if which != Tool::Device { arming.set(None); }
                                }
                            >
                                <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=*icon></svg>
                            </button>
                        }
                    }).collect_view()}
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

            {move || editing.get().then(|| view! {
                <Inspector draft=draft picked=picked thickness=thickness />
            })}

            <div class="plan-foot">
                <div class="zoom">
                    <button type="button" aria-label="Zoom out" on:click=move |_| zoom_by(1.0 / 1.25)>"−"</button>
                    <button type="button" aria-label="Zoom in" on:click=move |_| zoom_by(1.25)>"+"</button>
                    <button type="button" on:click=move |_| fit()>"Fit"</button>
                </div>
                {move || editing.get().then(|| view! { <SnapControl snap=snap /> })}
                <p class="hint">
                    {move || if editing.get() {
                        tool.get().hint()
                    } else if shown.get().is_empty() {
                        ""
                    } else {
                        "The devices on the plan show what they're doing. Click one to switch it."
                    }}
                </p>
            </div>

            {move || trouble.get().map(|why| view! { <p class="plan-banner">{why}</p> })}
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
    picked: RwSignal<Option<Pick>>,
    thickness: RwSignal<u32>,
) -> impl IntoView {
    move || {
        let plan = draft.get();
        match picked.get()? {
            Pick::Wall(w) => {
                let wall = plan.walls.get(w)?;
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
                                    draft.update(|plan| {
                                        if let Some(wall) = plan.walls.get_mut(w) {
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
                let wall = plan.walls.get(w)?;
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
                                on:input=move |event| {
                                    let Ok(next) = event_target_value(&event).parse::<u32>() else {
                                        return;
                                    };
                                    draft.update(|plan| {
                                        let Some(wall) = plan.walls.get_mut(w) else { return };
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
            Pick::Device(d) => {
                let placed = plan.devices.get(d)?;
                Some(
                    view! {
                        <div class="inspector">
                            <h2>"Device"</h2>
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

/// A small flat, faintly, where the plan will go: what this page is for, said in the shape of
/// the thing rather than in a sentence at the bottom of the screen.
///
/// Drawn the way the editor would draw it — walls with holes in them, a door with its swing, the
/// devices as pips — so it is a promise the page keeps rather than a decoration. In the canvas's
/// own pixels rather than in centimetres, so it sits in the middle and stays the same size
/// whatever the zoom is: it is a picture of a floorplan, not one, and zooming in on it would be
/// a promise the page can't keep.
fn watermark() -> impl IntoView {
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
    view! {
        <g class="watermark">
            <svg x="50%" y="50%" overflow="visible">
                <g transform="translate(-260 -205)">
                    <path class="wm-wall" d=walls fill="none" stroke-width="11"
                        stroke-linecap="square" />

                    // The windows: a pane across each gap, with a jamb at either end.
                    <path class="wm-glass" d="M70 0 H150 M240 0 H320 M0 150 V240" />
                    <path class="wm-jamb" d="M70 -6 V6 M150 -6 V6 M240 -6 V6 M320 -6 V6
                                             M-6 150 H6 M-6 240 H6" />

                    // The front door, standing open into the living room, and the two inside
                    // doors — the way every plan since the drawing board has shown one.
                    <path class="wm-swing" fill="none"
                        d="M220 360 V280 M220 280 A80 80 0 0 0 300 360" />
                    <path class="wm-swing" fill="none"
                        d="M320 110 H400 M400 110 A80 80 0 0 0 320 190" />
                    <path class="wm-swing" fill="none"
                        d="M470 200 V270 M470 270 A70 70 0 0 0 400 200" />

                    // Enough furniture to read as somebody's home: a bed under the windows, a
                    // sofa and a table in the living room, a bath and a basin.
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
                </g>
            </svg>
            <text x="50%" y="50%" dy="195" text-anchor="middle">"Nothing drawn yet"</text>
            <text x="50%" y="50%" dy="221" text-anchor="middle" class="watermark-lede">
                "Press Edit to draw the walls of your home"
            </text>
        </g>
    }
}

/// The devices of the home, to pick one to place. Already-placed devices stay in the list —
/// clicking one again moves it rather than adding a second of the same thing.
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
                            let on_plan = placed
                                .devices
                                .iter()
                                .any(|entry| entry.device == device.id);
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
) -> impl IntoView + use<> {
    let (x, y) = view.screen(placed.at);
    let look = looks(home, &device.id);
    let name = device.name.to_string();
    let title = match look.on {
        Some(true) => format!("{name} — on"),
        Some(false) => format!("{name} — off"),
        None => name.clone(),
    };
    let switch = look.switch.clone();
    let on = look.on;

    view! {
        <button
            type="button"
            class="marker"
            class:on=move || on == Some(true)
            class:offline=move || look.offline
            class:chosen=chosen
            class:movable=editing
            style=format!("left:{x}px;top:{y}px")
            title=title
            aria-pressed=(!editing).then(|| on.map(|on| on.to_string())).flatten()
            on:mousedown=move |event: ev::MouseEvent| {
                if !editing {
                    return;
                }
                event.stop_propagation();
                dragged.set(false);
                picked.set(Some(Pick::Device(index)));
                drag.set(Some(Drag::Device { device: index }));
            }
            on:click=move |event: ev::MouseEvent| {
                event.stop_propagation();
                if editing {
                    return;
                }
                let (Some(entity), Some(on)) = (switch.clone(), on) else {
                    return;
                };
                controls.set_on.run((entity, !on));
            }
        >
            <span class="pip"></span>
            <span class="marker-name">{name}</span>
        </button>
    }
}

/// What a device looks like on the plan right now.
struct Look {
    /// Whether anything it provides is switched on, if anything it provides can be.
    on: Option<bool>,
    offline: bool,
    /// The entity a click switches, if there is one.
    switch: Option<EntityId>,
}

/// A device's state, as the plan shows it: the first light or switch it provides speaks for it.
///
/// A device is usually one thing — a lamp, a plug — and when it isn't (a lamp with a sensor in
/// it) the switchable part is what someone reaching for a floorplan wants to press.
fn looks(home: &Home, device: &DeviceId) -> Look {
    let mut offline = false;
    let mut found: Option<(EntityId, Option<bool>)> = None;
    for entity in home
        .entities
        .iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
    {
        let state = home
            .states
            .iter()
            .find(|state| state.entity_id == entity.id);
        if state.is_some_and(|state| state.availability == irori_types::Availability::Unavailable) {
            offline = true;
        }
        let on = match state.and_then(|state| state.state.as_ref()) {
            Some(State::Light(light)) => Some(light.on),
            Some(State::Switch(switch)) => Some(switch.on),
            _ => continue,
        };
        if found.is_none() {
            found = Some((entity.id.clone(), on));
        }
    }
    match found {
        Some((entity, on)) => Look {
            on,
            offline,
            switch: Some(entity),
        },
        None => Look {
            on: None,
            offline,
            switch: None,
        },
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

/// The two ends of the wall that's picked up, to drag. Sized in screen pixels, which at this
/// zoom means dividing by the scale.
fn handles(wall: &Wall, view: Viewport) -> impl IntoView + use<> {
    let radius = 6.0 / view.scale;
    view! {
        <g class="handles" transform=transform(view)>
            <circle cx=wall.from.x cy=wall.from.y r=radius vector-effect="non-scaling-stroke" />
            <circle cx=wall.to.x cy=wall.to.y r=radius vector-effect="non-scaling-stroke" />
        </g>
    }
}

// --- Geometry -----------------------------------------------------------------------------

/// Rounds a measurement to the nearest multiple of `step` centimetres.
fn round(value: f64, step: i32) -> i32 {
    (value / f64::from(step)).round() as i32 * step
}

/// Where a new point goes: onto a corner that's already there if one is within reach, and onto
/// the grid otherwise.
///
/// `except` leaves one corner out — the one being dragged. Without it a corner could never be
/// moved off the grid square it started on, because it would keep snapping to itself.
fn place(
    plan: &Floorplan,
    world: (f64, f64),
    view: Viewport,
    snap: Snap,
    except: Option<Point>,
) -> Point {
    let reach = CORNER * view.cm_per_pixel();
    let mut nearest: Option<(f64, Point)> = None;
    for corner in plan.walls.iter().flat_map(|wall| [wall.from, wall.to]) {
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
fn shift(plan: &mut Floorplan, moves: &[(Point, Point)]) {
    let mut next = plan.clone();
    for wall in &mut next.walls {
        for end in [&mut wall.from, &mut wall.to] {
            if let Some((_, to)) = moves.iter().find(|(was, _)| was == end) {
                *end = *to;
            }
        }
    }
    if next.walls.iter().any(|wall| wall.from == wall.to) {
        return;
    }
    // A wall dragged shorter can leave its own door hanging off the end of it.
    for wall in &mut next.walls {
        trim(wall);
    }
    *plan = next;
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
fn joints(plan: &Floorplan, index: usize) -> (f64, f64) {
    let Some(wall) = plan.walls.get(index) else {
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
        for (other, from, to) in plan
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
    let (dx, dy) = (f64::from(to.x - from.x), f64::from(to.y - from.y));
    let length = dx.hypot(dy);
    if length <= f64::EPSILON {
        return (1.0, 0.0);
    }
    (dx / length, dy / length)
}

/// A point a given distance along a wall, with the wall's direction and its left-hand normal.
fn along(wall: &Wall, at: f64) -> ((f64, f64), (f64, f64), (f64, f64)) {
    let (ax, ay) = (f64::from(wall.from.x), f64::from(wall.from.y));
    let (dx, dy) = (f64::from(wall.to.x) - ax, f64::from(wall.to.y) - ay);
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
fn nearest_wall(plan: &Floorplan, world: (f64, f64), reach: f64) -> Option<usize> {
    plan.walls
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
fn pick_at(plan: &Floorplan, world: (f64, f64), reach: f64) -> Option<Pick> {
    let mut best: Option<(f64, Pick)> = None;
    for (index, wall) in plan.walls.iter().enumerate() {
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
    best.map(|(_, found)| found)
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

/// The furthest corners of everything drawn, for framing it.
fn extent(plan: &Floorplan) -> Option<(Point, Point)> {
    let points = plan
        .walls
        .iter()
        .flat_map(|wall| [wall.from, wall.to])
        .chain(plan.devices.iter().map(|placed| placed.at));
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
const TOOLS: [(Tool, &str, &str); 5] = [
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
        Tool::Device,
        "Device",
        r#"<rect x="6" y="6" width="12" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="12" cy="12" r="2.4" fill="currentColor"/>"#,
    ),
];

/// Take away what's picked up.
const BIN: &str = r#"<path d="M4 7h16M10 7V5h4v2M6 7l1 13h10l1-13" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(from: (i32, i32), to: (i32, i32)) -> Wall {
        Wall::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
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
        let plan = Floorplan {
            walls: vec![short],
            ..Floorplan::default()
        };
        assert!(plan.check().is_ok(), "{plan:?}");
    }

    /// Corners are what rooms are made of: a new point within reach of one lands exactly on it,
    /// rather than on the grid square next to it.
    #[test]
    fn a_new_point_lands_on_a_corner_that_is_already_there() {
        let view = Viewport {
            scale: 0.5,
            pan: (0.0, 0.0),
        };
        let plan = Floorplan {
            walls: vec![wall((0, 0), (403, 0))],
            ..Floorplan::default()
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
        let mut plan = Floorplan {
            walls: vec![
                wall((0, 0), (600, 0)),
                wall((600, 0), (600, 450)),
                wall((600, 450), (0, 450)),
                wall((0, 450), (0, 0)),
            ],
            ..Floorplan::default()
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
        assert!(plan.check().is_ok());
    }

    /// Moving a wall stretches the walls joined to it rather than tearing the room open.
    #[test]
    fn dragging_a_whole_wall_takes_its_neighbours_with_it() {
        let mut plan = Floorplan {
            walls: vec![
                wall((0, 0), (600, 0)),
                wall((600, 0), (600, 450)),
                wall((0, 450), (0, 0)),
            ],
            ..Floorplan::default()
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
        let before = Floorplan {
            walls: vec![wall((0, 0), (100, 0))],
            ..Floorplan::default()
        };
        let mut plan = before.clone();
        shift(&mut plan, &[(Point::new(0, 0), Point::new(100, 0))]);
        assert_eq!(plan, before);
    }

    /// The point of a step of your own: a wall that really is 137 cm long.
    #[test]
    fn a_custom_step_is_what_points_round_to() {
        let view = Viewport {
            scale: 0.5,
            pan: (0.0, 0.0),
        };
        let empty = Floorplan::default();
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
        let plan = Floorplan {
            walls: vec![
                wall((0, 0), (400, 0)),
                wall((400, 0), (400, 300)),
                wall((600, 600), (900, 600)),
            ],
            ..Floorplan::default()
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
        let straight = Floorplan {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (800, 0))],
            ..Floorplan::default()
        };
        assert!(joints(&straight, 0).1 < 1e-6, "nothing to fill");

        let sharp = Floorplan {
            walls: vec![wall((0, 0), (400, 0)), wall((400, 0), (0, 40))],
            ..Floorplan::default()
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
        let plan = Floorplan {
            walls: vec![front, wall((0, 300), (400, 300))],
            ..Floorplan::default()
        };
        assert_eq!(
            pick_at(&plan, (200.0, 2.0), 20.0),
            Some(Pick::Opening(0, 0))
        );
        assert_eq!(pick_at(&plan, (50.0, 2.0), 20.0), Some(Pick::Wall(0)));
        assert_eq!(pick_at(&plan, (50.0, 298.0), 20.0), Some(Pick::Wall(1)));
        assert_eq!(pick_at(&plan, (200.0, 150.0), 20.0), None, "empty room");
    }

    #[test]
    fn lengths_are_said_the_way_a_plan_says_them() {
        assert_eq!(metres(40.0), "40 cm");
        assert_eq!(metres(100.0), "1.00 m");
        assert_eq!(metres(425.0), "4.25 m");
    }

    #[test]
    fn the_whole_plan_can_be_framed() {
        let plan = Floorplan {
            walls: vec![wall((-50, 0), (400, 0)), wall((400, 0), (400, 300))],
            devices: vec![PlacedDevice {
                device: "demo_lamp".parse().expect("a valid device id"),
                at: Point::new(120, 420),
            }],
        };
        assert_eq!(
            extent(&plan),
            Some((Point::new(-50, 0), Point::new(400, 420)))
        );
        assert_eq!(extent(&Floorplan::default()), None);
    }
}
