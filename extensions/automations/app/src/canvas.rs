//! The canvas: nodes you drag, wires you draw from a port to a node, pan and zoom — and, laid
//! over the same picture, a run's path, the runs going now, or what changed since a version.

use std::collections::{BTreeMap, BTreeSet};

use irori_flow_types::trace::Outcome;
use irori_flow_types::{Flow, Node, NodeId, Port, PortRef, Wire};
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

use crate::editor::{Editing, Selected, View};
use crate::{Home, model, time};

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
        // An open node's form is for typing and picking, not for dragging the canvas, and the
        // canvas's own buttons are just buttons.
        if closest(&target, "[data-sheet], button, .banner").is_some() {
            return;
        }
        let client = [f64::from(event.client_x()), f64::from(event.client_y())];
        let editing = matches!(ed.view.get_untracked(), View::Edit | View::Diff { .. });
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
            ed.selected.set(Selected::Nothing);
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
            None => {}
        }
    };

    let on_up = move |event: web_sys::PointerEvent| {
        // A node pressed and let go without moving: a click, which opens it.
        if let Some(Drag::Node { id, .. }) = drag.get_value()
            && dragging.get_untracked().is_none()
        {
            ed.selected.set(Selected::Node(id));
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
        // Over an open node's form, the wheel moves the canvas rather than zooming it, so a long
        // form can be scrolled through; a list or a text box in it scrolls itself.
        if let Some(target) = event.target()
            && closest(&target, "[data-sheet]").is_some()
        {
            if closest(&target, ".combo-list, textarea").is_none() {
                event.prevent_default();
                pan.update(|p| {
                    p[0] -= event.delta_x();
                    p[1] -= event.delta_y();
                });
            }
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
                    <NodeCard id=id flow=flow positions=positions visits=visits diff=diff dragging=dragging zoom=zoom />
                </For>
            </div>
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
            <div class="hint">"Click a node to open it. Drag a port on its right to another node to wire them."</div>
            <div class="zoom">
                <button class="btn small" on:click=move |_| zoom.update(|z| *z = (*z * 1.2).min(2.0))>"+"</button>
                <button class="btn small" on:click=move |_| zoom.update(|z| *z = (*z / 1.2).max(0.3))>"−"</button>
                <button class="btn small" on:click=move |_| fit()>"Fit"</button>
            </div>
        </div>
    }
}

/// Takes a node out, with its wires and position.
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
    zoom: RwSignal<f64>,
) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let node = {
        let id = id.clone();
        Memo::new(move |_| flow.with(|f| f.as_ref().and_then(|f| f.nodes.get(&id).cloned())))
    };
    // Selected while editing, the node opens up into its own form.
    let open = {
        let id = id.clone();
        Memo::new(move |_| {
            matches!(ed.view.get(), View::Edit) && ed.selected.get() == Selected::Node(id.clone())
        })
    };
    // Kept a moment after it closes, so it can fold away rather than vanish.
    let shown = RwSignal::new(open.get_untracked());
    let closing = RwSignal::new(false);
    Effect::new(move |_| {
        if open.get() {
            closing.set(false);
            shown.set(true);
        } else if shown.get_untracked() {
            closing.set(true);
            set_timeout(
                // The node may have been deleted meanwhile.
                move || {
                    if open.try_get_untracked() == Some(false) {
                        let _ = shown.try_set(false);
                        let _ = closing.try_set(false);
                    }
                },
                std::time::Duration::from_millis(170),
            );
        }
    });
    let for_class = id.clone();
    let for_badge = id.clone();
    let for_status = id.clone();
    let for_place = id.clone();
    let for_sheet = id.clone();

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
        class
    };

    let live_value = move || {
        let entity = node.with(|n| n.as_ref().and_then(model::primary_entity))?;
        home.states
            .with(|states| states.get(&entity).map(model::state_words))
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
                        <span class="nid">{id.to_string()}</span>
                    </div>
                    <div class="text" title=text.clone()>{text.clone()}</div>
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
            {move || live_value().map(|value| view! { <span class="now">{format!("now {value}")}</span> })}
            {move || (!shown.get()).then(|| status().map(|s| view! { <div class="status">{s}</div> })).flatten()}
            {move || shown.get().then(|| {
                let id = for_sheet.clone();
                view! {
                    // Readable at any zoom: the form is scaled back to its own size.
                    <div
                        class="sheet"
                        data-sheet=""
                        style=move || format!("transform: scale({})", 1.0 / zoom.get())
                    >
                        <div class="sheet-body" class:closing=move || closing.get()>
                            <crate::inspector::NodeForm id=id />
                        </div>
                    </div>
                }
            })}
        </div>
    }
}
