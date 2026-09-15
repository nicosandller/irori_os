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

/// A name-keyed dependency graph with dev-dependencies already removed.
#[derive(Debug, Default)]
struct Graph {
    workspace: BTreeSet<String>,
    edges: BTreeMap<String, BTreeSet<String>>,
}

impl Graph {
    fn from_metadata(metadata: &Metadata) -> anyhow::Result<Self> {
        let names: BTreeMap<&str, &str> = metadata
            .packages
            .iter()
            .map(|p| (p.id.as_str(), p.name.as_str()))
            .collect();
        let name_of = |id: &str| {
            names
                .get(id)
                .map(|n| (*n).to_owned())
                .with_context(|| format!("unknown package id {id}"))
        };

        let mut graph = Graph::default();
        for id in &metadata.workspace_members {
            graph.workspace.insert(name_of(id)?);
        }
        let resolve = metadata
            .resolve
            .as_ref()
            .context("`cargo metadata` returned no resolve graph")?;
        for node in &resolve.nodes {
            let deps = graph.edges.entry(name_of(&node.id)?).or_default();
            for dep in &node.deps {
                let non_dev = dep
                    .dep_kinds
                    .iter()
                    .any(|k| k.kind.as_deref() != Some("dev"));
                if non_dev {
                    deps.insert(name_of(&dep.pkg)?);
                }
            }
        }
        Ok(graph)
    }

    /// Returns the path from `from` to the first reachable crate matching `pattern`.
    fn find_path(&self, from: &str, pattern: &str) -> Option<Vec<String>> {
        let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
        let mut queue = std::collections::VecDeque::from([from]);
        let mut seen = BTreeSet::from([from]);
        while let Some(current) = queue.pop_front() {
            if current != from && matches(pattern, current) {
                let mut path = vec![current.to_owned()];
                let mut node = current;
                while let Some(p) = parent.get(node) {
                    path.push((*p).to_owned());
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

    for krate in PROTOCOL_FREE {
        for banned in BANNED_IN_PROTOCOL_FREE {
            if let Some(path) = graph.find_path(krate, banned) {
                violations.push(format!(
                    "`{krate}` must not depend on `{banned}`: {}",
                    path.join(" -> ")
                ));
            }
        }
    }

    for krate in &graph.workspace {
        let Some((pattern, allowed)) = ALLOWED_WORKSPACE_DEPS
            .iter()
            .find(|(pattern, _)| matches(pattern, krate))
        else {
            continue;
        };
        for dep in graph.edges.get(krate).into_iter().flatten() {
            if graph.workspace.contains(dep) && !allowed.iter().any(|a| matches(a, dep)) {
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

    fn graph(workspace: &[&str], edges: &[(&str, &str)]) -> Graph {
        let mut g = Graph {
            workspace: workspace.iter().map(|s| (*s).to_owned()).collect(),
            ..Graph::default()
        };
        for (from, to) in edges {
            g.edges
                .entry((*from).to_owned())
                .or_default()
                .insert((*to).to_owned());
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
}
