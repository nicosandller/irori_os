//! The network map, in an extension's settings window: what its protocol says is on its network
//! and what reaches what (`docs/specs/protocols.md` §6.8), drawn around Irori.
//!
//! Irori sits in the middle. What it reaches directly (border routers, access points, wired
//! devices) is the first ring, what those reach the next, and so on: a radial tree, each
//! branch given room for as many devices as hang off it, with a mesh's other links drawn across
//! it as curves. The mesh is alive while it's open: Thread links flow toward Irori, Irori sends
//! out a slow pulse, a sleeping sensor breathes. And when one of the protocol's devices reports a
//! state, a spark runs along its path to Irori; a state a command caused runs out first, then
//! back, the way Automations lights a flow's wires as it runs.
//!
//! Every motion here is CSS: `.shell[data-motion="off"]` stops all of it, and nothing waits on
//! an animation frame.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::f64::consts::{PI, TAU};
use std::time::Duration;

use irori_types::{
    EntityId, LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

/// How often the map is asked for again while it's open.
const REFRESH: Duration = Duration::from_secs(5);
/// From one ring to the next.
const RING: f64 = 110.0;
/// Closest two nodes are drawn.
const APART: f64 = 52.0;
/// Room around the drawing, for labels.
const MARGIN: f64 = 56.0;
/// How long a spark takes along one link; `.net-spark` in index.html takes the same.
const HOP_MS: u32 = 420;
/// Most sparks on the map at once; past this a report doesn't add one.
const MOST_SPARKS: usize = 48;

/// A node where it's drawn.
#[derive(Debug, Clone, PartialEq)]
struct Placed {
    node: NetworkNode,
    x: f64,
    y: f64,
    depth: usize,
}

/// A link as it's drawn: from its end further from Irori to the nearer one.
#[derive(Debug, Clone, PartialEq)]
struct Drawn {
    link: NetworkLink,
    far: String,
    near: String,
    /// The SVG path, from `far` to `near`.
    path: String,
    /// The path back, from `near` to `far`.
    back: String,
}

/// The whole drawing.
#[derive(Debug, Clone, PartialEq)]
struct Drawing {
    nodes: Vec<Placed>,
    links: Vec<Drawn>,
    width: f64,
    height: f64,
    /// Each node's next hop toward Irori.
    toward: BTreeMap<String, String>,
}

impl Drawing {
    fn at(&self, id: &str) -> Option<&Placed> {
        self.nodes.iter().find(|placed| placed.node.id == id)
    }

    /// The links from `id` to Irori, in order, each drawn from its far end.
    fn path_home(&self, id: &str) -> Vec<&Drawn> {
        let mut hops = Vec::new();
        let mut at = id.to_owned();
        let mut seen = BTreeSet::new();
        while let Some(next) = self.toward.get(&at) {
            if !seen.insert(at.clone()) {
                break;
            }
            if let Some(link) = self.links.iter().find(|drawn| {
                (drawn.far == at && &drawn.near == next) || (&drawn.far == next && drawn.near == at)
            }) {
                hops.push(link);
            }
            at = next.clone();
        }
        hops
    }
}

/// Lays the map out around its controller.
fn layout(network: &Network) -> Drawing {
    let root = network
        .nodes
        .iter()
        .find(|node| node.role == NetworkRole::Controller)
        .or_else(|| network.nodes.first())
        .map(|node| node.id.clone())
        .unwrap_or_default();
    let mut neighbours: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for link in &network.links {
        neighbours.entry(&link.from).or_default().push(&link.to);
        neighbours.entry(&link.to).or_default().push(&link.from);
    }
    // Rings: how many hops each node is from the controller, and which way it is.
    let mut depth: BTreeMap<String, usize> = BTreeMap::from([(root.clone(), 0)]);
    let mut toward: BTreeMap<String, String> = BTreeMap::new();
    let mut queue = VecDeque::from([root.clone()]);
    while let Some(at) = queue.pop_front() {
        let here = depth[&at];
        let mut next: Vec<&str> = neighbours.get(at.as_str()).cloned().unwrap_or_default();
        next.sort_by_key(|id| sort_key(network, id));
        for other in next {
            if !depth.contains_key(other) {
                depth.insert(other.to_owned(), here + 1);
                toward.insert(other.to_owned(), at.clone());
                queue.push_back(other.to_owned());
            }
        }
    }
    // Anything with no path in is drawn on the first ring, unlinked.
    for node in &network.nodes {
        if !depth.contains_key(&node.id) {
            depth.insert(node.id.clone(), 1);
            toward.insert(node.id.clone(), root.clone());
        }
    }
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in &network.nodes {
        if let Some(parent) = toward.get(&node.id) {
            children
                .entry(parent.clone())
                .or_default()
                .push(node.id.clone());
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|id| sort_key(network, id));
    }
    // Each branch gets a share of the circle for every device at its tips.
    fn leaves(id: &str, children: &BTreeMap<String, Vec<String>>) -> f64 {
        match children.get(id) {
            Some(list) if !list.is_empty() => {
                list.iter().map(|child| leaves(child, children)).sum()
            }
            _ => 1.0,
        }
    }
    let mut angle: BTreeMap<String, f64> = BTreeMap::new();
    let mut stack = vec![(root.clone(), -PI / 2.0 - PI, -PI / 2.0 + PI)];
    while let Some((id, from, to)) = stack.pop() {
        angle.insert(id.clone(), (from + to) / 2.0);
        let Some(list) = children.get(&id) else {
            continue;
        };
        let total: f64 = list.iter().map(|child| leaves(child, &children)).sum();
        let mut start = from;
        for child in list {
            let share = (to - from) * leaves(child, &children) / total;
            stack.push((child.clone(), start, start + share));
            start += share;
        }
    }
    let mut points: BTreeMap<String, (f64, f64)> = network
        .nodes
        .iter()
        .map(|node| {
            let ring = depth[&node.id] as f64 * RING;
            let a = angle.get(&node.id).copied().unwrap_or(0.0);
            (node.id.clone(), (ring * a.cos(), ring * a.sin()))
        })
        .collect();
    // Then nudged apart where a crowded ring puts two on top of each other. Irori stays put.
    let ids: Vec<String> = points.keys().cloned().collect();
    for _ in 0..80 {
        let mut moved = false;
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                let (ax, ay) = points[a];
                let (bx, by) = points[b];
                let (dx, dy) = (bx - ax, by - ay);
                let distance = (dx * dx + dy * dy).sqrt().max(0.01);
                if distance >= APART {
                    continue;
                }
                moved = true;
                let push = (APART - distance) / 2.0;
                let (ux, uy) = (dx / distance, dy / distance);
                if a != &root {
                    points.insert(a.clone(), (ax - ux * push, ay - uy * push));
                }
                if b != &root {
                    points.insert(b.clone(), (bx + ux * push, by + uy * push));
                }
            }
        }
        if !moved {
            break;
        }
    }
    let (mut left, mut top, mut right, mut bottom) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for &(x, y) in points.values() {
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    let width = (right - left + 2.0 * MARGIN).max(320.0);
    let height = (bottom - top + 2.0 * MARGIN).max(220.0);
    // Centred on Irori's side to side, so a lopsided home doesn't hang off one edge.
    let shift_x = width / 2.0 - (left + right) / 2.0;
    let shift_y = height / 2.0 - (top + bottom) / 2.0;
    let nodes: Vec<Placed> = network
        .nodes
        .iter()
        .map(|node| {
            let (x, y) = points[&node.id];
            Placed {
                node: node.clone(),
                x: x + shift_x,
                y: y + shift_y,
                depth: depth[&node.id],
            }
        })
        .collect();
    let at = |id: &str| {
        nodes
            .iter()
            .find(|placed| placed.node.id == id)
            .map(|placed| (placed.x, placed.y))
            .unwrap_or_default()
    };
    let links = network
        .links
        .iter()
        .map(|link| {
            let (far, near) = if depth[&link.from] >= depth[&link.to] {
                (link.from.clone(), link.to.clone())
            } else {
                (link.to.clone(), link.from.clone())
            };
            // A link the tree is made of is nearly straight; one across the mesh bows out.
            let tree = toward.get(&far) == Some(&near);
            let bow = if tree { 0.04 } else { 0.18 };
            let (path, back) = curve(at(&far), at(&near), bow);
            Drawn {
                link: link.clone(),
                far,
                near,
                path,
                back,
            }
        })
        .collect();
    Drawing {
        nodes,
        links,
        width,
        height,
        toward,
    }
}

