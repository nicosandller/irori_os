//! Enforces the dependency rules from ROADMAP §2.1 over `cargo metadata`:
//!
//! - `irori-core` and `irori-rules` have no integration crate, no AI crate, and no protocol
//!   library anywhere in their (non-dev) dependency tree.
//! - Crates may only depend on the workspace crates their layer allows (e.g. integrations
//!   depend only on `irori-types` and `irori-integration`).

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use anyhow::{Context as _, bail};
use serde::Deserialize;

/// Crates whose whole dependency tree must stay protocol-free.
const PROTOCOL_FREE: &[&str] = &["irori-core", "irori-rules"];

/// Never allowed in a protocol-free crate's tree. Prefix match with a trailing `*`.
const BANNED_IN_PROTOCOL_FREE: &[&str] = &[
    "irori-int-*",
    "irori-assist",
    // Protocol libraries belong in integrations.
    "rumqttc",
    "rumqttd",
    "paho-mqtt",
    "rs-matter",
    "serialport",
    "tokio-serial",
];

/// Allowed direct workspace dependencies per crate (prefix match with a trailing `*`).
/// Crates not listed here are unrestricted.
const ALLOWED_WORKSPACE_DEPS: &[(&str, &[&str])] = &[
    ("irori-types", &[]),
    ("irori-integration", &["irori-types"]),
    ("irori-rules", &["irori-types"]),
    ("irori-client", &["irori-types"]),
    ("irori-int-*", &["irori-types", "irori-integration"]),
    ("irori-assist", &["irori-types", "irori-client"]),
];

