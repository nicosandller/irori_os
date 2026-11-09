//! The network map, a tab of the Devices page: every protocol's network (`docs/specs/protocols.md`
//! §6.8) drawn as one home around Irori.
//!
//! Each running protocol says what's on its networks and who reaches whom; this joins them. Every
//! network's controller is Irori, so they meet in the middle, and an access point named the same
//! by two (the home's Wi-Fi, for Matter and ESPHome) is one. Only the home's devices are drawn,
//! with whatever carries their messages to Irori. A node that's one of Irori's devices carries its
//! room, model, battery and whether it's answering, from the home.
//!
//! The drawing itself is `network_map.js`: the devices hold still and the links move, a device's
//! messages run along its path to Irori (now and then, not every one), and the backdrop bends
//! under them. A device can be dragged somewhere else, and stays there in this browser. This side
//! is the toolbar, the side panel and the data.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use irori_types::{
    ActionPlace, Availability, Capabilities, DeviceId, EntityId, LinkQuality, NetworkMedium,
    NetworkRole, State,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use web_sys::js_sys::{Array, Function, Object, Reflect};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast as _, JsValue};

use crate::api::{self, DrawnNetwork, Home};

/// How often the networks are asked for again while the map is showing.
const REFRESH: Duration = Duration::from_secs(5);
/// Irori's id on the joined map.
pub const HUB: &str = "irori";
/// How many messages the panel lists.
const LOGGED: usize = 5;

/// What a node is, as the map draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hub,
    /// A coordinator, border router or access point: the way into a network.
    Coord,
    Router,
    End,
    Sleepy,
    /// On the home's own network.
    Device,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Hub => "hub",
            Kind::Coord => "coord",
            Kind::Router => "router",
            Kind::End => "end",
            Kind::Sleepy => "sleepy",
            Kind::Device => "device",
        }
    }
}

/// One network on the joined map.
#[derive(Debug, Clone, PartialEq)]
pub struct Net {
    pub name: String,
    /// Which colour it's drawn in: `zigbee`, `matter`, `esphome`, `thread`, `cast`, `wifi`,
    /// `cloud`, `other-N`.
    pub tone: String,
    pub extension: String,
    pub detail: Option<String>,
    /// When its links were last measured, in seconds since 1970.
    pub scanned_at: Option<i64>,
}

/// A node on the joined map.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub role: NetworkRole,
    pub medium: Option<NetworkMedium>,
    /// The network it's drawn in the colour of, and every network it's on.
    pub net: usize,
    pub nets: Vec<usize>,
    /// Its next hop toward Irori.
    pub parent: Option<String>,
    /// The device in the home it is.
    pub device: Option<DeviceId>,
    pub detail: Option<String>,
}

/// A link on the joined map: `a` is the end nearer Irori, when it's anyone's way there.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub a: String,
    pub b: String,
    pub net: usize,
    pub quality: Option<LinkQuality>,
    pub wired: bool,
    pub reading: Option<String>,
    pub detail: Option<String>,
}

impl Link {
    /// How well it carries, 0-1, for the weight of its line and the bar in the panel.
    fn strength(&self) -> f64 {
        if self.wired {
            return 1.0;
        }
        match self.quality {
            Some(LinkQuality::Excellent) => 1.0,
            Some(LinkQuality::Good) => 0.7,
            Some(LinkQuality::Fair) => 0.32,
            Some(LinkQuality::Poor) => 0.15,
            None => 0.5,
        }
    }

    /// `wired`, `good`, `weak`, `poor` or `unknown`.
    fn look(&self) -> &'static str {
        if self.wired {
            return "wired";
        }
        match self.quality {
            Some(LinkQuality::Excellent | LinkQuality::Good) => "good",
            Some(LinkQuality::Fair) => "weak",
            Some(LinkQuality::Poor) => "poor",
            None => "unknown",
        }
    }

    fn word(&self) -> &'static str {
        match self.look() {
            "wired" => "Wired",
            "good" => "Good signal",
            "weak" => "Weak signal",
            "poor" => "Poor signal",
            _ => "Not measured",
        }
    }

    fn other(&self, id: &str) -> Option<&str> {
        if self.a == id {
            Some(&self.b)
        } else if self.b == id {
            Some(&self.a)
        } else {
            None
        }
    }
}

/// Every network, joined into one home.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Joined {
    pub networks: Vec<Net>,
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
}

impl Joined {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The way from `id` to Irori, `id` first.
    pub fn route(&self, id: &str) -> Vec<String> {
        let mut route = vec![id.to_owned()];
        let mut at = self.node(id);
        while let Some(parent) = at.and_then(|node| node.parent.clone()) {
            if route.contains(&parent) {
                break;
            }
            at = self.node(&parent);
            route.push(parent);
        }
        route
    }

    fn link(&self, a: &str, b: &str) -> Option<&Link> {
        self.links
            .iter()
            .find(|link| (link.a == a && link.b == b) || (link.a == b && link.b == a))
    }

    /// Its link to its parent, how it reaches Irori.
    fn uplink(&self, id: &str) -> Option<&Link> {
        let parent = self.node(id)?.parent.as_deref()?;
        self.link(id, parent)
    }

    fn device_node(&self, device: &DeviceId) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|node| node.device.as_ref() == Some(device))
    }
}

/// The colour a network is drawn in, from its name: the protocols Irori ships have their own,
/// anything else takes the next of the spare ones.
fn tone(name: &str, spare: &mut usize) -> String {
    let name = name.to_lowercase();
    for known in ["zigbee", "matter", "esphome", "thread", "cast"] {
        if name.contains(known) {
            return known.to_owned();
        }
    }
    if name.contains("wi-fi") || name.contains("wifi") {
        return "wifi".to_owned();
    }
    *spare += 1;
    format!("other-{}", (*spare - 1) % 4 + 1)
}

/// How a node with `role` is drawn.
fn kind_of(role: NetworkRole) -> Kind {
    match role {
        NetworkRole::Controller => Kind::Hub,
        NetworkRole::Coordinator
        | NetworkRole::BorderRouter
        | NetworkRole::AccessPoint
        | NetworkRole::Cloud => Kind::Coord,
        NetworkRole::Router => Kind::Router,
        NetworkRole::EndDevice => Kind::End,
        NetworkRole::SleepyEndDevice => Kind::Sleepy,
        NetworkRole::Device => Kind::Device,
    }
}