/// Routers and border routers before what hangs off them, then by name: the same home draws the
/// same way each time.
fn sort_key(network: &Network, id: &str) -> (u8, String) {
    let node = network.nodes.iter().find(|node| node.id == id);
    let order = match node.map(|node| node.role) {
        Some(NetworkRole::BorderRouter) => 0,
        Some(NetworkRole::AccessPoint) => 1,
        Some(NetworkRole::Router) => 2,
        Some(NetworkRole::Device) => 3,
        Some(NetworkRole::EndDevice) => 4,
        Some(NetworkRole::SleepyEndDevice) => 5,
        _ => 6,
    };
    (
        order,
        node.map_or_else(String::new, |node| node.name.to_lowercase()),
    )
}

/// A gentle curve between two points, both ways round.
fn curve(from: (f64, f64), to: (f64, f64), bow: f64) -> (String, String) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let (cx, cy) = (
        (from.0 + to.0) / 2.0 - dy * bow,
        (from.1 + to.1) / 2.0 + dx * bow,
    );
    (
        format!(
            "M{:.1} {:.1} Q{cx:.1} {cy:.1} {:.1} {:.1}",
            from.0, from.1, to.0, to.1
        ),
        format!(
            "M{:.1} {:.1} Q{cx:.1} {cy:.1} {:.1} {:.1}",
            to.0, to.1, from.0, from.1
        ),
    )
}