pub fn run() -> anyhow::Result<()> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--locked",
        ])
        .output()
        .context("failed to run `cargo metadata`")?;
    if !output.status.success() {
        bail!(
            "`cargo metadata` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    let graph = Graph::from_metadata(&metadata)?;

    let violations = check(&graph);
    if violations.is_empty() {
        println!(
            "dependency rules OK ({} workspace crates checked)",
            graph.workspace.len()
        );
        Ok(())
    } else {
        for v in &violations {
            eprintln!("error: {v}");
        }
        bail!("{} dependency rule violation(s)", violations.len())
    }
}

/// The resolved dependency graph, keyed by package id so that two versions of the same crate
/// stay separate nodes. Dev-dependencies are already removed.
#[derive(Debug, Default)]
struct Graph {
    /// Package id -> crate name.
    names: BTreeMap<String, String>,
    /// Package ids of workspace members.
    workspace: BTreeSet<String>,
    /// Package id -> package ids of its non-dev dependencies.
    edges: BTreeMap<String, BTreeSet<String>>,
}

impl Graph {
    fn from_metadata(metadata: &Metadata) -> anyhow::Result<Self> {
        let mut graph = Graph {
            names: metadata
                .packages
                .iter()
                .map(|p| (p.id.clone(), p.name.clone()))
                .collect(),
            workspace: metadata.workspace_members.iter().cloned().collect(),
            ..Graph::default()
        };
        let resolve = metadata
            .resolve
            .as_ref()
            .context("`cargo metadata` returned no resolve graph")?;
        for node in &resolve.nodes {
            let deps = graph.edges.entry(node.id.clone()).or_default();
            for dep in &node.deps {
                let non_dev = dep
                    .dep_kinds
                    .iter()
                    .any(|k| k.kind.as_deref() != Some("dev"));
                if non_dev {
                    deps.insert(dep.pkg.clone());
                }
            }
        }
        Ok(graph)
    }

    fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.names.get(id).map_or(id, String::as_str)
    }

    /// Returns the crate names on the path from `from` to the first reachable crate matching
    /// `pattern`.
    fn find_path(&self, from: &str, pattern: &str) -> Option<Vec<String>> {
        let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
        let mut queue = std::collections::VecDeque::from([from]);
        let mut seen = BTreeSet::from([from]);
        while let Some(current) = queue.pop_front() {
            if current != from && matches(pattern, self.name(current)) {
                let mut path = vec![self.name(current).to_owned()];
                let mut node = current;
                while let Some(p) = parent.get(node) {
                    path.push(self.name(p).to_owned());
                    node = p;
                }
                path.reverse();
                return Some(path);
            }
            for dep in self.edges.get(current).into_iter().flatten() {
                if seen.insert(dep) {
                    parent.insert(dep, current);
                    queue.push_back(dep);
                }
            }
        }
        None
    }
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

fn check(graph: &Graph) -> Vec<String> {
    let mut violations = Vec::new();

    for id in &graph.workspace {
        let krate = graph.name(id);
        if !PROTOCOL_FREE.contains(&krate) {
            continue;
        }
        for banned in BANNED_IN_PROTOCOL_FREE {
            if let Some(path) = graph.find_path(id, banned) {
                violations.push(format!(
                    "`{krate}` must not depend on `{banned}`: {}",
                    path.join(" -> ")
                ));
            }
        }
    }

    for id in &graph.workspace {
        let krate = graph.name(id);
        let Some((pattern, allowed)) = ALLOWED_WORKSPACE_DEPS
            .iter()
            .find(|(pattern, _)| matches(pattern, krate))
        else {
            continue;
        };
        for dep_id in graph.edges.get(id).into_iter().flatten() {
            let dep = graph.name(dep_id);
            if graph.workspace.contains(dep_id) && !allowed.iter().any(|a| matches(a, dep)) {
                violations.push(format!(
                    "`{krate}` may only depend on workspace crates {allowed:?} (rule for `{pattern}`), but depends on `{dep}`"
                ));
            }
        }
    }

    violations
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    deps: Vec<NodeDep>,
}

#[derive(Debug, Deserialize)]
struct NodeDep {
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
struct DepKind {
    kind: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a graph from `name@version` ids; a bare name is version 0.
    fn graph(workspace: &[&str], edges: &[(&str, &str)]) -> Graph {
        let id = |s: &str| {
            if s.contains('@') {
                s.to_owned()
            } else {
                format!("{s}@0")
            }
        };
        let mut g = Graph {
            workspace: workspace.iter().map(|s| id(s)).collect(),
            ..Graph::default()
        };
        for (from, to) in edges {
            for node in [from, to] {
                let name = node.split('@').next().unwrap_or(node).to_owned();
                g.names.insert(id(node), name);
            }
            g.edges.entry(id(from)).or_default().insert(id(to));
        }
        for member in workspace {
            let name = member.split('@').next().unwrap_or(member).to_owned();
            g.names.insert(id(member), name);
        }
        g
    }

    #[test]
    fn clean_graph_passes() {
        let g = graph(
            &[
                "irori-core",
                "irori-types",
                "irori-integration",
                "irori-int-mqtt",
            ],
            &[
                ("irori-core", "irori-types"),
                ("irori-core", "irori-integration"),
                ("irori-integration", "irori-types"),
                ("irori-int-mqtt", "irori-integration"),
                ("irori-int-mqtt", "rumqttc"),
            ],
        );
        assert_eq!(check(&g), Vec::<String>::new());
    }

    #[test]
    fn transitive_protocol_dependency_in_core_is_reported_with_path() {
        let g = graph(
            &["irori-core", "irori-integration"],
            &[
                ("irori-core", "irori-integration"),
                ("irori-integration", "some-helper"),
                ("some-helper", "rumqttc"),
            ],
        );
        let violations = check(&g);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("irori-core -> irori-integration -> some-helper -> rumqttc")
        );
    }

    #[test]
    fn integration_depending_on_core_is_reported() {
        let g = graph(
            &["irori-int-demo", "irori-core"],
            &[("irori-int-demo", "irori-core")],
        );
        let violations = check(&g);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("`irori-int-demo`"));
    }

    #[test]
    fn versions_of_the_same_crate_are_not_merged() {
        // Core reaches helper v1; only helper v2 (used elsewhere) depends on rumqttc.
        let g = graph(
            &["irori-core", "irori-int-mqtt"],
            &[
                ("irori-core", "helper@1"),
                ("irori-int-mqtt", "helper@2"),
                ("helper@2", "rumqttc"),
            ],
        );
        assert_eq!(check(&g), Vec::<String>::new());
    }
}