/// Joins every protocol's networks into one, around one Irori. `home` says which device each
/// node is.
pub fn join(drawn: &[DrawnNetwork], home: &Home) -> Joined {
    let mut joined = Joined::default();
    let mut spare = 0;
    let mut at: BTreeMap<String, usize> = BTreeMap::new();
    // Where each host is on the joined map.
    let mut hosts: BTreeMap<String, usize> = BTreeMap::new();
    joined.nodes.push(Node {
        id: HUB.to_owned(),
        name: "Irori".to_owned(),
        kind: Kind::Hub,
        role: NetworkRole::Controller,
        medium: None,
        net: 0,
        nets: Vec::new(),
        parent: None,
        device: None,
        detail: None,
    });
    at.insert(HUB.to_owned(), 0);
    let mut links: BTreeMap<(String, String), Link> = BTreeMap::new();

    let all = drawn;
    let in_home = |extension: &str, unique: Option<&irori_types::UniqueId>| {
        unique.and_then(|unique| {
            home.devices
                .iter()
                .find(|device| device.protocol.as_str() == extension && &device.unique_id == unique)
                .map(|device| device.id.clone())
        })
    };
    // Which extension each network came from, by name, to tell two of the same name apart.
    let mut owners: Vec<String> = Vec::new();
    for (position, drawn) in all.iter().enumerate() {
        let network = &drawn.network;
        // A network with none of the home's devices on it isn't drawn: a protocol that's found
        // devices nobody has added yet (the Demo's, say) has nothing of the home's to show.
        if !network
            .nodes
            .iter()
            .any(|node| in_home(drawn.extension.as_str(), node.device_unique_id.as_ref()).is_some())
        {
            continue;
        }
        let index = joined.networks.len();
        let owner = home
            .extensions
            .get(&drawn.extension)
            .map_or_else(|| drawn.extension.to_string(), |e| e.name.clone());
        let name = network.name.clone().unwrap_or_else(|| owner.clone());
        owners.push(owner);
        // A network reached through a vendor's cloud is drawn in the cloud's colour, whatever
        // it's called.
        let through_cloud = network
            .nodes
            .iter()
            .any(|node| node.role == NetworkRole::Cloud);
        joined.networks.push(Net {
            tone: if through_cloud {
                "cloud".to_owned()
            } else {
                tone(&name, &mut spare)
            },
            name,
            extension: drawn.extension.to_string(),
            detail: network.detail.clone(),
            scanned_at: network.scanned_at.map(|at| at.as_second()),
        });
        // The ids a network uses for its own nodes, on the joined map: its extension's, and which
        // of that extension's networks it is, before its own. The same from one visit to the
        // next, so a device dragged somewhere is found there again.
        let within = all[..position]
            .iter()
            .filter(|earlier| earlier.extension == drawn.extension)
            .count();
        let prefix = format!("{}#{within}", drawn.extension);
        let mut ids: BTreeMap<&str, String> = BTreeMap::new();
        for node in &network.nodes {
            let device = in_home(drawn.extension.as_str(), node.device_unique_id.as_ref());
            let named = device
                .as_ref()
                .and_then(|id| home.devices.iter().find(|device| &device.id == id))
                .map(|device| device.name.to_string());
            let kind = kind_of(node.role);
            // A box another network already drew, by its host: a Nest Hub that Cast has as a
            // device and Matter hears as a border router is one node, both.
            let same_box = node
                .host
                .as_ref()
                .filter(|_| node.role != NetworkRole::Controller)
                .and_then(|host| hosts.get(host))
                .copied();
            if let Some(place) = same_box {
                let kept = &mut joined.nodes[place];
                ids.insert(&node.id, kept.id.clone());
                if !kept.nets.contains(&index) {
                    kept.nets.push(index);
                }
                if kept.device.is_none() && device.is_some() {
                    kept.device = device;
                    kept.name = named.unwrap_or_else(|| node.name.clone());
                }
                if kind == Kind::Coord && kept.kind != Kind::Coord {
                    kept.kind = kind;
                    kept.role = node.role;
                    kept.net = index;
                    kept.detail = node.detail.clone().or(kept.detail.take());
                }
                continue;
            }
            let id = match node.role {
                NetworkRole::Controller => HUB.to_owned(),
                NetworkRole::AccessPoint => format!("ap:{}", node.id),
                _ => format!("{prefix}:{}", node.id),
            };
            ids.insert(&node.id, id.clone());
            if let Some(&place) = at.get(&id) {
                if !joined.nodes[place].nets.contains(&index) {
                    joined.nodes[place].nets.push(index);
                }
                continue;
            }
            at.insert(id.clone(), joined.nodes.len());
            if let Some(host) = node.host.clone() {
                hosts.insert(host, joined.nodes.len());
            }
            joined.nodes.push(Node {
                id,
                name: named.unwrap_or_else(|| node.name.clone()),
                kind,
                role: node.role,
                medium: node.medium,
                net: index,
                nets: vec![index],
                parent: None,
                device,
                detail: node.detail.clone(),
            });
        }
        for link in &network.links {
            let (Some(from), Some(to)) = (ids.get(link.from.as_str()), ids.get(link.to.as_str()))
            else {
                continue;
            };
            if from == to {
                continue;
            }
            let key = if from < to {
                (from.clone(), to.clone())
            } else {
                (to.clone(), from.clone())
            };
            let wired = link.quality.is_none()
                && matches!(
                    link.medium,
                    Some(NetworkMedium::Serial | NetworkMedium::Ethernet)
                );
            let candidate = Link {
                // Further first, as drawn; turned round once the tree is known.
                a: to.clone(),
                b: from.clone(),
                net: index,
                quality: link.quality,
                wired,
                reading: link.reading.clone(),
                detail: link.detail.clone(),
            };
            match links.get(&key) {
                Some(kept) if kept.quality >= candidate.quality && !candidate.wired => {}
                _ => {
                    links.insert(key, candidate);
                }
            }
        }
    }
    joined.links = links.into_values().collect();
    for node in &mut joined.nodes {
        if node.nets.is_empty() {
            node.nets.push(0);
        }
        node.nets.sort_unstable();
    }

    // Each node's way to Irori: the fewest hops, over the better links first.
    let mut neighbours: BTreeMap<&str, Vec<(&str, f64)>> = BTreeMap::new();
    for link in &joined.links {
        neighbours
            .entry(&link.a)
            .or_default()
            .push((&link.b, link.strength()));
        neighbours
            .entry(&link.b)
            .or_default()
            .push((&link.a, link.strength()));
    }
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::from([HUB]);
    let mut queue = VecDeque::from([HUB]);
    while let Some(here) = queue.pop_front() {
        let mut next = neighbours.get(here).cloned().unwrap_or_default();
        next.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        for (other, _) in next {
            if seen.insert(other) {
                parent.insert(other.to_owned(), here.to_owned());
                queue.push_back(other);
            }
        }
    }
    for node in &mut joined.nodes {
        node.parent = parent.get(&node.id).cloned();
    }
    for link in &mut joined.links {
        if parent.get(&link.a) == Some(&link.b) {
            std::mem::swap(&mut link.a, &mut link.b);
        }
    }

    // Only the home's devices are drawn, with the ways into each network, and whatever else
    // carries a device's messages to Irori: a router nobody added still passes them on.
    let mut keep: BTreeSet<String> = BTreeSet::from([HUB.to_owned()]);
    for node in &joined.nodes {
        if node.kind == Kind::Coord {
            keep.insert(node.id.clone());
        }
        if node.device.is_some() {
            keep.extend(joined.route(&node.id));
        }
    }
    joined.nodes.retain(|node| keep.contains(&node.id));
    joined
        .links
        .retain(|link| keep.contains(&link.a) && keep.contains(&link.b));

    // Two networks of one name (the Demo's Matter and Matter's own) say whose each is.
    let names: Vec<String> = joined.networks.iter().map(|net| net.name.clone()).collect();
    for (net, owner) in joined.networks.iter_mut().zip(&owners) {
        let clash = names.iter().filter(|name| **name == net.name).count() > 1;
        if clash && !net.name.eq_ignore_ascii_case(owner) {
            net.name = format!("{} ({owner})", net.name);
        }
    }
    joined
}

/// What the home says about a device on the map, now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Standing {
    /// Every one of its entities is unavailable.
    pub offline: bool,
    /// It's a light, and it's on.
    pub lit: bool,
    pub battery: Option<String>,
    /// When it last said anything, in milliseconds since 1970.
    pub heard_ms: Option<f64>,
    pub room: Option<String>,
    pub model: Option<String>,
}

/// How every device on the map stands, by device: worked out in one pass over the home, once
/// each time the home changes, and read by the drawing and the panel alike. A busy home changes
/// several times a second, and looking each device's entities up one by one, for every
/// device, every time, was most of the page's work.
pub type Standings = BTreeMap<DeviceId, Standing>;