fn medium_class(medium: Option<NetworkMedium>) -> &'static str {
    match medium {
        Some(NetworkMedium::Thread) => "thread",
        Some(NetworkMedium::Wifi) => "wifi",
        Some(NetworkMedium::Zigbee) => "zigbee",
        Some(NetworkMedium::Bluetooth) => "bluetooth",
        Some(NetworkMedium::Ethernet) => "wired",
        Some(NetworkMedium::Ip) | None => "lan",
    }
}

fn medium_words(medium: Option<NetworkMedium>) -> &'static str {
    match medium {
        Some(NetworkMedium::Thread) => "Thread",
        Some(NetworkMedium::Wifi) => "Wi-Fi",
        Some(NetworkMedium::Zigbee) => "Zigbee",
        Some(NetworkMedium::Bluetooth) => "Bluetooth",
        Some(NetworkMedium::Ethernet) => "wired",
        Some(NetworkMedium::Ip) | None => "the home network",
    }
}

fn quality_class(quality: Option<LinkQuality>) -> &'static str {
    match quality {
        Some(LinkQuality::Excellent) => "q-excellent",
        Some(LinkQuality::Good) => "q-good",
        Some(LinkQuality::Fair) => "q-fair",
        Some(LinkQuality::Poor) => "q-poor",
        None => "q-unknown",
    }
}

fn quality_words(quality: Option<LinkQuality>) -> Option<&'static str> {
    quality.map(|quality| match quality {
        LinkQuality::Excellent => "excellent",
        LinkQuality::Good => "good",
        LinkQuality::Fair => "fair",
        LinkQuality::Poor => "poor",
    })
}

fn role_class(role: NetworkRole) -> &'static str {
    match role {
        NetworkRole::Controller => "controller",
        NetworkRole::BorderRouter => "border-router",
        NetworkRole::AccessPoint => "access-point",
        NetworkRole::Router => "router",
        NetworkRole::EndDevice => "end-device",
        NetworkRole::SleepyEndDevice => "sleepy",
        NetworkRole::Device => "device",
    }
}

fn role_words(role: NetworkRole) -> &'static str {
    match role {
        NetworkRole::Controller => "Controller",
        NetworkRole::BorderRouter => "Border router",
        NetworkRole::AccessPoint => "Access point",
        NetworkRole::Router => "Router",
        NetworkRole::EndDevice => "End device",
        NetworkRole::SleepyEndDevice => "Sleepy end device",
        NetworkRole::Device => "Device",
    }
}

/// A label short enough to sit under a node.
fn short(name: &str) -> String {
    let cut: String = name.chars().take(16).collect();
    if cut.chars().count() < name.chars().count() {
        format!("{}…", cut.trim_end())
    } else {
        cut
    }
}

/// Something moving on the map for a moment.
#[derive(Debug, Clone, PartialEq)]
enum Flash {
    /// A spark along a link, starting after `delay` ms.
    Spark { path: String, delay: u32 },
    /// A ring opening out from a node as a spark arrives.
    Ping { x: f64, y: f64, delay: u32 },
}

