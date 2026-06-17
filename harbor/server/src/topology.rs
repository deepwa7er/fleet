//! Graph topology: machines, per-project placement, and the typed edges between
//! projects. Read from a single `topology.yaml` at the secondbrain root and
//! served as part of the API snapshot so the extension can draw the graph view.
//!
//! This is the structured form of what used to live only as prose in the
//! portfolio's "integration map" — harbor reads declared relationships, it does
//! not infer them from prose or from the free-text `location` field.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A host that appears as a machine node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    /// Stable identifier referenced by placements (e.g. `deepwa7er`).
    pub id: String,
    /// Human label for the node (e.g. `deepwa7er VPS`).
    pub label: String,
    /// Free-form role used to style the node (e.g. `server`, `dev`).
    #[serde(default)]
    pub role: Option<String>,
}

/// Where a project's service runs. `unit`, when set, links the runtime to a
/// specific systemd unit so the graph can show live lighthouse health for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunsOn {
    /// Machine id this service runs on.
    pub machine: String,
    /// Optional systemd unit (e.g. `ferry.service`) to bind live health to.
    /// When omitted, no health is shown — harbor never guesses the unit name.
    #[serde(default)]
    pub unit: Option<String>,
}

/// A project's placement: the machine its codebase lives on and where, if
/// anywhere, its service runs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Placement {
    /// Machine id the source checkout lives on.
    #[serde(default)]
    pub codebase: Option<String>,
    /// Machines (and optional units) the project's service runs on. Empty for
    /// projects that aren't deployed.
    #[serde(default)]
    pub runs_on: Vec<RunsOn>,
}

/// A typed, directional relationship between two projects (e.g. `monitors`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// Free-form relationship label, styled by the extension.
    pub kind: String,
}

/// The whole graph topology, as declared in `topology.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Topology {
    #[serde(default)]
    pub machines: Vec<Machine>,
    /// Per-project placement, keyed by the project's `name`.
    #[serde(default)]
    pub placements: HashMap<String, Placement>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// Read `<source_dir>/topology.yaml`, if present.
///
/// A missing file is normal (the graph view simply has nothing to draw) and
/// returns `None`. A present-but-unparseable file is logged and also returns
/// `None` rather than failing the snapshot — one bad file should never blank the
/// dashboard, the same tolerance the frontmatter reader uses.
pub fn load(source_dir: &Path) -> Option<Topology> {
    let path = source_dir.join("topology.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "no topology.yaml; graph view disabled");
            return None;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "reading topology.yaml; graph view disabled");
            return None;
        }
    };
    match serde_yaml::from_str::<Topology>(&text) {
        Ok(topology) => Some(topology),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "parsing topology.yaml; graph view disabled");
            None
        }
    }
}

impl Topology {
    /// Prune dangling references against the known set of project names, so the
    /// served graph only ever points at nodes that exist. Every drop is logged.
    ///
    /// - A placement whose key is not a known project is dropped whole.
    /// - A `codebase` or `runs_on` pointing at an unknown machine id is cleared.
    /// - An edge with an endpoint that is not a known project is dropped.
    pub fn validate(&mut self, projects: &HashSet<String>) {
        let machines: HashSet<&str> = self.machines.iter().map(|m| m.id.as_str()).collect();

        self.placements.retain(|name, placement| {
            if !projects.contains(name) {
                tracing::warn!(project = %name, "topology: placement for unknown project; dropping");
                return false;
            }
            if let Some(codebase) = &placement.codebase
                && !machines.contains(codebase.as_str())
            {
                tracing::warn!(project = %name, machine = %codebase, "topology: codebase on unknown machine; clearing");
                placement.codebase = None;
            }
            placement.runs_on.retain(|run| {
                let known = machines.contains(run.machine.as_str());
                if !known {
                    tracing::warn!(project = %name, machine = %run.machine, "topology: runs_on unknown machine; dropping");
                }
                known
            });
            true
        });

        self.edges.retain(|edge| {
            let known = projects.contains(&edge.from) && projects.contains(&edge.to);
            if !known {
                tracing::warn!(from = %edge.from, to = %edge.to, "topology: edge with unknown endpoint; dropping");
            }
            known
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Topology {
        let yaml = r"
machines:
  - id: deepwa7er
    label: deepwa7er VPS
    role: server
  - id: macbook
    label: MacBook
    role: dev
placements:
  ferry:
    codebase: macbook
    runs_on:
      - { machine: deepwa7er, unit: ferry.service }
  buoy:
    codebase: macbook
edges:
  - { from: lighthouse, to: ferry, kind: monitors }
";
        serde_yaml::from_str(yaml).expect("sample parses")
    }

    #[test]
    fn parses_machines_placements_edges() {
        let t = sample();
        assert_eq!(t.machines.len(), 2);
        assert_eq!(t.placements["ferry"].codebase.as_deref(), Some("macbook"));
        assert_eq!(t.placements["ferry"].runs_on[0].unit.as_deref(), Some("ferry.service"));
        assert!(t.placements["buoy"].runs_on.is_empty());
        assert_eq!(t.edges.len(), 1);
    }

    #[test]
    fn validate_prunes_dangling_references() {
        let mut t = sample();
        // Reference a project that isn't in the known set, plus a bad machine.
        t.placements.insert(
            "ghost".into(),
            Placement { codebase: Some("nowhere".into()), runs_on: vec![] },
        );
        t.placements.get_mut("ferry").unwrap().runs_on.push(RunsOn {
            machine: "nope".into(),
            unit: None,
        });
        t.edges.push(Edge { from: "ferry".into(), to: "ghost".into(), kind: "x".into() });

        let projects: HashSet<String> =
            ["ferry", "buoy", "lighthouse"].iter().map(|s| (*s).to_string()).collect();
        t.validate(&projects);

        assert!(!t.placements.contains_key("ghost"), "unknown-project placement dropped");
        assert_eq!(t.placements["ferry"].runs_on.len(), 1, "bad runs_on machine dropped");
        assert_eq!(t.edges.len(), 1, "edge to unknown project dropped");
        assert_eq!(t.edges[0].to, "ferry");
    }

    #[test]
    fn unknown_codebase_machine_is_cleared() {
        let mut t = sample();
        t.placements.get_mut("buoy").unwrap().codebase = Some("ghost-host".into());
        let projects: HashSet<String> = ["ferry", "buoy"].iter().map(|s| (*s).to_string()).collect();
        t.validate(&projects);
        assert_eq!(t.placements["buoy"].codebase, None);
    }
}