pub fn standings(home: &Home, joined: &Joined) -> Standings {
    let wanted: BTreeSet<&DeviceId> = joined
        .nodes
        .iter()
        .filter_map(|node| node.device.as_ref())
        .collect();
    let states: BTreeMap<&EntityId, &irori_types::EntityState> = home
        .states
        .iter()
        .map(|state| (&state.entity_id, state))
        .collect();
    // Per device: whether any entity has a state, whether all of those are unavailable.
    let mut seen: BTreeMap<&DeviceId, (bool, bool)> = BTreeMap::new();
    let mut all = Standings::new();
    for entity in &home.entities {
        let Some(device) = entity.device_id.as_ref().filter(|d| wanted.contains(d)) else {
            continue;
        };
        let now = all.entry(device.clone()).or_default();
        let Some(state) = states.get(&entity.id) else {
            continue;
        };
        let (any, unavailable) = seen.entry(device).or_insert((false, true));
        *any = true;
        *unavailable &= state.availability == Availability::Unavailable;
        if matches!(entity.capabilities, Capabilities::Light(_))
            && matches!(&state.state, Some(State::Light(light)) if light.on)
        {
            now.lit = true;
        }
        let heard = state.last_updated.as_second() as f64 * 1000.0;
        now.heard_ms = Some(now.heard_ms.map_or(heard, |was| was.max(heard)));
        if now.battery.is_none() {
            now.battery = crate::devices::battery_reading(entity, state);
        }
    }
    for (device, (any, unavailable)) in seen {
        if let Some(now) = all.get_mut(device) {
            now.offline = any && unavailable;
        }
    }
    for device in &home.devices {
        if !wanted.contains(&device.id) {
            continue;
        }
        let now = all.entry(device.id.clone()).or_default();
        now.room = home.area_of(device);
        now.model = match (&device.manufacturer, &device.model) {
            (Some(maker), Some(model)) if !model.starts_with(maker.as_str()) => {
                Some(format!("{maker} {model}"))
            }
            (_, Some(model)) => Some(model.clone()),
            (Some(maker), None) => Some(maker.clone()),
            _ => None,
        };
    }
    all
}

/// How one device stands, or nothing known.
fn stand(all: &Standings, device: Option<&DeviceId>) -> Standing {
    device
        .and_then(|device| all.get(device))
        .cloned()
        .unwrap_or_default()
}

/// The drawing `network_map.js` takes, as JSON.
fn drawing(joined: &Joined, all: &Standings) -> String {
    let nodes: Vec<serde_json::Value> = joined
        .nodes
        .iter()
        .map(|node| {
            let now = stand(all, node.device.as_ref());
            serde_json::json!({
                "id": node.id, "name": node.name, "kind": node.kind.word(),
                "net": node.net, "nets": node.nets, "parent": node.parent,
                "offline": now.offline, "on": now.lit,
            })
        })
        .collect();
    let links: Vec<serde_json::Value> = joined
        .links
        .iter()
        .map(|link| {
            serde_json::json!({
                "a": link.a, "b": link.b, "look": link.look(), "q": link.strength(),
                "label": link.reading, "net": link.net,
            })
        })
        .collect();
    let networks: Vec<serde_json::Value> = joined
        .networks
        .iter()
        .map(|net| serde_json::json!({ "name": net.name, "tone": net.tone }))
        .collect();
    serde_json::json!({ "networks": networks, "nodes": nodes, "links": links }).to_string()
}

/// "12 devices · 6 routers · 1 offline", for a network's card.
fn counts(joined: &Joined, index: usize, all: &Standings) -> String {
    let on: Vec<&Node> = joined
        .nodes
        .iter()
        .filter(|node| node.kind != Kind::Hub && node.kind != Kind::Coord)
        .filter(|node| node.nets.contains(&index))
        .collect();
    let routers = on.iter().filter(|node| node.kind == Kind::Router).count();
    let offline = on
        .iter()
        .filter(|node| stand(all, node.device.as_ref()).offline)
        .count();
    let mut words = vec![plural(on.len(), "device")];
    if routers > 0 {
        words.push(plural(routers, "router"));
    }
    if offline > 0 {
        words.push(format!("{offline} offline"));
    }
    words.join(" · ")
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// How long ago, from milliseconds: "26s ago", "4m ago", "3h ago".
pub fn ago(ms: f64) -> String {
    let s = (ms / 1000.0).max(0.0).round() as i64;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", (s as f64 / 60.0).round() as i64)
    } else if s < 86_400 {
        format!("{}h ago", (s as f64 / 3600.0).round() as i64)
    } else {
        format!("{}d ago", (s as f64 / 86_400.0).round() as i64)
    }
}

/// What a node is, in words, for the panel's heading.
fn kind_words(joined: &Joined, node: &Node) -> String {
    let net = |i: usize| {
        joined
            .networks
            .get(i)
            .map_or_else(String::new, |net| net.name.clone())
    };
    let nets = node
        .nets
        .iter()
        .map(|&i| net(i))
        .collect::<Vec<_>>()
        .join(" and ");
    match node.role {
        NetworkRole::Controller => "IroriOS hub".to_owned(),
        NetworkRole::Coordinator => format!("{} coordinator", net(node.net)),
        NetworkRole::BorderRouter => format!("Thread border router · {}", net(node.net)),
        NetworkRole::AccessPoint => format!("Wi-Fi access point · {nets}"),
        NetworkRole::Router => format!("{} · routes for others", net(node.net)),
        NetworkRole::EndDevice => format!("{} end device", net(node.net)),
        NetworkRole::SleepyEndDevice => format!("{} · sleepy", net(node.net)),
        NetworkRole::Cloud => "Vendor cloud · over the internet".to_owned(),
        NetworkRole::Device => match node.medium {
            Some(NetworkMedium::Ethernet) => format!("{} · wired", net(node.net)),
            Some(NetworkMedium::Bluetooth) => format!("{} · Bluetooth", net(node.net)),
            Some(NetworkMedium::Internet) => format!("{} · through the cloud", net(node.net)),
            Some(NetworkMedium::Ip) => format!("{} · on the network", net(node.net)),
            _ => format!("{} · Wi-Fi", net(node.net)),
        },
    }
}

/// The drawing on the page, and what it calls back.
struct Mounted {
    handle: JsValue,
    _on_select: Closure<dyn Fn(JsValue)>,
    _on_arranged: Closure<dyn Fn(JsValue)>,
    _on_zoom: Closure<dyn Fn(JsValue)>,
}

/// How far the map zooms in: four times the whole of it. It never zooms out past the whole.
const MOST_ZOOM: f64 = 4.0;

/// The speaker the sound button shows: sounding, and crossed out.
const SPEAKER: &str = r#"<path d="M4 9.5h3.2L12 5.6v12.8l-4.8-3.9H4z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M15.6 9.2a4 4 0 0 1 0 5.6M18.2 6.6a7.6 7.6 0 0 1 0 10.8" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#;
const SPEAKER_OFF: &str = r#"<path d="M4 9.5h3.2L12 5.6v12.8l-4.8-3.9H4z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="m16 9.5 5 5m0-5-5 5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#;

impl Mounted {
    fn call(&self, method: &str, arguments: &[JsValue]) {
        let Ok(function) = Reflect::get(&self.handle, &JsValue::from_str(method)) else {
            return;
        };
        if let Some(function) = function.dyn_ref::<Function>() {
            let _ = function.apply(&self.handle, &arguments.iter().collect::<Array>());
        }
    }
}

/// A line of the panel's message list.
#[derive(Debug, Clone, PartialEq)]
struct Logged {
    time: String,
    text: String,
    key: u64,
}

fn clock(ms: f64) -> String {
    let date = web_sys::js_sys::Date::new(&JsValue::from_f64(ms));
    format!(
        "{:02}:{:02}:{:02}",
        date.get_hours(),
        date.get_minutes(),
        date.get_seconds()
    )
}

fn now_ms() -> f64 {
    web_sys::js_sys::Date::now()
}

