//! The canvas: nodes you drag, wires you draw from a port to a node, pan and zoom — and, laid
//! over the same picture, a run's path, the runs going now, or what changed since a version.

use std::collections::{BTreeMap, BTreeSet};

use irori_flow_types::trace::Outcome;
use irori_flow_types::{Flow, Node, NodeId, Port, PortRef, Wire};
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

use crate::editor::{Editing, Selected, View};
use crate::{Home, live, model, time};

/// A place for a new node: right of everything, level with the first.
pub fn free_spot(flow: &Flow) -> [f64; 2] {
    let at = model::positions(flow);
    let right = at.values().map(|p| p[0]).fold(f64::MIN, f64::max);
    let top = at.values().map(|p| p[1]).fold(f64::MAX, f64::min);
    if at.is_empty() {
        [60.0, 80.0]
    } else {
        [right + 260.0, top.max(40.0) + 40.0 * (at.len() % 4) as f64]
    }
}

#[derive(Debug, Clone)]
enum Drag {
    Pan {
        from: [f64; 2],
        pan: [f64; 2],
    },
    Node {
        id: NodeId,
        from: [f64; 2],
        at: [f64; 2],
    },
    Wire {
        from: PortRef,
    },
    /// A branch, or a part of one, moved by its grip: every node in it, from where it was.
    Group {
        nodes: Vec<(NodeId, [f64; 2])>,
        from: [f64; 2],
    },
}

/// How a node looked in the run being shown.
#[derive(Debug, Clone, PartialEq)]
struct Visit {
    seq: u32,
    class: &'static str,
}

fn closest(target: &web_sys::EventTarget, selector: &str) -> Option<web_sys::Element> {
    target
        .dyn_ref::<web_sys::Element>()
        .and_then(|element| element.closest(selector).ok().flatten())
}