/// What a state report sends across the map: along the path from its device to Irori, and,
/// when a command caused it, out from Irori first.
fn flashes_for(drawing: &Drawing, node: &str, commanded: bool) -> Vec<Flash> {
    let hops = drawing.path_home(node);
    if hops.is_empty() {
        return Vec::new();
    }
    let mut flashes = Vec::new();
    let mut delay = 0;
    let here = |id: &str| drawing.at(id).map(|placed| (placed.x, placed.y));
    if commanded {
        for hop in hops.iter().rev() {
            flashes.push(Flash::Spark {
                path: hop.back.clone(),
                delay,
            });
            delay += HOP_MS * 3 / 4;
        }
        if let Some((x, y)) = here(node) {
            flashes.push(Flash::Ping { x, y, delay });
        }
        delay += HOP_MS / 2;
    }
    for hop in &hops {
        flashes.push(Flash::Spark {
            path: hop.path.clone(),
            delay,
        });
        delay += HOP_MS * 3 / 4;
    }
    if let Some(last) = hops.last()
        && let Some((x, y)) = here(&last.near)
    {
        flashes.push(Flash::Ping { x, y, delay });
    }
    flashes
}

/// The map's fold in a settings window. `id` is the extension, whose protocol's devices are the
/// ones it knows.
#[component]
pub fn NetworkMap(id: String) -> impl IntoView {
    let open = RwSignal::new(false);
    let network = RwSignal::new(None::<Network>);
    let trouble = RwSignal::new(None::<String>);
    let selected = RwSignal::new(None::<String>);
    let flashes = RwSignal::new(Vec::<(u64, Flash)>::new());
    let next_flash = StoredValue::new(0_u64);

    let load = {
        let id = id.clone();
        move || {
            let id = id.clone();
            spawn_local(async move {
                match api::fetch_network(&id).await {
                    Ok(fetched) => {
                        if network.get_untracked() != fetched {
                            network.set(fetched);
                        }
                        trouble.set(None);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };
    load();
    let handle = set_interval_with_handle(
        {
            let load = load.clone();
            move || {
                if open.get_untracked() {
                    load();
                }
            }
        },
        REFRESH,
    )
    .ok();
    on_cleanup(move || {
        if let Some(handle) = handle {
            handle.clear();
        }
    });

    let drawing = Memo::new(move |_| network.with(|network| network.as_ref().map(layout)));

    let show = move |more: Vec<Flash>| {
        if more.is_empty() || flashes.with_untracked(Vec::len) > MOST_SPARKS {
            return;
        }
        let last = more
            .iter()
            .map(|flash| match flash {
                Flash::Spark { delay, .. } | Flash::Ping { delay, .. } => *delay,
            })
            .max()
            .unwrap_or(0);
        let keys: Vec<u64> = more
            .iter()
            .map(|_| {
                let key = next_flash.get_value();
                next_flash.set_value(key + 1);
                key
            })
            .collect();
        flashes.update(|now| now.extend(keys.iter().copied().zip(more)));
        set_timeout(
            move || {
                flashes.try_update(|now| now.retain(|(key, _)| !keys.contains(key)));
            },
            Duration::from_millis(u64::from(last + HOP_MS * 2)),
        );
    };

    // A device's report runs across the map, while it's open.
    let live = expect_context::<crate::Live>();
    {
        let protocol = id.clone();
        Effect::new(move |_| {
            let Some((_, entity_id, commanded)) = live.heard.get() else {
                return;
            };
            if !open.get_untracked() {
                return;
            }
            let Some(drawing) = drawing.get_untracked() else {
                return;
            };
            let Some(node) = live
                .home
                .with_untracked(|home| node_of(&drawing, home, &protocol, &entity_id))
            else {
                return;
            };
            show(flashes_for(&drawing, &node, commanded));
        });
    }
    // Opening it sends a pulse out along every link once, ring by ring: the map saying hello.
    // Once each time it opens, as soon as there's a map to send it across.
    let greeted = StoredValue::new(false);
    // Its things pop in as it opens, not again each time the map changes while it's open.
    let intro = RwSignal::new(false);
    Effect::new(move |_| {
        if !open.get() {
            greeted.set_value(false);
            return;
        }
        if greeted.get_value() {
            return;
        }
        let Some(drawing) = drawing.get() else {
            return;
        };
        greeted.set_value(true);
        intro.set(true);
        set_timeout(
            move || {
                intro.try_set(false);
            },
            Duration::from_millis(2500),
        );
        let hello: Vec<Flash> = drawing
            .links
            .iter()
            .take(MOST_SPARKS * 2)
            .map(|drawn| Flash::Spark {
                path: drawn.back.clone(),
                delay: 250
                    + 260
                        * drawing
                            .at(&drawn.near)
                            .map_or(0, |placed| placed.depth as u32),
            })
            .collect();
        show(hello);
    });

    let state = move || {
        network.with(|network| {
            network.as_ref().map(|network| {
                let devices = network
                    .nodes
                    .iter()
                    .filter(|node| node.device_unique_id.is_some())
                    .count();
                let mediums: BTreeSet<&str> = network
                    .nodes
                    .iter()
                    .filter(|node| node.device_unique_id.is_some())
                    .map(|node| medium_words(node.medium))
                    .collect();
                let mut words = match devices {
                    1 => "1 device".to_owned(),
                    n => format!("{n} devices"),
                };
                if !mediums.is_empty() {
                    words.push_str(" · ");
                    words.push_str(&mediums.into_iter().collect::<Vec<_>>().join(", "));
                }
                words
            })
        })
    };

    let body = view! {
        <div class="net-map">
            {move || trouble.get().map(|why| view! { <p class="muted small">{why}</p> })}
            {move || match drawing.get() {
                None => view! {
                    <p class="muted small">"It hasn't drawn its network yet."</p>
                }
                .into_any(),
                Some(drawing) => draw(drawing, selected, flashes, intro).into_any(),
            }}
        </div>
    }
    .into_any();

    view! {
        <div class="settings-group net-map-group">
            {crate::fold::fold(
                None,
                crate::fold::Head {
                    icon: None,
                    title: view! { "Network map" }.into_any(),
                    state: Some(view! { {state} }.into_any()),
                    actions: None,
                },
                open.into(),
                move || {
                    open.update(|open| *open = !*open);
                    if open.get_untracked() {
                        load();
                    }
                },
                body,
            )}
        </div>
    }
}

/// The map node that's the entity's device, or the device it's reached through.
fn node_of(
    drawing: &Drawing,
    home: &api::Home,
    protocol: &str,
    entity_id: &EntityId,
) -> Option<String> {
    let entity = home
        .entities
        .iter()
        .find(|entity| &entity.id == entity_id)?;
    let mut device_id = entity.device_id.clone()?;
    for _ in 0..4 {
        let device = home.devices.iter().find(|device| device.id == device_id)?;
        if device.protocol.as_str() != protocol {
            return None;
        }
        if let Some(placed) = drawing
            .nodes
            .iter()
            .find(|placed| placed.node.device_unique_id.as_ref() == Some(&device.unique_id))
        {
            return Some(placed.node.id.clone());
        }
        device_id = device.via_device_id.clone()?;
    }
    None
}

fn draw(
    drawing: Drawing,
    selected: RwSignal<Option<String>>,
    flashes: RwSignal<Vec<(u64, Flash)>>,
    intro: RwSignal<bool>,
) -> impl IntoView {
    let view_box = format!("0 0 {:.0} {:.0}", drawing.width, drawing.height);
    let root = drawing
        .nodes
        .iter()
        .find(|placed| placed.depth == 0)
        .map(|placed| (placed.x, placed.y))
        .unwrap_or_default();
    let rings = drawing
        .nodes
        .iter()
        .map(|placed| placed.depth)
        .max()
        .unwrap_or(0);
    let picked = {
        let drawing = drawing.clone();
        move || {
            let id = selected.get()?;
            let placed = drawing.at(&id)?.clone();
            let heard: Vec<(String, Option<&'static str>, &'static str)> = drawing
                .links
                .iter()
                .filter(|drawn| drawn.far == id || drawn.near == id)
                .filter_map(|drawn| {
                    let other = if drawn.far == id {
                        &drawn.near
                    } else {
                        &drawn.far
                    };
                    let other = drawing.at(other)?;
                    Some((
                        other.node.name.clone(),
                        quality_words(drawn.link.quality),
                        medium_words(drawn.link.medium),
                    ))
                })
                .collect();
            Some((placed, heard))
        }
    };
    let links = drawing.links.clone();
    let nodes = drawing.nodes.clone();
    let mediums: BTreeSet<&'static str> = drawing
        .links
        .iter()
        .map(|drawn| medium_class(drawn.link.medium))
        .collect();
    view! {
        <svg class="net-svg" class:intro=move || intro.get() viewBox=view_box role="img" aria-label="Network map">
            <defs>
                <radialGradient id="net-glow">
                    <stop offset="0%" stop-color="var(--ember)" stop-opacity=".35" />
                    <stop offset="100%" stop-color="var(--ember)" stop-opacity="0" />
                </radialGradient>
            </defs>
            // Faint rings, one for each step out from Irori.
            {(1..=rings)
                .map(|ring| view! {
                    <circle
                        class="net-ring"
                        cx=format!("{:.1}", root.0)
                        cy=format!("{:.1}", root.1)
                        r=format!("{:.1}", ring as f64 * RING)
                    />
                })
                .collect_view()}
            <circle class="net-halo" cx=format!("{:.1}", root.0) cy=format!("{:.1}", root.1) r="70" />
            <circle class="net-pulse" cx=format!("{:.1}", root.0) cy=format!("{:.1}", root.1) r="24" />
            <circle class="net-pulse late" cx=format!("{:.1}", root.0) cy=format!("{:.1}", root.1) r="24" />
            <g class="net-links">
                {links
                    .into_iter()
                    .map(|drawn| {
                        let far = drawn.far.clone();
                        let near = drawn.near.clone();
                        let lit = move || {
                            selected.with(|picked| {
                                picked.as_deref().is_some_and(|id| id == far || id == near)
                            })
                        };
                        let class = format!(
                            "net-link {} {}",
                            medium_class(drawn.link.medium),
                            quality_class(drawn.link.quality)
                        );
                        view! { <path class=class class:lit=lit d=drawn.path /> }
                    })
                    .collect_view()}
            </g>
            <g class="net-flashes">
                <For
                    each=move || flashes.get()
                    key=|(key, _)| *key
                    children=|(_, flash)| match flash {
                        Flash::Spark { path, delay } => view! {
                            <path
                                class="net-spark"
                                d=path
                                pathLength="1"
                                style=format!("animation-delay: {delay}ms")
                            />
                        }
                        .into_any(),
                        Flash::Ping { x, y, delay } => view! {
                            <circle
                                class="net-ping"
                                cx=format!("{x:.1}")
                                cy=format!("{y:.1}")
                                r="10"
                                style=format!("animation-delay: {delay}ms")
                            />
                        }
                        .into_any(),
                    }
                />
            </g>
            <g class="net-nodes">
                {nodes
                    .into_iter()
                    .enumerate()
                    .map(|(i, placed)| node_view(placed, i, selected))
                    .collect_view()}
            </g>
        </svg>
        <div class="net-legend">
            {mediums
                .into_iter()
                .map(|medium| {
                    let words = match medium {
                        "thread" => "Thread",
                        "wifi" => "Wi-Fi",
                        "wired" => "Wired",
                        "zigbee" => "Zigbee",
                        "bluetooth" => "Bluetooth",
                        _ => "Home network",
                    };
                    view! {
                        <span class="net-key">
                            <svg viewBox="0 0 28 8" aria-hidden="true">
                                <path class=format!("net-link {medium} q-good") d="M1 4 L27 4" />
                            </svg>
                            {words}
                        </span>
                    }
                })
                .collect_view()}
        </div>
        <div class="net-info">
            {move || match picked() {
                None => view! {
                    <p class="muted small">
                        "Tap anything on the map to see how it's connected. A device's "
                        "messages travel to Irori along its line as they come in."
                    </p>
                }
                .into_any(),
                Some((placed, heard)) => view! {
                    <div class="net-picked">
                        <span class="net-picked-name">{placed.node.name.clone()}</span>
                        <span class="fold-state">
                            {role_words(placed.node.role)}
                            {placed.node.medium.map(|medium| format!(" · {}", medium_words(Some(medium))))}
                        </span>
                    </div>
                    {placed.node.detail.clone().map(|detail| view! { <p class="muted small">{detail}</p> })}
                    <ul class="net-heard">
                        {heard
                            .into_iter()
                            .map(|(name, quality, medium)| view! {
                                <li>
                                    <span>{name}</span>
                                    <span class="muted small">
                                        {medium}
                                        {quality.map(|quality| format!(", {quality}"))}
                                    </span>
                                </li>
                            })
                            .collect_view()}
                    </ul>
                }
                .into_any(),
            }}
        </div>
    }
}

fn node_view(placed: Placed, index: usize, selected: RwSignal<Option<String>>) -> impl IntoView {
    let id = placed.node.id.clone();
    let id_click = id.clone();
    let role = placed.node.role;
    let stranger = placed.node.device_unique_id.is_none()
        && matches!(role, NetworkRole::Router | NetworkRole::EndDevice);
    let class = format!(
        "net-node {} {}",
        role_class(role),
        medium_class(placed.node.medium)
    );
    // Each ring arrives a moment after the one inside it.
    let style = format!(
        "animation-delay: {}ms",
        placed.depth * 140 + (index % 12) * 18
    );
    let radius = match role {
        NetworkRole::Controller => 20.0,
        NetworkRole::BorderRouter | NetworkRole::AccessPoint => 13.0,
        NetworkRole::Router => 10.0,
        NetworkRole::Device => 9.0,
        NetworkRole::EndDevice => 8.0,
        NetworkRole::SleepyEndDevice => 7.0,
    };
    let shape = match role {
        NetworkRole::Controller => view! {
            <circle class="net-shape" r="20" />
            <rect class="net-mark-frame" x="-8" y="-8" width="16" height="16" rx="3" />
            <rect class="net-mark" x="-4.5" y="-4.5" width="9" height="9" rx="1.5" />
        }
        .into_any(),
        NetworkRole::BorderRouter => {
            let points: Vec<String> = (0..6)
                .map(|corner| {
                    let a = TAU * f64::from(corner) / 6.0 - PI / 2.0;
                    format!("{:.1},{:.1}", 13.0 * a.cos(), 13.0 * a.sin())
                })
                .collect();
            view! {
                <polygon class="net-shape" points=points.join(" ") />
                <circle class="net-core" r="3.5" />
            }
            .into_any()
        }
        NetworkRole::AccessPoint => view! {
            <circle class="net-shape" r="13" />
            <path class="net-glyph" d="M-6 -1 Q0 -7 6 -1 M-3.5 1.8 Q0 -1.5 3.5 1.8" />
            <circle class="net-core" cy="4.5" r="1.6" />
        }
        .into_any(),
        _ => view! { <circle class="net-shape" r=format!("{radius}") /> }.into_any(),
    };
    let label = short(&placed.node.name);
    let full = placed.node.name.clone();
    let title = full.clone();
    view! {
        <g
            class=class
            class:stranger=stranger
            class:picked=move || selected.with(|picked| picked.as_deref() == Some(id.as_str()))
            transform=format!("translate({:.1} {:.1})", placed.x, placed.y)
            tabindex="0"
            role="button"
            aria-label=full.clone()
            on:click=move |_| {
                selected.update(|picked| {
                    *picked = if picked.as_deref() == Some(id_click.as_str()) {
                        None
                    } else {
                        Some(id_click.clone())
                    };
                });
            }
        >
            <g class="net-pop" style=style>
                <title>{title}</title>
                {shape}
                <text class="net-label" y=format!("{:.1}", radius + 14.0)>{label}</text>
            </g>
        </g>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, role: NetworkRole, medium: Option<NetworkMedium>) -> NetworkNode {
        NetworkNode {
            id: id.to_owned(),
            name: id.to_owned(),
            role,
            medium,
            device_unique_id: None,
            detail: None,
        }
    }

    fn link(from: &str, to: &str, medium: NetworkMedium) -> NetworkLink {
        NetworkLink {
            from: from.to_owned(),
            to: to.to_owned(),
            medium: Some(medium),
            quality: Some(LinkQuality::Good),
            detail: None,
        }
    }

    /// Irori, a border router, a Thread router behind it with a sensor, a Wi-Fi plug.
    fn home() -> Network {
        Network {
            nodes: vec![
                node("irori", NetworkRole::Controller, None),
                node("br", NetworkRole::BorderRouter, Some(NetworkMedium::Thread)),
                node("router", NetworkRole::Router, Some(NetworkMedium::Thread)),
                node(
                    "sensor",
                    NetworkRole::SleepyEndDevice,
                    Some(NetworkMedium::Thread),
                ),
                node("wifi", NetworkRole::AccessPoint, Some(NetworkMedium::Wifi)),
                node("plug", NetworkRole::Device, Some(NetworkMedium::Wifi)),
            ],
            links: vec![
                link("br", "irori", NetworkMedium::Ip),
                link("router", "br", NetworkMedium::Thread),
                link("sensor", "router", NetworkMedium::Thread),
                // The sensor also hears the border router: across the tree, not part of it.
                link("sensor", "br", NetworkMedium::Thread),
                link("wifi", "irori", NetworkMedium::Ip),
                link("plug", "wifi", NetworkMedium::Wifi),
            ],
        }
    }

    #[test]
    fn irori_is_in_the_middle_and_each_hop_a_ring_further_out() {
        let drawing = layout(&home());
        let depth = |id: &str| drawing.at(id).expect("placed").depth;
        assert_eq!(depth("irori"), 0);
        assert_eq!((depth("br"), depth("wifi")), (1, 1));
        assert_eq!((depth("router"), depth("plug")), (2, 2));
        // The sensor hears the border router directly, so it's one ring out from it.
        assert_eq!(depth("sensor"), 2);
        let irori = drawing.at("irori").expect("placed");
        assert!((irori.x - drawing.width / 2.0).abs() < RING, "{irori:?}");
    }

    #[test]
    fn nothing_is_drawn_on_top_of_anything_else() {
        let mut network = home();
        for n in 0..30 {
            network.nodes.push(node(
                &format!("lamp-{n}"),
                NetworkRole::EndDevice,
                Some(NetworkMedium::Thread),
            ));
            network
                .links
                .push(link(&format!("lamp-{n}"), "router", NetworkMedium::Thread));
        }
        let drawing = layout(&network);
        for (i, a) in drawing.nodes.iter().enumerate() {
            for b in &drawing.nodes[i + 1..] {
                let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                assert!(
                    distance > APART * 0.8,
                    "{} and {} at {distance}",
                    a.node.id,
                    b.node.id
                );
            }
        }
    }

    #[test]
    fn a_report_runs_from_its_device_to_irori_and_a_command_out_first() {
        let drawing = layout(&home());
        let sparks = |flashes: &[Flash]| {
            flashes
                .iter()
                .filter(|flash| matches!(flash, Flash::Spark { .. }))
                .count()
        };
        // The plug is two hops out: plug → Wi-Fi → Irori.
        let report = flashes_for(&drawing, "plug", false);
        assert_eq!(sparks(&report), 2);
        assert!(matches!(report.last(), Some(Flash::Ping { .. })));
        // A command goes out and comes back: four sparks, the device pinged on the way.
        let command = flashes_for(&drawing, "plug", true);
        assert_eq!(sparks(&command), 4);
        // Irori itself sends nothing anywhere.
        assert!(flashes_for(&drawing, "irori", false).is_empty());
    }

    #[test]
    fn a_node_with_no_way_in_still_has_a_place() {
        let mut network = home();
        network
            .nodes
            .push(node("lost", NetworkRole::Device, Some(NetworkMedium::Ip)));
        let drawing = layout(&network);
        assert_eq!(drawing.at("lost").expect("placed").depth, 1);
    }

    fn device(id: &str, protocol: &str, unique_id: &str, via: Option<&str>) -> irori_types::Device {
        irori_types::Device {
            id: id.parse().expect("a valid device id"),
            protocol: protocol.parse().expect("a valid protocol id"),
            unique_id: unique_id.parse().expect("a valid unique id"),
            name: id.parse().expect("a valid name"),
            description: None,
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            serial_number: None,
            area_id: None,
            suggested_area: None,
            via_device_id: via.map(|via| via.parse().expect("a valid device id")),
        }
    }

    fn entity(id: &str, protocol: &str, device: &str) -> irori_types::Entity {
        irori_types::Entity {
            id: id.parse().expect("a valid entity id"),
            protocol: protocol.parse().expect("a valid protocol id"),
            unique_id: format!("{device}-{id}").parse().expect("a valid unique id"),
            name: "Reading".parse().expect("a valid name"),
            device_id: Some(device.parse().expect("a valid device id")),
            area_id: None,
            capabilities: irori_types::Capabilities::BinarySensor(
                irori_types::BinarySensorCapabilities { device_class: None },
            ),
            entity_category: None,
        }
    }

    #[test]
    fn a_report_finds_its_device_on_the_map_or_the_bridge_it_comes_through() {
        let mut network = home();
        network.nodes[5].device_unique_id = Some("FAB-01".parse().expect("a valid unique id"));
        let drawing = layout(&network);
        let home = api::Home {
            devices: vec![
                // The plug, on the map as itself.
                device("plug", "matter", "FAB-01", None),
                // A bulb behind a bridge behind the plug: the map has only the plug.
                device("bridge", "matter", "FAB-01-ep1", Some("plug")),
                device("bulb", "matter", "FAB-01-ep2", Some("bridge")),
                // Another protocol's device with the same unique id is someone else's.
                device("other", "zigbee", "FAB-01", None),
            ],
            entities: vec![
                entity("binary_sensor.plug", "matter", "plug"),
                entity("binary_sensor.bulb", "matter", "bulb"),
                entity("binary_sensor.other", "zigbee", "other"),
            ],
            ..api::Home::default()
        };
        let find = |entity: &str| {
            node_of(
                &drawing,
                &home,
                "matter",
                &entity.parse().expect("a valid entity id"),
            )
        };
        assert_eq!(find("binary_sensor.plug").as_deref(), Some("plug"));
        assert_eq!(find("binary_sensor.bulb").as_deref(), Some("plug"));
        assert_eq!(find("binary_sensor.other"), None);
        assert_eq!(find("binary_sensor.nothing"), None);
    }

    #[test]
    fn a_long_name_is_cut_under_its_node() {
        assert_eq!(short("Lamp"), "Lamp");
        assert_eq!(short("Living room ceiling light"), "Living room ceil…");
    }
}