/// The network map, the Devices page's last tab. Only on the page while its tab is chosen, so
/// it asks for the networks as it opens and every few seconds after.
#[component]
pub fn NetworkMap() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let crate::Motion(motion) = expect_context::<crate::Motion>();
    let frame = NodeRef::<leptos::html::Div>::new();
    let mounted = StoredValue::new_local(None::<Mounted>);
    let trouble = RwSignal::new(None::<String>);

    let layout = RwSignal::new("mesh");
    let filter = RwSignal::new(None::<usize>);
    let labels = RwSignal::new(false);
    let query = RwSignal::new(String::new());
    let backdrop = RwSignal::new("grid");
    let sound = RwSignal::new(false);
    let selected = RwSignal::new(None::<String>);
    let log = RwSignal::new(Vec::<Logged>::new());
    let heard_at = RwSignal::new(Vec::<f64>::new());
    let clock_now = RwSignal::new(now_ms());
    let networks = RwSignal::new(None::<Vec<DrawnNetwork>>);
    // Whether any device has been dragged somewhere of its own, for the Reset button.
    let arranged = RwSignal::new(false);
    // How far the map is zoomed in, as the drawing says: 1 is the whole of it.
    let zoom = RwSignal::new(1.0_f64);

    // Asked for as the tab opens, and every few seconds while it's showing.
    let ask = move || {
        spawn_local(async move {
            match api::fetch_networks().await {
                Ok(fresh) => {
                    trouble.set(None);
                    if networks.get_untracked().as_ref() != Some(&fresh) {
                        networks.set(Some(fresh));
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    let ticking = set_interval_with_handle(ask, REFRESH).ok();
    // The panel's "26s ago"s, kept current.
    let seconds =
        set_interval_with_handle(move || clock_now.set(now_ms()), Duration::from_secs(1)).ok();
    on_cleanup(move || {
        if let Some(handle) = ticking {
            handle.clear();
        }
        if let Some(handle) = seconds {
            handle.clear();
        }
    });
    ask();

    let joined = Memo::new(move |_| {
        let drawn = networks.get().unwrap_or_default();
        live.home.with(|home| join(&drawn, home))
    });

    let standing = Memo::new(move |_| live.home.with(|home| joined.with(|j| standings(home, j))));

    let call = move |method: &'static str, arguments: Vec<JsValue>| {
        mounted.with_value(|mounted| {
            if let Some(map) = mounted {
                map.call(method, &arguments);
            }
        });
    };
    let moving = move || motion.get() && !crate::count::still();

    // The drawing goes in once its frame is on the page.
    Effect::new(move |_| {
        let Some(frame) = frame.get() else {
            return;
        };
        if mounted.with_value(Option::is_some) {
            return;
        }
        let mount = Reflect::get(&window(), &JsValue::from_str("iroriNetwork"))
            .ok()
            .and_then(|maps| Reflect::get(&maps, &JsValue::from_str("mount")).ok())
            .and_then(|mount| mount.dyn_into::<Function>().ok());
        let Some(mount) = mount else {
            trouble.set(Some("The map's drawing couldn't be loaded.".to_owned()));
            return;
        };
        let options = Object::new();
        let _ = Reflect::set(&options, &"layout".into(), &layout.get_untracked().into());
        let _ = Reflect::set(
            &options,
            &"backdrop".into(),
            &backdrop.get_untracked().into(),
        );
        let _ = Reflect::set(&options, &"motion".into(), &moving().into());
        let on_select = Closure::<dyn Fn(JsValue)>::new(move |id: JsValue| {
            let _ = selected.try_set(id.as_string());
        });
        let on_arranged = Closure::<dyn Fn(JsValue)>::new(move |moved: JsValue| {
            let _ = arranged.try_set(moved.is_truthy());
        });
        let _ = Reflect::set(&options, &"onArranged".into(), on_arranged.as_ref());
        let on_zoom = Closure::<dyn Fn(JsValue)>::new(move |k: JsValue| {
            if let Some(k) = k.as_f64() {
                let _ = zoom.try_set(k);
            }
        });
        let _ = Reflect::set(&options, &"onZoom".into(), on_zoom.as_ref());
        let element: &web_sys::Element = &frame;
        let arguments: Array = [
            JsValue::from(element.clone()),
            options.into(),
            on_select.as_ref().clone(),
        ]
        .into_iter()
        .collect();
        if let Ok(handle) = mount.apply(&JsValue::NULL, &arguments) {
            mounted.set_value(Some(Mounted {
                handle,
                _on_select: on_select,
                _on_arranged: on_arranged,
                _on_zoom: on_zoom,
            }));
        }
    });
    on_cleanup(move || {
        mounted.update_value(|mounted| {
            if let Some(map) = mounted.take() {
                map.call("destroy", &[]);
            }
        });
    });

    // What's drawn follows the home and its networks; everything else follows its control. The
    // drawing is only what the map shows (who's offline, which lights are on), so a message that
    // changes none of it, which is nearly every one, doesn't redraw the map.
    let drawn = Memo::new(move |_| joined.with(|joined| standing.with(|all| drawing(joined, all))));
    Effect::new(move |_| {
        // Nothing until the networks have been asked for: Irori alone, and then everything
        // arriving around it, would be a flash of an empty home.
        if networks.with(Option::is_none) {
            return;
        }
        let json = drawn.get();
        if frame.get().is_some() {
            call("setDrawing", vec![json.into()]);
        }
    });
    Effect::new(move |_| call("setMotion", vec![moving().into()]));
    Effect::new(move |_| call("setLayout", vec![layout.get().into()]));
    Effect::new(move |_| {
        call(
            "setFilter",
            vec![filter.get().map_or(JsValue::NULL, |i| (i as f64).into())],
        );
    });
    Effect::new(move |_| call("setLabels", vec![labels.get().into()]));
    Effect::new(move |_| call("setQuery", vec![query.get().into()]));
    Effect::new(move |_| call("setBackdrop", vec![backdrop.get().into()]));
    Effect::new(move |_| call("setSound", vec![sound.get().into()]));
    Effect::new(move |_| {
        let id = selected.get();
        call("select", vec![id.map_or(JsValue::NULL, JsValue::from)]);
    });

    // A state that arrives runs from its device to Irori, and is listed.
    Effect::new(move |_| {
        let Some((count, entity, commanded)) = live.heard.get() else {
            return;
        };
        let found = live.home.with_untracked(|home| {
            let device = home
                .entities
                .iter()
                .find(|e| e.id == entity)
                .and_then(|e| e.device_id.clone())?;
            joined.with_untracked(|joined| {
                joined
                    .device_node(&device)
                    .map(|node| (node.id.clone(), node.name.clone()))
            })
        });
        let Some((node, name)) = found else {
            return;
        };
        call("report", vec![node.as_str().into(), commanded.into()]);
        let now = now_ms();
        heard_at.update(|at| {
            at.retain(|&t| now - t < 60_000.0);
            at.push(now);
        });
        let text = if commanded {
            format!("Irori → {name} → Irori, by command")
        } else {
            format!("{name} → Irori · {}", entity_words(&entity))
        };
        log.update(|log| {
            log.insert(
                0,
                Logged {
                    time: clock(now),
                    text,
                    key: count,
                },
            );
            log.truncate(LOGGED);
        });
    });

    // The networks' own buttons: Zigbee's scan.
    let scans = Memo::new(move |_| {
        live.home.with(|home| {
            home.extensions
                .iter()
                .flat_map(|(id, extension)| {
                    extension
                        .actions
                        .iter()
                        .filter(|action| action.place == ActionPlace::Network)
                        .filter(|action| extension.available_actions.contains(&action.id))
                        .map(|action| {
                            (
                                id.clone(),
                                action.id.clone(),
                                action.label.clone(),
                                extension.open_actions.contains_key(&action.id),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
    });
    let scanning = move || scans.with(|scans| scans.iter().any(|(.., open)| *open));
    let scan = move || {
        call("scan", vec![]);
        for (extension, action, ..) in scans.get_untracked() {
            spawn_local(async move {
                if let Err(why) = api::trigger_action(&extension, &action).await {
                    trouble.set(Some(why));
                }
            });
        }
    };

    let segment = |label: &'static str, chosen: Signal<bool>, pick: Box<dyn Fn() + Send + Sync>| {
        view! {
            <button type="button" class="nm-seg" aria-pressed=move || chosen.get().to_string()
                on:click=move |_| pick()>{label}</button>
        }
    };

    view! {
        <div class="network-map">
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            <div class="nm-body">
            <div class="nm-main">
                <div class="nm-tools">
                    <div class="nm-segs" role="group" aria-label="Layout">
                        {segment("Mesh", Signal::derive(move || layout.get() == "mesh"), Box::new(move || layout.set("mesh")))}
                        {segment("Hops", Signal::derive(move || layout.get() == "rings"), Box::new(move || layout.set("rings")))}
                    </div>
                    <div class="nm-segs" role="group" aria-label="Networks">
                        <button type="button" class="nm-seg" aria-pressed=move || filter.get().is_none().to_string()
                            on:click=move |_| filter.set(None)>
                            <span class="nm-dot" data-tone="all"></span>"All"
                        </button>
                        {move || joined.with(|joined| joined.networks.iter().enumerate().map(|(i, net)| {
                            let tone = net.tone.clone();
                            let name = net.name.clone();
                            view! {
                                <button type="button" class="nm-seg"
                                    aria-pressed=move || (filter.get() == Some(i)).to_string()
                                    on:click=move |_| filter.set(Some(i))>
                                    <span class="nm-dot" data-tone=tone></span>{name}
                                </button>
                            }
                        }).collect_view())}
                    </div>
                    <button type="button" class="nm-toggle" aria-pressed=move || labels.get().to_string()
                        on:click=move |_| labels.update(|on| *on = !*on)>"Link labels"</button>
                    <span class="nm-gap"></span>
                    <input type="search" class="nm-find" placeholder="Find a device"
                        prop:value=move || query.get()
                        on:input=move |event| query.set(event_target_value(&event))
                        on:keydown=move |event: web_sys::KeyboardEvent| {
                            if event.key() == "Enter" {
                                let wanted = query.get_untracked().trim().to_lowercase();
                                if !wanted.is_empty() {
                                    let first = joined.with_untracked(|joined| joined.nodes.iter()
                                        .find(|node| node.name.to_lowercase().contains(&wanted))
                                        .map(|node| node.id.clone()));
                                    if first.is_some() {
                                        selected.set(first);
                                    }
                                }
                            } else if event.key() == "Escape" {
                                query.set(String::new());
                            }
                        } />
                </div>
                <div class="nm-view">
                    <div class="nm-stage" node_ref=frame></div>
                    // The floorplan's zoom: the strip for a little, the buttons for a jump, and
                    // the whole map again. A pinch, or Control and a scroll, zooms about the
                    // pointer; zoomed in, the empty map drags along.
                    <div class="zoom nm-zoom">
                        <crate::floorplan::zoom::ZoomWheel
                            scale=Signal::derive(move || zoom.get())
                            by=Callback::new(move |factor: f64| call("zoomBy", vec![factor.into()]))
                            least=1.0
                            most=MOST_ZOOM
                            spoken=Callback::new(|k: f64| format!("{k:.1} times the whole map"))
                        />
                        <div class="zoom-buttons">
                            <button type="button" aria-label="Zoom out"
                                on:click=move |_| call("zoomBy", vec![(1.0 / 1.25).into()])>"−"</button>
                            <button type="button" aria-label="Zoom in"
                                on:click=move |_| call("zoomBy", vec![1.25.into()])>"+"</button>
                            <button type="button" class="home" aria-label="Whole map"
                                title="The whole map" on:click=move |_| call("resetView", vec![])>
                                <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=crate::floorplan::HOME></svg>
                            </button>
                        </div>
                    </div>
                </div>
                {move || joined.with(|joined| joined.networks.is_empty()).then(|| view! {
                    <p class="muted small">
                        "No network is drawn yet. Zigbee, Matter, ESPHome and the Demo draw theirs \
                         once they're running and their devices are added (+ Add device)."
                    </p>
                })}
                <div class="nm-legend">
                    <span><i class="nm-key hub"></i>"Irori"</span>
                    <span><i class="nm-key coord"></i>"Coordinator, border router or access point"</span>
                    {move || joined.with(|joined| joined.networks.iter().map(|net| {
                        let tone = net.tone.clone();
                        let name = net.name.clone();
                        view! { <span><span class="nm-dot" data-tone=tone></span>{name}</span> }
                    }).collect_view())}
                    <span><i class="nm-key router"></i>"Routes for others"</span>
                    <span><i class="nm-key end"></i>"End device"</span>
                    <span><i class="nm-key sleepy"></i>"Sleepy"</span>
                    <span><i class="nm-line good"></i>"Good"</span>
                    <span><i class="nm-line weak"></i>"Weak"</span>
                    <span><i class="nm-line poor"></i>"Poor"</span>
                    <span class="nm-gap"></span>
                    <span>"Drag a device to move it · pinch to zoom"</span>
                </div>
                <div class="nm-tools">
                    {move || (!scans.with(Vec::is_empty)).then(|| view! {
                        <button type="button" class="nm-action" disabled=scanning
                            on:click=move |_| scan()>
                            {move || if scanning() { "Scanning…" } else { "Scan network" }}
                        </button>
                    })}
                    // Puts every dragged device back where the map would have it.
                    {move || arranged.get().then(|| view! {
                        <button type="button" class="nm-toggle" on:click=move |_| call("resetLayout", vec![])>
                            "Reset layout"
                        </button>
                    })}
                    <span class="nm-gap"></span>
                    <div class="nm-segs" role="group" aria-label="Backdrop">
                        {segment("Warp grid", Signal::derive(move || backdrop.get() == "grid"), Box::new(move || backdrop.set("grid")))}
                        {segment("Coverage", Signal::derive(move || backdrop.get() == "coverage"), Box::new(move || backdrop.set("coverage")))}
                    </div>
                    <button type="button" class="nm-toggle nm-icon" aria-pressed=move || sound.get().to_string()
                        aria-label="Sound" title=move || if sound.get() { "Sound on: each message is a note" } else { "Sound off" }
                        on:click=move |_| sound.update(|on| *on = !*on)>
                        <svg viewBox="0 0 24 24" aria-hidden="true"
                            inner_html=move || if sound.get() { SPEAKER } else { SPEAKER_OFF }></svg>
                    </button>
                </div>
            </div>
            <aside class="nm-panel">
                {move || match selected.get() {
                    Some(id) => chosen(id, joined, standing, selected, clock_now).into_any(),
                    None => overview(joined, standing, selected, heard_at, clock_now).into_any(),
                }}
                <div class="nm-block nm-log">
                    <h4 class="nm-eyebrow">"Messages"</h4>
                    {move || log.with(Vec::is_empty).then(|| view! {
                        <p class="muted small">"What the devices say appears here as it arrives."</p>
                    })}
                    <For each=move || log.get() key=|line| line.key let:line>
                        <div class="nm-log-line"><span class="nm-time">{line.time}</span><span>{line.text}</span></div>
                    </For>
                </div>
            </aside>
            </div>
        </div>
    }
}

/// An entity's id in a person's words, for the message list: `light.kitchen_ceiling` is
/// "light".
fn entity_words(entity: &EntityId) -> String {
    entity
        .as_str()
        .split('.')
        .next()
        .unwrap_or_default()
        .replace('_', " ")
}

/// The panel with nothing chosen: each network, how busy, and what needs looking at.
fn overview(
    joined: Memo<Joined>,
    standing: Memo<Standings>,
    selected: RwSignal<Option<String>>,
    heard_at: RwSignal<Vec<f64>>,
    clock_now: RwSignal<f64>,
) -> impl IntoView {
    // What the panel shows is worked out first and only drawn again when it changes: nearly
    // every message changes nothing here, and drawing the panel afresh on each made the browser
    // lay the page out again several times a second.
    let card_rows = Memo::new(move |_| {
        standing.with(|all| {
            joined.with(|joined| {
                joined
                    .networks
                    .iter()
                    .enumerate()
                    .map(|(i, net)| {
                        (
                            net.tone.clone(),
                            net.name.clone(),
                            net.detail.clone().unwrap_or_default(),
                            counts(joined, i, all),
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    let cards = move || {
        card_rows
            .get()
            .into_iter()
            .map(|(tone, name, detail, counted)| {
                view! {
                    <div class="nm-card">
                        <div class="nm-card-head">
                            <span class="nm-dot" data-tone=tone></span>
                            <strong>{name}</strong>
                            <span class="nm-gap"></span>
                            <span class="nm-mono">{detail}</span>
                        </div>
                        <div class="muted small">{counted}</div>
                    </div>
                }
            })
            .collect_view()
    };
    let last_scan = move || {
        joined.with(|joined| {
            joined
                .networks
                .iter()
                .filter_map(|net| net.scanned_at)
                .max()
                .map(|at| ago(clock_now.get() - at as f64 * 1000.0))
        })
    };
    let attention = Memo::new(move |_| {
        standing.with(|all| {
            joined.with(|joined| {
                let mut rows: Vec<(String, String, String, &'static str)> = Vec::new();
                for node in &joined.nodes {
                    if node.device.is_none() {
                        continue;
                    }
                    let now = stand(all, node.device.as_ref());
                    if now.offline {
                        let when = now
                            .heard_ms
                            .map(|at| format!(" · {}", ago(clock_now.get() - at)))
                            .unwrap_or_default();
                        rows.push((
                            node.id.clone(),
                            node.name.clone(),
                            format!("Offline{when}"),
                            "poor",
                        ));
                        continue;
                    }
                    if let Some(link) = joined.uplink(&node.id)
                        && matches!(link.look(), "weak" | "poor")
                    {
                        let reading = link.reading.clone().unwrap_or_default();
                        rows.push((
                            node.id.clone(),
                            node.name.clone(),
                            format!("Weak link · {reading}"),
                            "weak",
                        ));
                    }
                    if let Some(level) = now
                        .battery
                        .as_deref()
                        .and_then(|b| b.trim_end_matches('%').trim().parse::<f64>().ok())
                        .filter(|&level| level < 25.0)
                    {
                        rows.push((
                            node.id.clone(),
                            node.name.clone(),
                            format!("Battery {level:.0}%"),
                            "weak",
                        ));
                    }
                }
                rows
            })
        })
    });
    view! {
        <div>
        <div class="nm-block">{cards}</div>
        <div class="nm-figure">
            <span>"Messages, last minute"</span>
            <span class="nm-mono big">{move || {
                let now = clock_now.get();
                heard_at.with(|at| at.iter().filter(|&&t| now - t < 60_000.0).count())
            }}</span>
        </div>
        {move || last_scan().map(|when| view! {
            <div class="nm-figure"><span>"Last scan"</span><span>{when}</span></div>
        })}
        <div class="nm-block">
            <h4 class="nm-eyebrow">"Needs attention"</h4>
            {move || {
                let rows = attention.get();
                if rows.is_empty() {
                    view! { <p class="muted small">"Nothing: every device answers, and every link is good."</p> }.into_any()
                } else {
                    rows.into_iter().map(|(id, name, why, tone)| view! {
                        <button type="button" class="nm-row" on:click=move |_| selected.set(Some(id.clone()))>
                            <span class="nm-dot" data-tone=tone></span>
                            <span class="nm-grow">{name}</span>
                            <span class="muted small">{why}</span>
                        </button>
                    }).collect_view().into_any()
                }
            }}
        </div>
        </div>
    }
}

/// The panel for one node: what it is, how it's doing, and how it reaches Irori.
fn chosen(
    id: String,
    joined: Memo<Joined>,
    standing: Memo<Standings>,
    selected: RwSignal<Option<String>>,
    clock_now: RwSignal<f64>,
) -> impl IntoView {
    let id = StoredValue::new(id);
    let view = move || {
        let id = id.get_value();
        standing.with(|all| {
            joined.with(|joined| {
                let Some(node) = joined.node(&id).cloned() else {
                    return view! { <p class="muted small">"It isn't on the map any more."</p> }.into_any();
                };
                let net = joined.networks.get(node.net);
                let tone = if node.kind == Kind::Hub {
                    "all".to_owned()
                } else {
                    net.map_or_else(|| "all".to_owned(), |net| net.tone.clone())
                };
                let now = stand(all, node.device.as_ref());
                let sub = [now.room.clone(), now.model.clone().or_else(|| node.detail.clone())]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                let heard = now.heard_ms.map(|at| ago(clock_now.get() - at));
                let status = if node.kind == Kind::Hub {
                    "Running".to_owned()
                } else if now.offline {
                    format!("Offline{}", heard.map(|h| format!(" · last heard {h}")).unwrap_or_default())
                } else if node.device.is_some() {
                    format!("Online{}", heard.map(|h| format!(" · last heard {h}")).unwrap_or_default())
                } else {
                    "On the network".to_owned()
                };
                let uplink = joined.uplink(&node.id).cloned();
                let parent_name = node
                    .parent
                    .as_deref()
                    .and_then(|p| joined.node(p))
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let route: Vec<String> = joined
                    .route(&node.id)
                    .iter()
                    .filter_map(|hop| joined.node(hop).map(|n| n.name.clone()))
                    .collect();
                let hops = route.len().saturating_sub(1);
                let neighbours: Vec<(String, String, String, String)> = joined
                    .links
                    .iter()
                    .filter_map(|link| {
                        let other = joined.node(link.other(&node.id)?)?;
                        let tone = if other.kind == Kind::Hub {
                            "all".to_owned()
                        } else {
                            joined.networks.get(other.net).map_or_else(String::new, |n| n.tone.clone())
                        };
                        Some((
                            other.id.clone(),
                            other.name.clone(),
                            link.reading.clone().unwrap_or_else(|| link.word().to_owned()),
                            tone,
                        ))
                    })
                    .collect();
                let device = node.device.clone();
                let offline = now.offline;
                view! {
                    <div class="nm-block">
                        <div class="nm-head">
                            <div>
                                <div class="nm-eyebrow" data-tone=tone.clone()>{kind_words(joined, &node)}</div>
                                <h3 class="nm-title">{node.name.clone()}</h3>
                                {(!sub.is_empty()).then(|| view! { <div class="muted small">{sub}</div> })}
                            </div>
                            <button type="button" class="nm-close" aria-label="Close"
                                on:click=move |_| selected.set(None)>"×"</button>
                        </div>
                        <div class="nm-status"><span class="nm-dot" data-tone=if offline { "poor" } else { "ok" }></span>{status}</div>
                        {uplink.map(|link| {
                            let look = link.look();
                            let word = match look {
                                "wired" => "Wired".to_owned(),
                                "weak" | "poor" => format!("{}, a router between them would help", link.word()),
                                _ => link.word().to_owned(),
                            };
                            let width = format!("{:.0}%", link.strength() * 100.0);
                            view! {
                                <div class="nm-card">
                                    <div class="nm-card-head">
                                        <span class="muted">{format!("Link to {parent_name}")}</span>
                                        <span class="nm-gap"></span>
                                        <span class="nm-mono" data-look=look>{link.reading.clone().unwrap_or_default()}</span>
                                    </div>
                                    <div class="nm-bar"><div data-look=look style:width=width></div></div>
                                    <div class="muted small">{link.detail.clone().unwrap_or(word)}</div>
                                </div>
                            }
                        })}
                        {(hops > 0).then(|| view! {
                            <div>
                                <h4 class="nm-eyebrow">{format!("Route to Irori · {}", plural(hops, "hop"))}</h4>
                                <div class="nm-route">
                                    {route.into_iter().enumerate().map(|(i, name)| view! {
                                        {(i > 0).then(|| view! { <span class="muted">"→"</span> })}
                                        <span class="nm-hop">{name}</span>
                                    }).collect_view()}
                                </div>
                            </div>
                        })}
                        <div>
                            <h4 class="nm-eyebrow">"Neighbours"</h4>
                            {neighbours.into_iter().map(|(other, name, label, tone)| view! {
                                <button type="button" class="nm-row" on:click=move |_| selected.set(Some(other.clone()))>
                                    <span class="nm-dot" data-tone=tone></span>
                                    <span class="nm-grow">{name}</span>
                                    <span class="nm-mono small">{label}</span>
                                </button>
                            }).collect_view()}
                        </div>
                        {now.battery.filter(|_| !offline).map(|battery| view! {
                            <div class="nm-figure"><span>"Battery"</span><span>{battery}</span></div>
                        })}
                        {device.map(|device| view! {
                            <a class="nm-action" href=format!("/devices/{device}")>"Open device"</a>
                        })}
                    </div>
                }
                .into_any()
            })
        })
    };
    view! { <div>{view}</div> }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{Network, NetworkLink, NetworkNode};

    fn node(id: &str, role: NetworkRole) -> NetworkNode {
        NetworkNode {
            id: id.to_owned(),
            name: id.to_owned(),
            role,
            medium: None,
            device_unique_id: None,
            detail: None,
            host: None,
        }
    }

    /// A node that's one of the home's devices, by the same id.
    fn mine(id: &str, role: NetworkRole) -> NetworkNode {
        NetworkNode {
            device_unique_id: Some(id.parse().expect("valid")),
            ..node(id, role)
        }
    }

    fn link(from: &str, to: &str, quality: Option<LinkQuality>) -> NetworkLink {
        NetworkLink {
            from: from.to_owned(),
            to: to.to_owned(),
            medium: None,
            quality,
            reading: None,
            detail: None,
        }
    }

    fn drawn(
        extension: &str,
        name: &str,
        nodes: Vec<NetworkNode>,
        links: Vec<NetworkLink>,
    ) -> DrawnNetwork {
        DrawnNetwork {
            extension: extension.parse().expect("an id"),
            network: Network {
                name: Some(name.to_owned()),
                nodes,
                links,
                ..Network::default()
            },
        }
    }

    #[test]
    fn every_network_meets_at_one_irori_and_a_shared_access_point_is_one() {
        let zigbee = drawn(
            "zigbee",
            "Zigbee",
            vec![
                node("irori", NetworkRole::Controller),
                node("zc", NetworkRole::Coordinator),
                mine("lamp", NetworkRole::Router),
                mine("motion", NetworkRole::SleepyEndDevice),
            ],
            vec![
                link("zc", "irori", None),
                link("lamp", "zc", Some(LinkQuality::Excellent)),
                link("motion", "lamp", Some(LinkQuality::Fair)),
                link("motion", "zc", Some(LinkQuality::Poor)),
            ],
        );
        let matter = drawn(
            "matter",
            "Matter",
            vec![
                node("irori", NetworkRole::Controller),
                node("wifi", NetworkRole::AccessPoint),
                mine("plug", NetworkRole::Device),
            ],
            vec![
                link("wifi", "irori", None),
                link("plug", "wifi", Some(LinkQuality::Good)),
            ],
        );
        let esphome = drawn(
            "esphome",
            "ESPHome",
            vec![
                node("irori", NetworkRole::Controller),
                node("wifi", NetworkRole::AccessPoint),
                mine("door", NetworkRole::Device),
            ],
            vec![link("wifi", "irori", None), link("door", "wifi", None)],
        );
        let home = Home {
            devices: vec![
                thing("lamp"),
                thing("motion"),
                thing_of("matter", "plug"),
                thing_of("esphome", "door"),
            ],
            ..Home::default()
        };
        let joined = join(&[zigbee, matter, esphome], &home);
        assert_eq!(
            joined.nodes.iter().filter(|n| n.kind == Kind::Hub).count(),
            1
        );
        let wifi = joined.node("ap:wifi").expect("one Wi-Fi");
        assert_eq!(wifi.nets, vec![1, 2]);
        assert_eq!(joined.networks[0].tone, "zigbee");
        assert_eq!(joined.networks[2].tone, "esphome");
        // The motion sensor's way home is the shortest: straight to the coordinator, even though
        // that link is poor, and its uplink says so.
        assert_eq!(
            joined.route("zigbee#0:motion"),
            vec!["zigbee#0:motion", "zigbee#0:zc", "irori"]
        );
        assert_eq!(
            joined.uplink("zigbee#0:motion").map(Link::look),
            Some("poor")
        );
        // Tree links are drawn from the end nearer Irori.
        let lamp = joined.link("zigbee#0:lamp", "zigbee#0:zc").expect("linked");
        assert_eq!(
            (lamp.a.as_str(), lamp.b.as_str()),
            ("zigbee#0:zc", "zigbee#0:lamp")
        );
        assert_eq!(
            joined.route("esphome#0:door"),
            vec!["esphome#0:door", "ap:wifi", "irori"]
        );
    }

    fn thing(id: &str) -> irori_types::Device {
        thing_of("zigbee", id)
    }

    fn thing_of(protocol: &str, id: &str) -> irori_types::Device {
        irori_types::Device {
            id: id.parse().expect("valid"),
            protocol: protocol.parse().expect("valid"),
            unique_id: id.parse().expect("valid"),
            name: id.parse().expect("valid"),
            description: None,
            manufacturer: Some("IKEA".to_owned()),
            model: Some("TRADFRI bulb".to_owned()),
            sw_version: None,
            hw_version: None,
            serial_number: None,
            area_id: None,
            suggested_area: None,
            via_device_id: None,
        }
    }

    fn part(id: &str, device: &str, capabilities: Capabilities) -> irori_types::Entity {
        irori_types::Entity {
            id: id.parse().expect("valid"),
            protocol: "zigbee".parse().expect("valid"),
            unique_id: id.replace('.', "-").parse().expect("valid"),
            name: id.replace('.', " ").parse().expect("valid"),
            device_id: Some(device.parse().expect("valid")),
            area_id: None,
            capabilities,
            entity_category: None,
        }
    }

    fn reading(
        id: &str,
        at: &str,
        availability: Availability,
        state: State,
    ) -> irori_types::EntityState {
        let at: irori_types::Timestamp = at.parse().expect("a valid timestamp");
        irori_types::EntityState {
            entity_id: id.parse().expect("valid"),
            availability,
            state: Some(state),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: irori_types::Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9".parse().expect("valid"),
                parent_id: None,
                origin: irori_types::Origin::System,
            },
        }
    }

    /// Worked out in one pass for every device on the map: a lamp that's on, with the newest
    /// of its readings; a door whose every entity is unavailable; and a device the map doesn't
    /// show, left out.
    #[test]
    fn every_device_on_the_map_stands_as_its_entities_say() {
        let lamp_on = State::Light(irori_types::LightState {
            on: true,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        });
        let switch = |on| State::Switch(irori_types::SwitchState { on });
        let home = Home {
            devices: vec![thing("lamp"), thing("door"), thing("elsewhere")],
            entities: vec![
                part(
                    "light.lamp",
                    "lamp",
                    Capabilities::Light(Default::default()),
                ),
                part(
                    "switch.lamp_identify",
                    "lamp",
                    Capabilities::Switch(Default::default()),
                ),
                part(
                    "switch.door",
                    "door",
                    Capabilities::Switch(Default::default()),
                ),
                part(
                    "switch.elsewhere",
                    "elsewhere",
                    Capabilities::Switch(Default::default()),
                ),
            ],
            states: vec![
                reading(
                    "light.lamp",
                    "2026-10-23T10:00:00Z",
                    Availability::Available,
                    lamp_on,
                ),
                reading(
                    "switch.lamp_identify",
                    "2026-10-23T10:05:00Z",
                    Availability::Available,
                    switch(false),
                ),
                reading(
                    "switch.door",
                    "2026-10-23T09:00:00Z",
                    Availability::Unavailable,
                    switch(false),
                ),
                reading(
                    "switch.elsewhere",
                    "2026-10-23T09:00:00Z",
                    Availability::Unavailable,
                    switch(false),
                ),
            ],
            ..Home::default()
        };
        let mut lamp = node("lamp", NetworkRole::Router);
        lamp.device_unique_id = Some("lamp".parse().expect("valid"));
        let mut door = node("door", NetworkRole::EndDevice);
        door.device_unique_id = Some("door".parse().expect("valid"));
        let map = drawn(
            "zigbee",
            "Zigbee",
            vec![node("irori", NetworkRole::Controller), lamp, door],
            vec![link("lamp", "irori", None), link("door", "lamp", None)],
        );
        let joined = join(&[map], &home);
        let all = standings(&home, &joined);

        let lamp = &all[&"lamp".parse::<DeviceId>().expect("valid")];
        assert!(lamp.lit && !lamp.offline);
        assert_eq!(lamp.model.as_deref(), Some("IKEA TRADFRI bulb"));
        let newest: irori_types::Timestamp = "2026-10-23T10:05:00Z".parse().expect("valid");
        assert_eq!(lamp.heard_ms, Some(newest.as_second() as f64 * 1000.0));
        let door = &all[&"door".parse::<DeviceId>().expect("valid")];
        assert!(door.offline && !door.lit);
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(counts(&joined, 0, &all), "2 devices · 1 router · 1 offline");
    }

    #[test]
    fn times_read_the_way_a_person_says_them() {
        assert_eq!(ago(26_000.0), "26s ago");
        assert_eq!(ago(26.0 * 60_000.0), "26m ago");
        assert_eq!(ago(3.0 * 3_600_000.0), "3h ago");
    }

    /// What nobody has added stays off the map: a device found but not added, and a network
    /// with none of the home's devices on it. A router nobody added stays when it carries an
    /// added device's messages. Two networks named alike say whose each is.
    #[test]
    fn only_the_homes_devices_are_drawn_and_networks_named_alike_say_whose() {
        let zigbee = drawn(
            "zigbee",
            "Zigbee",
            vec![
                node("irori", NetworkRole::Controller),
                node("zc", NetworkRole::Coordinator),
                node("relay", NetworkRole::Router),
                mine("sensor", NetworkRole::EndDevice),
                node("stranger", NetworkRole::EndDevice),
            ],
            vec![
                link("zc", "irori", None),
                link("relay", "zc", Some(LinkQuality::Good)),
                link("sensor", "relay", Some(LinkQuality::Good)),
                link("stranger", "zc", Some(LinkQuality::Good)),
            ],
        );
        let demo_matter = drawn(
            "demo",
            "Matter",
            vec![
                node("irori", NetworkRole::Controller),
                node("br", NetworkRole::BorderRouter),
                mine("lamp", NetworkRole::Router),
            ],
            vec![link("br", "irori", None), link("lamp", "br", None)],
        );
        let demo_esphome = drawn(
            "demo",
            "ESPHome",
            vec![
                node("irori", NetworkRole::Controller),
                node("wifi", NetworkRole::AccessPoint),
                mine("garage", NetworkRole::Device),
            ],
            vec![link("wifi", "irori", None), link("garage", "wifi", None)],
        );
        let matter = drawn(
            "matter",
            "Matter",
            vec![
                node("irori", NetworkRole::Controller),
                node("br", NetworkRole::BorderRouter),
                mine("plug", NetworkRole::Router),
            ],
            vec![link("br", "irori", None), link("plug", "br", None)],
        );
        let home = Home {
            devices: vec![
                thing("sensor"),
                thing_of("demo", "lamp"),
                thing_of("matter", "plug"),
            ],
            ..Home::default()
        };
        let joined = join(&[zigbee, demo_matter, demo_esphome, matter], &home);

        let names: Vec<&str> = joined.networks.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Zigbee", "Matter (demo)", "Matter"]);
        assert!(
            joined.node("zigbee#0:relay").is_some(),
            "it carries the sensor"
        );
        assert!(
            joined.node("zigbee#0:stranger").is_none(),
            "nobody added it"
        );
        assert!(
            joined.node("ap:wifi").is_none(),
            "its network had nothing of the home's"
        );
        assert!(
            joined
                .links
                .iter()
                .all(|l| joined.node(&l.a).is_some() && joined.node(&l.b).is_some())
        );
        assert_eq!(
            joined.route("zigbee#0:sensor"),
            vec!["zigbee#0:sensor", "zigbee#0:relay", "zigbee#0:zc", "irori"]
        );
        // Its ids hold whichever of the Demo's networks were left out, so a place it was
        // dragged to is found again.
        assert_eq!(joined.node("demo#0:lamp").map(|n| n.net), Some(1));
        assert_eq!(joined.node("matter#0:plug").map(|n| n.net), Some(2));
    }

    /// A Nest Hub is a Cast device and a Thread border router, on one host: one node, the
    /// home's device, drawn as the way into Thread.
    #[test]
    fn one_box_two_protocols_see_is_one_node() {
        let host = irori_types::host_name("fuchsia-dce5-5ba4-40db.local.");
        let cast = drawn(
            "cast",
            "Wi-Fi",
            vec![
                node("irori", NetworkRole::Controller),
                node("wifi", NetworkRole::AccessPoint),
                NetworkNode {
                    host: host.clone(),
                    ..mine("hub", NetworkRole::Device)
                },
            ],
            vec![link("wifi", "irori", None), link("hub", "wifi", None)],
        );
        let matter = drawn(
            "matter",
            "Matter",
            vec![
                node("irori", NetworkRole::Controller),
                NetworkNode {
                    host,
                    ..node("br-1", NetworkRole::BorderRouter)
                },
                mine("lock", NetworkRole::SleepyEndDevice),
            ],
            vec![
                link("br-1", "irori", None),
                link("lock", "br-1", Some(LinkQuality::Good)),
            ],
        );
        let mut hub = thing_of("cast", "hub");
        hub.name = "Kitchen Display".parse().expect("valid");
        let home = Home {
            devices: vec![hub, thing_of("matter", "lock")],
            ..Home::default()
        };
        let joined = join(&[cast, matter], &home);
        assert!(joined.node("matter#0:br-1").is_none(), "drawn once");
        let both = joined.node("cast#0:hub").expect("the hub");
        assert_eq!(both.kind, Kind::Coord);
        assert_eq!(both.name, "Kitchen Display");
        assert_eq!(both.device, Some("hub".parse().expect("valid")));
        assert_eq!(both.nets, vec![0, 1]);
        assert_eq!(
            joined.route("matter#0:lock"),
            vec!["matter#0:lock", "cast#0:hub", "irori"]
        );
    }

    #[test]
    fn a_network_through_a_vendors_cloud_is_drawn_in_the_clouds_colour() {
        let home = Home {
            devices: vec![thing_of("switchbot", "meter")],
            ..Home::default()
        };
        let mut cloud = node("cloud", NetworkRole::Cloud);
        cloud.medium = Some(NetworkMedium::Internet);
        let mut meter = mine("meter", NetworkRole::Device);
        meter.medium = Some(NetworkMedium::Bluetooth);
        let joined = join(
            &[drawn(
                "switchbot",
                "SwitchBot",
                vec![node("irori", NetworkRole::Controller), cloud, meter],
                vec![link("cloud", "irori", None), link("meter", "cloud", None)],
            )],
            &home,
        );
        assert_eq!(joined.networks[0].tone, "cloud");
        let cloud = joined
            .nodes
            .iter()
            .find(|n| n.role == NetworkRole::Cloud)
            .expect("drawn");
        assert_eq!(cloud.kind, Kind::Coord);
        assert_eq!(
            kind_words(&joined, cloud),
            "Vendor cloud · over the internet"
        );
        let meter = joined
            .nodes
            .iter()
            .find(|n| n.device.is_some())
            .expect("drawn");
        assert!(kind_words(&joined, meter).ends_with("· Bluetooth"));
    }

    #[test]
    fn an_unnamed_network_takes_a_spare_colour() {
        let mut spare = 0;
        assert_eq!(tone("Z-Wave", &mut spare), "other-1");
        assert_eq!(tone("Bluetooth", &mut spare), "other-2");
        assert_eq!(tone("Home Wi-Fi", &mut spare), "wifi");
    }
}