#[component]
pub fn Canvas() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let pan = RwSignal::new([0.0_f64, 0.0_f64]);
    let zoom = RwSignal::new(1.0_f64);
    let drag = StoredValue::new(None::<Drag>);
    let loose = RwSignal::new(None::<([f64; 2], [f64; 2])>);
    // The node being dragged, so it can lift while it moves.
    let dragging = RwSignal::new(None::<NodeId>);
    let root = NodeRef::<leptos::html::Div>::new();
    // The flow playing out as it runs.
    let show = live::Show::new();
    provide_context(show);
    live::watch(ed, show);

    // What's drawn: the draft, or the definition a run ran.
    let flow = Memo::new(move |_| match ed.view.get() {
        View::Trace { flow, .. } => Some(*flow),
        _ => ed.draft.get(),
    });
    let positions =
        Memo::new(move |_| flow.get().map(|f| model::positions(&f)).unwrap_or_default());

    let world = move |client: [f64; 2]| -> [f64; 2] {
        let rect = root.get_untracked().map(|el| el.get_bounding_client_rect());
        let (left, top) = rect.map_or((0.0, 0.0), |r| (r.left(), r.top()));
        let [px, py] = pan.get_untracked();
        let z = zoom.get_untracked();
        [(client[0] - left - px) / z, (client[1] - top - py) / z]
    };

    // The run being shown: which nodes it reached, how, and which wires it took.
    let visits = Memo::new(move |_| -> (BTreeMap<NodeId, Visit>, BTreeSet<Wire>) {
        let View::Trace { record, upto, .. } = ed.view.get() else {
            return Default::default();
        };
        let mut visits = BTreeMap::new();
        let mut taken = BTreeSet::new();
        for step in record.steps.iter().take(upto + 1) {
            if let Some(via) = &step.via {
                taken.insert(via.clone());
            }
            let failed = step
                .call
                .as_ref()
                .is_some_and(|call| matches!(call.result, Some(Err(_))))
                || matches!(&record.outcome, Some(Outcome::Error { node, .. }) if *node == step.node && step.port.is_none());
            let class = match step.port {
                _ if failed => "visited-error",
                Some(Port::No | Port::Timeout | Port::Else | Port::Error) => "visited-no",
                _ => "visited",
            };
            visits
                .entry(step.node.clone())
                .and_modify(|visit: &mut Visit| visit.class = class)
                .or_insert(Visit {
                    seq: step.seq,
                    class,
                });
        }
        (visits, taken)
    });

    // The version being compared against: what's new or changed in the draft.
    let diff = Memo::new(move |_| -> BTreeMap<NodeId, &'static str> {
        let View::Diff { other, .. } = ed.view.get() else {
            return BTreeMap::new();
        };
        ed.draft
            .get()
            .map(|draft| {
                draft
                    .nodes
                    .iter()
                    .filter_map(|(id, node)| match other.nodes.get(id) {
                        None => Some((id.clone(), "added")),
                        Some(old) if old != node => Some((id.clone(), "changed")),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    });

    let on_down = move |event: web_sys::PointerEvent| {
        if event.button() != 0 {
            return;
        }
        let Some(target) = event.target() else {
            return;
        };
        let client = [f64::from(event.client_x()), f64::from(event.client_y())];
        let editing = matches!(ed.view.get_untracked(), View::Edit | View::Diff { .. });
        // A branch's grip picks up every node in it.
        if let Some(grip) = closest(&target, "[data-group]") {
            let (Some(which), Some(f)) = (grip.get_attribute("data-group"), flow.get_untracked())
            else {
                return;
            };
            if !editing {
                return;
            }
            let members = model::branches(&f).into_iter().find_map(|branch| {
                if which == format!("branch:{}", branch.key) {
                    return Some(branch.nodes);
                }
                branch
                    .parts
                    .into_iter()
                    .find(|part| which == format!("part:{}", part.key))
                    .map(|part| part.nodes)
            });
            let Some(members) = members else { return };
            let at = positions.get_untracked();
            let nodes = members
                .into_iter()
                .filter_map(|id| at.get(&id).map(|p| (id.clone(), *p)))
                .collect();
            drag.set_value(Some(Drag::Group {
                nodes,
                from: client,
            }));
            if let Some(el) = root.get_untracked() {
                let _ = el.set_pointer_capture(event.pointer_id());
            }
            event.prevent_default();
            return;
        }
        // An open node's form is for typing and picking, not for dragging the canvas, and the
        // canvas's own buttons are just buttons.
        if closest(&target, "[data-sheet], button, .banner, .group-label").is_some() {
            return;
        }
        if let Some(port) = closest(&target, "[data-port]") {
            if !editing {
                return;
            }
            let (Some(node), Some(which)) = (
                port.get_attribute("data-node-id"),
                port.get_attribute("data-port"),
            ) else {
                return;
            };
            let (Ok(node), Ok(which)) = (node.parse::<NodeId>(), which.parse::<Port>()) else {
                return;
            };
            let start = flow
                .get_untracked()
                .and_then(|f| {
                    let at = positions.get_untracked().get(&node).copied()?;
                    f.nodes.get(&node).map(|n| model::out_anchor(at, n, which))
                })
                .unwrap_or_else(|| world(client));
            loose.set(Some((start, start)));
            drag.set_value(Some(Drag::Wire {
                from: PortRef { node, port: which },
            }));
        } else if let Some(node) = closest(&target, "[data-node]") {
            // Anywhere on a node picks it up; a press that doesn't move opens it (see `on_up`).
            let Some(id) = node
                .get_attribute("data-node")
                .and_then(|id| id.parse::<NodeId>().ok())
            else {
                return;
            };
            if !editing {
                ed.selected.set(Selected::Node(id));
                return;
            }
            let at = positions
                .get_untracked()
                .get(&id)
                .copied()
                .unwrap_or([0.0, 0.0]);
            drag.set_value(Some(Drag::Node {
                id,
                from: client,
                at,
            }));
        } else if let Some(wire) = closest(&target, "[data-wire]") {
            if let Some(index) = wire.get_attribute("data-wire").and_then(|i| i.parse().ok()) {
                ed.selected.set(Selected::Wire(index));
            }
            return;
        } else {
            // Moving the canvas leaves an open node open; a click on it closes it (`on_up`).
            drag.set_value(Some(Drag::Pan {
                from: client,
                pan: pan.get_untracked(),
            }));
        }
        if let Some(el) = root.get_untracked() {
            let _ = el.set_pointer_capture(event.pointer_id());
        }
        event.prevent_default();
    };

    let on_move = move |event: web_sys::PointerEvent| {
        let client = [f64::from(event.client_x()), f64::from(event.client_y())];
        match drag.get_value() {
            Some(Drag::Pan { from, pan: start }) => {
                pan.set([
                    start[0] + client[0] - from[0],
                    start[1] + client[1] - from[1],
                ]);
            }
            Some(Drag::Node { id, from, at }) => {
                let z = zoom.get_untracked();
                let dx = (client[0] - from[0]) / z;
                let dy = (client[1] - from[1]) / z;
                if dx.abs() + dy.abs() < 3.0 && dragging.get_untracked().is_none() {
                    return;
                }
                if dragging.get_untracked().as_ref() != Some(&id) {
                    dragging.set(Some(id.clone()));
                }
                let snapped = [
                    ((at[0] + dx) / 8.0).round() * 8.0,
                    ((at[1] + dy) / 8.0).round() * 8.0,
                ];
                // Positions aren't part of the definition, so moving never makes a new version.
                ed.draft.update(|draft| {
                    if let Some(flow) = draft {
                        flow.layout.insert(id.clone(), snapped);
                    }
                });
            }
            Some(Drag::Wire { .. }) => {
                if let Some((start, _)) = loose.get_untracked() {
                    loose.set(Some((start, world(client))));
                }
            }
            Some(Drag::Group { nodes, from }) => {
                let z = zoom.get_untracked();
                // Moved together, on the same grid, so the branch keeps its shape.
                let dx = ((client[0] - from[0]) / z / 8.0).round() * 8.0;
                let dy = ((client[1] - from[1]) / z / 8.0).round() * 8.0;
                ed.draft.update(|draft| {
                    if let Some(flow) = draft {
                        for (id, at) in &nodes {
                            flow.layout.insert(id.clone(), [at[0] + dx, at[1] + dy]);
                        }
                    }
                });
            }
            None => {}
        }
    };

    let on_up = move |event: web_sys::PointerEvent| {
        // A node pressed and let go without moving: a click, which opens it. The same on the
        // empty canvas closes what's open.
        match drag.get_value() {
            Some(Drag::Node { id, .. }) if dragging.get_untracked().is_none() => {
                ed.selected.set(Selected::Node(id));
            }
            Some(Drag::Pan { from, .. })
                if (f64::from(event.client_x()) - from[0]).abs()
                    + (f64::from(event.client_y()) - from[1]).abs()
                    < 4.0 =>
            {
                ed.selected.set(Selected::Nothing);
            }
            _ => {}
        }
        if let Some(Drag::Wire { from }) = drag.get_value() {
            let target = document()
                .element_from_point(event.client_x() as f32, event.client_y() as f32)
                .and_then(|el| el.closest("[data-node]").ok().flatten())
                .and_then(|el| el.get_attribute("data-node"))
                .and_then(|id| id.parse::<NodeId>().ok());
            if let Some(to) = target
                && to != from.node
            {
                ed.edit(|flow| {
                    let wire = Wire {
                        from: from.clone(),
                        to,
                    };
                    if !flow.wires.contains(&wire) {
                        flow.wires.push(wire);
                    }
                });
            }
        }
        loose.set(None);
        drag.set_value(None);
        dragging.set(None);
        if let Some(el) = root.get_untracked() {
            let _ = el.release_pointer_capture(event.pointer_id());
        }
    };

    let on_wheel = move |event: web_sys::WheelEvent| {
        // The open node's panel scrolls itself.
        if event
            .target()
            .is_some_and(|t| closest(&t, "[data-sheet]").is_some())
        {
            return;
        }
        event.prevent_default();
        let client = [f64::from(event.client_x()), f64::from(event.client_y())];
        let before = world(client);
        let factor = (-event.delta_y() * 0.0015).exp();
        let z = (zoom.get_untracked() * factor).clamp(0.3, 2.0);
        zoom.set(z);
        let rect = root.get_untracked().map(|el| el.get_bounding_client_rect());
        let (left, top) = rect.map_or((0.0, 0.0), |r| (r.left(), r.top()));
        pan.set([
            client[0] - left - before[0] * z,
            client[1] - top - before[1] * z,
        ]);
    };

    // Moves and releases are heard on the whole page, so a drag keeps going wherever the
    // pointer goes and whatever is drawn under it.
    let moves = window_event_listener(leptos::ev::pointermove, on_move);
    let ups = window_event_listener(leptos::ev::pointerup, on_up);
    on_cleanup(move || {
        moves.remove();
        ups.remove();
    });

    // Escape closes an open node; Delete removes what's selected, unless typing somewhere.
    let keys = window_event_listener(leptos::ev::keydown, move |event: web_sys::KeyboardEvent| {
        if event.key() == "Escape" {
            if !event.default_prevented() {
                ed.selected.set(Selected::Nothing);
            }
            return;
        }
        if event.key() != "Delete" && event.key() != "Backspace" {
            return;
        }
        let typing = document()
            .active_element()
            .is_some_and(|el| matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"));
        if typing || !matches!(ed.view.get_untracked(), View::Edit) {
            return;
        }
        match ed.selected.get_untracked() {
            Selected::Node(id) => {
                remove_node(&ed, &id);
            }
            Selected::Wire(index) => {
                ed.edit(|flow| {
                    if index < flow.wires.len() {
                        flow.wires.remove(index);
                    }
                });
                ed.selected.set(Selected::Nothing);
            }
            Selected::Nothing => {}
        }
    });
    on_cleanup(move || keys.remove());

    let fit = move || {
        let at = positions.get_untracked();
        if at.is_empty() {
            return;
        }
        let min_x = at.values().map(|p| p[0]).fold(f64::MAX, f64::min);
        let min_y = at.values().map(|p| p[1]).fold(f64::MAX, f64::min);
        zoom.set(1.0);
        pan.set([60.0 - min_x, 70.0 - min_y]);
    };
    // Frame the flow once it's there.
    let framed = StoredValue::new(false);
    Effect::new(move |_| {
        if !framed.get_value() && !positions.get().is_empty() {
            framed.set_value(true);
            fit();
        }
    });

    view! {
        <div
            class="canvas"
            node_ref=root
            on:pointerdown=on_down
            on:pointercancel=move |_| { loose.set(None); drag.set_value(None); dragging.set(None); }
            on:wheel=on_wheel
        >
            <div
                class="plane"
                style=move || {
                    let [x, y] = pan.get();
                    format!("transform: translate({x}px, {y}px) scale({})", zoom.get())
                }
            >
                <Groups flow=flow positions=positions />
                <svg class="wires" width="1" height="1">
                    {move || {
                        let Some(flow) = flow.get() else { return Vec::new() };
                        let at = positions.get();
                        let (_, taken) = visits.get();
                        let tracing = matches!(ed.view.get(), View::Trace { .. });
                        let selected = ed.selected.get();
                        flow.wires.iter().enumerate().filter_map(|(i, wire)| {
                            let from = flow.nodes.get(&wire.from.node)?;
                            let a = model::out_anchor(*at.get(&wire.from.node)?, from, wire.from.port);
                            let b = model::in_anchor(*at.get(&wire.to)?);
                            let d = model::curve(a, b);
                            let class = if selected == Selected::Wire(i) {
                                "wire selected"
                            } else if taken.contains(wire) {
                                "wire taken"
                            } else if tracing {
                                "wire idle"
                            } else {
                                "wire"
                            };
                            Some(view! {
                                <g>
                                    <path class=class d=d.clone()></path>
                                    <path class="wire hit" d=d data-wire=i.to_string()></path>
                                </g>
                            })
                        }).collect::<Vec<_>>()
                    }}
                    // A wire the live run passed along glows, drawn from one node to the next, and
                    // stays lit until the run's lights go out. Keyed, so a wire already glowing
                    // isn't drawn again when the next one lights.
                    <For
                        each=move || {
                            if matches!(ed.view.get(), View::Trace { .. }) {
                                return Vec::new();
                            }
                            show.flowing.get().into_iter().collect::<Vec<_>>()
                        }
                        key=|lit| lit.clone()
                        let:lit
                    >
                        {
                            let wire = lit.0;
                            let d = Memo::new(move |_| {
                                let at = positions.get();
                                flow.with(|flow| {
                                    let from = flow.as_ref()?.nodes.get(&wire.from.node)?;
                                    let a = model::out_anchor(*at.get(&wire.from.node)?, from, wire.from.port);
                                    let b = model::in_anchor(*at.get(&wire.to)?);
                                    Some(model::curve(a, b))
                                })
                                .unwrap_or_default()
                            });
                            // The glow, and a dot riding its head from one node to the next.
                            view! {
                                <path class="wire glowing" pathLength="1" d=d></path>
                                <path class="wire spark" pathLength="1" d=d></path>
                            }
                        }
                    </For>
                    {move || loose.get().map(|(a, b)| view! {
                        <path class="wire draft" d=model::curve(a, b)></path>
                    })}
                </svg>
                // Keyed by id: a node keeps its card, and its open form, while it's moved or edited.
                <For
                    each=move || {
                        flow.with(|flow| {
                            flow.as_ref()
                                .map(|flow| flow.nodes.keys().cloned().collect::<Vec<_>>())
                                .unwrap_or_default()
                        })
                    }
                    key=|id| id.clone()
                    let:id
                >
                    <NodeCard id=id flow=flow positions=positions visits=visits diff=diff dragging=dragging />
                </For>
            </div>
            <NodePanel pan=pan zoom=zoom positions=positions root=root />
            {move || match ed.view.get() {
                View::Trace { record, .. } => {
                    let summary = record.summary();
                    let what = if summary.summary.is_empty() { summary.outcome.clone() } else { summary.summary.clone() };
                    Some(view! {
                        <div class="banner">
                            <span>{format!("Run {} · {}", time::when(&record.started_at), what)}</span>
                            <button class="btn small" on:click=move |_| ed.view.set(View::Edit)>"Back to editing"</button>
                        </div>
                    }.into_any())
                }
                View::Diff { version, .. } => Some(view! {
                    <div class="banner">
                        <span>{format!("Compared with version {}: dashed nodes are new or changed", &version[..8.min(version.len())])}</span>
                        <button class="btn small" on:click=move |_| ed.view.set(View::Edit)>"Done"</button>
                    </div>
                }.into_any()),
                View::Edit => match ed.selected.get() {
                    Selected::Wire(index) => Some(view! {
                        <div class="banner">
                            <span>"Wire selected"</span>
                            <button class="btn small danger" on:click=move |_| {
                                ed.edit(|flow| { if index < flow.wires.len() { flow.wires.remove(index); } });
                                ed.selected.set(Selected::Nothing);
                            }>"Remove it"</button>
                        </div>
                    }.into_any()),
                    _ => None,
                },
            }}
            {move || match show.latest.get() {
                Some((n, words)) if matches!(ed.view.get(), View::Edit) => view! {
                    // Keyed by which playing it is, so each one slides in afresh.
                    <div class="hint live-strip" data-n=n.to_string()><span class="live-dot"></span>{words}</div>
                }.into_any(),
                _ => view! {
                    <div class="hint">"Click a node to open it. Drag a port on its right to another node to wire them."</div>
                }.into_any(),
            }}
            <div class="zoom">
                <button class="btn small" on:click=move |_| zoom.update(|z| *z = (*z * 1.2).min(2.0))>"+"</button>
                <button class="btn small" on:click=move |_| zoom.update(|z| *z = (*z / 1.2).max(0.3))>"−"</button>
                <button class="btn small" on:click=move |_| fit()>"Fit"</button>
            </div>
        </div>
    }
}

/// Takes a node out, with its wires and position.
/// Soft backgrounds behind each branch of the flow and the parts it splits into, each named.
/// A name is changed by double-clicking it.
#[component]
fn Groups(flow: Memo<Option<Flow>>, positions: Memo<BTreeMap<NodeId, [f64; 2]>>) -> impl IntoView {
    let ed = expect_context::<Editing>();
    // Worked out again only when the wiring changes, not while a node is dragged.
    let branches =
        Memo::new(move |_| flow.with(|f| f.as_ref().map(model::branches).unwrap_or_default()));
    let renaming = RwSignal::new(None::<NodeId>);

    let rect = move |nodes: &[NodeId], at: &BTreeMap<NodeId, [f64; 2]>, flow: &Flow| {
        let mut bounds: Option<[f64; 4]> = None;
        for id in nodes {
            let (Some(p), Some(node)) = (at.get(id), flow.nodes.get(id)) else {
                continue;
            };
            let r = [p[0], p[1], p[0] + model::NODE_W, p[1] + model::height(node)];
            bounds = Some(bounds.map_or(r, |b| {
                [
                    b[0].min(r[0]),
                    b[1].min(r[1]),
                    b[2].max(r[2]),
                    b[3].max(r[3]),
                ]
            }));
        }
        bounds
    };
    const PART_PAD: f64 = 10.0;
    const PART_LABEL: f64 = 18.0;
    const BRANCH_PAD: f64 = 12.0;
    const BRANCH_LABEL: f64 = 22.0;
    const HUES: [u16; 6] = [24, 205, 145, 275, 340, 55];

    let label = move |key: NodeId, name: String, class: &'static str| {
        let grip = format!("{class}:{key}");
        let editing = {
            let key = key.clone();
            move || renaming.get().as_ref() == Some(&key)
        };
        let key_for_edit = key.clone();
        let save = move |text: String| {
            let key = key.clone();
            renaming.set(None);
            let text = text.trim().to_owned();
            ed.edit(|flow| match irori_types::Name::try_from(text.as_str()) {
                Ok(name) if !text.is_empty() => {
                    flow.groups.insert(key, name);
                }
                _ => {
                    flow.groups.remove(&key);
                }
            });
        };
        let save_on_key = save.clone();
        let shown = name.clone();
        view! {
            <span class=format!("group-label {class}")>
                <span class="group-grip" data-group=grip title="Drag to move everything in it">"⠿"</span>
                <span class="group-name" title="Double-click to rename"
                    on:dblclick=move |e| { e.stop_propagation(); renaming.set(Some(key_for_edit.clone())); }>
                {move || if editing() {
                    let save = save.clone();
                    let save_on_key = save_on_key.clone();
                    view! {
                        <input type="text" class="group-rename" prop:value=shown.clone() autofocus=true
                            on:blur=move |e| save(event_target_value(&e))
                            on:keydown=move |e: web_sys::KeyboardEvent| match e.key().as_str() {
                                "Enter" => save_on_key(event_target_value(&e)),
                                "Escape" => { e.prevent_default(); renaming.set(None); }
                                _ => {}
                            } />
                    }.into_any()
                } else {
                    view! { <span>{shown.clone()}</span> }.into_any()
                }}
                </span>
            </span>
        }
    };

    move || {
        let Some(f) = flow.get() else {
            return Vec::new();
        };
        let at = positions.get();
        branches
            .get()
            .into_iter()
            .enumerate()
            .flat_map(|(i, branch)| {
                let hue = HUES[i % HUES.len()];
                let mut drawn = Vec::new();
                let mut outer = rect(&branch.nodes, &at, &f);
                for part in &branch.parts {
                    let Some(r) = rect(&part.nodes, &at, &f) else {
                        continue;
                    };
                    let r = [
                        r[0] - PART_PAD,
                        r[1] - PART_PAD - PART_LABEL,
                        r[2] + PART_PAD,
                        r[3] + PART_PAD,
                    ];
                    outer = outer.map(|o| {
                        [
                            o[0].min(r[0]),
                            o[1].min(r[1]),
                            o[2].max(r[2]),
                            o[3].max(r[3]),
                        ]
                    });
                    let name = model::part_name(&f, part);
                    drawn.push(
                        view! {
                            <div class="group part" style=format!(
                                "--h:{hue}; left:{}px; top:{}px; width:{}px; height:{}px",
                                r[0], r[1], r[2] - r[0], r[3] - r[1])>
                                {label(part.key.clone(), name, "part")}
                            </div>
                        }
                        .into_any(),
                    );
                }
                if let Some(o) = outer {
                    let o = [
                        o[0] - BRANCH_PAD,
                        o[1] - BRANCH_PAD - BRANCH_LABEL,
                        o[2] + BRANCH_PAD,
                        o[3] + BRANCH_PAD,
                    ];
                    let name = model::branch_name(&f, &branch);
                    drawn.insert(
                        0,
                        view! {
                            <div class="group branch" style=format!(
                                "--h:{hue}; left:{}px; top:{}px; width:{}px; height:{}px",
                                o[0], o[1], o[2] - o[0], o[3] - o[1])>
                                {label(branch.key.clone(), name, "branch")}
                            </div>
                        }
                        .into_any(),
                    );
                }
                drawn
            })
            .collect::<Vec<_>>()
    }
}

pub fn remove_node(ed: &Editing, id: &NodeId) {
    ed.edit(|flow| {
        flow.nodes.remove(id);
        flow.layout.remove(id);
        flow.wires
            .retain(|wire| &wire.from.node != id && &wire.to != id);
    });
    ed.selected.set(Selected::Nothing);
}

#[component]
fn NodeCard(
    id: NodeId,
    flow: Memo<Option<Flow>>,
    positions: Memo<BTreeMap<NodeId, [f64; 2]>>,
    visits: Memo<(BTreeMap<NodeId, Visit>, BTreeSet<Wire>)>,
    diff: Memo<BTreeMap<NodeId, &'static str>>,
    dragging: RwSignal<Option<NodeId>>,
) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let show = expect_context::<live::Show>();
    let node = {
        let id = id.clone();
        Memo::new(move |_| flow.with(|f| f.as_ref().and_then(|f| f.nodes.get(&id).cloned())))
    };
    // How this node came out in what's playing now, if it's in it.
    let lit = {
        let id = id.clone();
        Memo::new(move |_| {
            if !matches!(ed.view.get(), View::Edit) {
                return None;
            }
            show.lit.with(|lit| lit.get(&id).cloned())
        })
    };
    // A trigger counting down its `for`: how far along, and how long is left.
    let holding = {
        let id = id.clone();
        move || {
            let until = show
                .holding
                .with(|h| h.iter().find(|h| h.node == id).map(|h| h.until))?;
            let hold = node.with(|n| match n {
                Some(Node::Trigger {
                    trigger:
                        irori_flow_types::Trigger::State {
                            hold: Some(hold), ..
                        },
                }) => Some(hold.millis()),
                _ => None,
            })?;
            let left = (time::millis(until) - time::millis(time::now())).max(0);
            let hold = hold.max(1);
            #[allow(clippy::cast_precision_loss)]
            let done = (1.0 - left as f64 / hold as f64).clamp(0.0, 1.0) * 100.0;
            Some((done, left))
        }
    };
    // Selected while editing, its form is open in the panel over the canvas.
    let open = {
        let id = id.clone();
        Memo::new(move |_| {
            matches!(ed.view.get(), View::Edit) && ed.selected.get() == Selected::Node(id.clone())
        })
    };
    let for_class = id.clone();
    let for_badge = id.clone();
    let for_status = id.clone();
    let for_place = id.clone();

    let class = move || {
        let mut class = String::from("node");
        if ed.selected.get() == Selected::Node(for_class.clone()) {
            class.push_str(" selected");
        }
        if open.get() {
            class.push_str(" open");
        }
        let problems = ed.problems_at(&for_class);
        if problems.iter().any(|p| p.is_error()) {
            class.push_str(" problem-error");
        } else if !problems.is_empty() {
            class.push_str(" problem-warning");
        }
        if matches!(ed.view.get(), View::Trace { .. }) {
            match visits.get().0.get(&for_class) {
                Some(visit) => {
                    class.push(' ');
                    class.push_str(visit.class);
                }
                None => class.push_str(" unvisited"),
            }
        }
        if matches!(ed.view.get(), View::Edit)
            && ed.active.with(|runs| {
                runs.iter()
                    .any(|run| run.at.iter().any(|t| t.node == for_class))
            })
        {
            class.push_str(" live");
        }
        if let Some(change) = diff.get().get(&for_class) {
            class.push(' ');
            class.push_str(change);
        }
        if dragging.get().as_ref() == Some(&for_class) {
            class.push_str(" dragging");
        }
        if let Some(lit) = lit.get() {
            class.push(' ');
            class.push_str(lit.mark.class());
        }
        class
    };

    let live_value = move || {
        // A condition with several checks shows each one's state on its own line instead.
        if node.with(|n| n.as_ref().and_then(model::checks_of).is_some()) {
            return None;
        }
        let entity = node.with(|n| n.as_ref().and_then(model::primary_entity))?;
        home.states
            .with(|states| states.get(&entity).map(|s| model::state_words(s, &home)))
    };

    let status = move || {
        if !matches!(ed.view.get(), View::Edit) {
            return None;
        }
        let hold_ms = node.with(|node| match node {
            Some(Node::Wait {
                until:
                    irori_flow_types::WaitUntil::State { hold: Some(h), .. }
                    | irori_flow_types::WaitUntil::Expr { hold: Some(h), .. },
                ..
            }) => Some(h.millis()),
            _ => None,
        });
        ed.tick.track();
        let now = time::millis(time::now());
        ed.active.with(|runs| {
            runs.iter()
                .flat_map(|run| run.at.iter())
                .find(|t| t.node == for_status)
                .map(|t| match t.doing.as_str() {
                    "waiting" => match (t.holding_since, hold_ms) {
                        (Some(since), Some(hold)) => format!(
                            "holding {} of {}",
                            time::span(now - time::millis(since)),
                            time::span(hold)
                        ),
                        _ => format!(
                            "waiting {}{}",
                            t.since
                                .map(|s| time::span(now - time::millis(s)))
                                .unwrap_or_default(),
                            t.until
                                .map(|u| format!(
                                    " · gives up in {}",
                                    time::span(time::millis(u) - now)
                                ))
                                .unwrap_or_default()
                        ),
                    },
                    "delaying" => format!(
                        "done in {}",
                        t.until
                            .map(|u| time::span(time::millis(u) - now))
                            .unwrap_or_default()
                    ),
                    "calling" => "calling…".to_owned(),
                    "joining" => "waiting for the other paths".to_owned(),
                    other => other.to_owned(),
                })
        })
    };

    // The card itself, drawn again only when its definition changes.
    let card = {
        let id = id.clone();
        move || {
            node.get().map(|node| {
                let text = model::sentence(&node, &home);
                let colour = model::family(&node);
                let label = model::label(&node);
                view! {
                    {(!node.is_trigger()).then(|| view! { <span class="port in"></span> })}
                    <div class="head">
                        <span class="kind-bar" style=format!("background:{colour}")></span>
                        {label}
                        <span class="nid" title=id.to_string()>{id.to_string()}</span>
                    </div>
                    {match model::checks_of(&node) {
                        // Several checks: a line each, with whether it holds right now.
                        Some(checks) => {
                            let joiner = if checks.any { "or" } else { "and" };
                            let height = model::text_height(&node);
                            view! {
                                <div class="text checks" title=text.clone() style=format!("height:{height}px")>
                                    {checks.clauses.into_iter().enumerate().map(|(i, clause)| {
                                        let line = clause.line(&home);
                                        let dot = move || match clause.holds_now(&home) {
                                            Some(true) => "check-dot holds",
                                            Some(false) => "check-dot fails",
                                            None => "check-dot unknown",
                                        };
                                        view! {
                                            <div class="check-line">
                                                <span class=dot></span>
                                                {(i > 0).then(|| view! { <span class="check-join">{joiner}</span> })}
                                                <span class="check-words">{line}</span>
                                            </div>
                                        }
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }
                        None => view! { <div class="text" title=text.clone()>{text.clone()}</div> }.into_any(),
                    }}
                    {node.ports().into_iter().map(|port| {
                        let name = model::port_label(&node, port);
                        let node_for_port = id.to_string();
                        let port_text = port.to_string();
                        let taken = {
                            let id = id.clone();
                            move || visits.get().1.iter().any(|w| w.from.node == id && w.from.port == port)
                        };
                        view! {
                            <div class="port-row">
                                {name}
                                <span
                                    class="port out"
                                    class:taken=taken
                                    data-port=port_text
                                    data-node-id=node_for_port
                                    title="Drag to a node to wire it"
                                ></span>
                            </div>
                        }
                    }).collect_view()}
                }
            })
        }
    };

    view! {
        <div
            class=class
            data-node=id.to_string()
            style=move || {
                let at = positions.with(|p| p.get(&for_place).copied().unwrap_or([0.0, 0.0]));
                let height = node.with(|n| n.as_ref().map(model::height).unwrap_or(80.0));
                format!("left:{}px; top:{}px; min-height:{}px", at[0], at[1], height)
            }
        >
            {card}
            {move || match ed.view.get() {
                View::Trace { .. } => visits.get().0.get(&for_badge).map(|visit| {
                    let class = match visit.class {
                        "visited-no" => "badge no",
                        "visited-error" => "badge error",
                        _ => "badge",
                    };
                    view! { <span class=class>{visit.seq.to_string()}</span> }
                }),
                _ => None,
            }}
            {move || lit.get().map(|lit| view! {
                // Keyed by the playing, so the badge pops in again for the next run.
                <span class=format!("mark {}", lit.mark.class()) data-show=lit.show.to_string()>{lit.mark.badge()}</span>
                {lit.note.map(|note| { let title = note.clone(); view! { <span class="live-note" title=title>{note}</span> } })}
            })}
            {move || holding().map(|(done, left)| view! {
                <div class="hold-bar" title=format!("fires in {} if it stays", time::span(left))>
                    <span style=format!("--from:{done}%; --ms:{left}ms")></span>
                </div>
            })}
            {move || live_value().map(|value| view! { <span class="now">{format!("now {value}")}</span> })}
            {move || status().map(|s| view! { <div class="status">{s}</div> })}
        </div>
    }
}

/// The open node's form, in a panel over the middle of the canvas — wherever the node is, and
/// however long the form — that scrolls on its own. It grows out of the node and shrinks back.
#[component]
fn NodePanel(
    pan: RwSignal<[f64; 2]>,
    zoom: RwSignal<f64>,
    positions: Memo<BTreeMap<NodeId, [f64; 2]>>,
    root: NodeRef<leptos::html::Div>,
) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let open = Memo::new(move |_| match (ed.view.get(), ed.selected.get()) {
        (View::Edit, Selected::Node(id))
            if ed
                .draft
                .with(|d| d.as_ref().is_some_and(|f| f.nodes.contains_key(&id))) =>
        {
            Some(id)
        }
        _ => None,
    });
    // Kept a moment after it closes, so it can shrink away rather than vanish.
    let shown = RwSignal::new(None::<NodeId>);
    let closing = RwSignal::new(false);
    Effect::new(move |_| match open.get() {
        Some(id) => {
            closing.set(false);
            if shown.get_untracked().as_ref() != Some(&id) {
                shown.set(Some(id));
            }
        }
        None if shown.get_untracked().is_some() => {
            closing.set(true);
            set_timeout(
                move || {
                    if open.try_get_untracked() == Some(None) {
                        let _ = shown.try_set(None);
                        let _ = closing.try_set(false);
                    }
                },
                std::time::Duration::from_millis(170),
            );
        }
        None => {}
    });
    // Where the node is, from the middle of the canvas: where the panel grows out of.
    let origin = move |id: &NodeId| {
        let at = positions
            .get_untracked()
            .get(id)
            .copied()
            .unwrap_or([0.0, 0.0]);
        let [px, py] = pan.get_untracked();
        let z = zoom.get_untracked();
        let (w, h) = root
            .get_untracked()
            .map(|el| (f64::from(el.client_width()), f64::from(el.client_height())))
            .unwrap_or((800.0, 600.0));
        let x = px + (at[0] + model::NODE_W / 2.0) * z - w / 2.0;
        let y = py + at[1] * z - h * 0.3;
        format!("--from-x: {x:.0}px; --from-y: {y:.0}px")
    };
    move || {
        shown.get().map(|id| {
            let style = origin(&id);
            let title = ed.draft.with_untracked(|d| {
                d.as_ref()
                    .and_then(|f| f.nodes.get(&id))
                    .map(model::label)
                    .unwrap_or_default()
            });
            view! {
                <div class="node-panel" class:closing=move || closing.get() data-sheet="" style=style>
                    <div class="node-panel-head">
                        <span class="grow">{title}<span class="nid">{id.to_string()}</span></span>
                        <button class="x" title="Close (Esc)" aria-label="Close"
                            on:click=move |_| ed.selected.set(Selected::Nothing)>"×"</button>
                    </div>
                    <div class="node-panel-body">
                        <crate::inspector::NodeForm id=id />
                    </div>
                </div>
            }
        })
    }
}
