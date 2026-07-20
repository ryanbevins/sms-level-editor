//! Project-side authoring model for Sunshine `scene.ral` route graphs.
//!
//! The authoring document is semantic: it retains no source byte buffer. An
//! untouched lift recompiles to the exact imported `RalDocument`, including
//! descriptor offsets, unused connection slots, and classified padding.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sms_formats::{RalDocument, RalGraph, RalNode, StageMiscPaddingRegion};
use thiserror::Error;

pub const ROUTE_RESOURCE_PATH: &[u8] = b"map/scene.ral";

#[derive(Debug, Clone, PartialEq)]
pub struct RouteAssignmentSuggestion {
    pub graph_id: String,
    pub graph_name: String,
    pub current: bool,
    pub same_factory_uses: usize,
    pub consumer_count: usize,
    pub nearest_distance: f32,
}
pub const DEFAULT_ROUTE_BAKE_TOLERANCE: f32 = 25.0;
pub const MAX_ROUTE_SAMPLES_PER_LINK: usize = 64;
const MAX_RAL_NODES: usize = u16::MAX as usize + 1;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RouteAuthoringError {
    #[error("route graph {graph:?} was not found")]
    MissingGraph { graph: String },
    #[error("route graph {graph:?} already exists")]
    DuplicateGraph { graph: String },
    #[error("route graph {graph:?} has no control points")]
    EmptyGraph { graph: String },
    #[error("route graph {graph:?} contains duplicate control id {id:?}")]
    DuplicateControl { graph: String, id: String },
    #[error("route graph {graph:?} link {link:?} references missing control {id:?}")]
    MissingControl {
        graph: String,
        link: String,
        id: String,
    },
    #[error("route graph {graph:?} link {link:?} connects a control to itself")]
    SelfLink { graph: String, link: String },
    #[error("route graph {graph:?} link {link:?} has no active direction")]
    DirectionlessLink { graph: String, link: String },
    #[error("route graph {graph:?} node {node} exceeds eight outgoing connections")]
    TooManyConnections { graph: String, node: usize },
    #[error("route graph {graph:?} would compile to {count} nodes (maximum {MAX_RAL_NODES})")]
    TooManyNodes { graph: String, count: usize },
    #[error("route graph {graph:?} link {link:?} exceeded {MAX_ROUTE_SAMPLES_PER_LINK} generated samples")]
    TooManySamples { graph: String, link: String },
    #[error("route graph {graph:?} generated point component {value} outside the i16 range")]
    PositionOutOfRange { graph: String, value: f32 },
    #[error("route graph {graph:?} has duplicate link between {from:?} and {to:?}")]
    DuplicateLink {
        graph: String,
        from: String,
        to: String,
    },
    #[error("route bake tolerance must be finite and greater than zero")]
    InvalidTolerance,
    #[error("compiled route document is invalid: {0}")]
    Format(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteAuthoringDocument {
    pub raw_resource_path: Vec<u8>,
    pub graphs: Vec<RouteGraph>,
    pub file_size: u32,
    pub padding: Vec<StageMiscPaddingRegion>,
    #[serde(default = "default_bake_tolerance")]
    pub bake_tolerance: f32,
    #[serde(default)]
    pub layout_dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteGraph {
    pub id: String,
    pub name: String,
    pub name_offset: u32,
    pub nodes_offset: u32,
    pub controls: Vec<RouteControlPoint>,
    pub links: Vec<RouteLink>,
    #[serde(default)]
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteControlPoint {
    pub id: String,
    /// Full retail node state, including inactive connection/period slots.
    pub node: RalNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteLink {
    pub id: String,
    pub from: String,
    pub to: String,
    pub forward: Option<RouteDirection>,
    pub reverse: Option<RouteDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bezier: Option<BezierHandles>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDirection {
    pub source_slot: Option<u8>,
    pub period: RoutePeriod,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum RoutePeriod {
    AutoDistance,
    Manual(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BezierHandles {
    pub from: [f32; 3],
    pub to: [f32; 3],
}

fn default_bake_tolerance() -> f32 {
    DEFAULT_ROUTE_BAKE_TOLERANCE
}

impl RouteAuthoringDocument {
    pub fn lift(raw_resource_path: impl Into<Vec<u8>>, document: &RalDocument) -> Self {
        Self {
            raw_resource_path: raw_resource_path.into(),
            graphs: document
                .graphs
                .iter()
                .enumerate()
                .map(|(index, graph)| RouteGraph::lift(index, graph))
                .collect(),
            file_size: document.file_size,
            padding: document.padding.clone(),
            bake_tolerance: DEFAULT_ROUTE_BAKE_TOLERANCE,
            layout_dirty: false,
        }
    }

    pub fn compile(&self) -> Result<RalDocument, RouteAuthoringError> {
        if !self.bake_tolerance.is_finite() || self.bake_tolerance <= 0.0 {
            return Err(RouteAuthoringError::InvalidTolerance);
        }
        let any_dirty = self.layout_dirty || self.graphs.iter().any(|graph| graph.dirty);
        let mut result = RalDocument {
            graphs: self
                .graphs
                .iter()
                .map(|graph| graph.compile(self.bake_tolerance))
                .collect::<Result<Vec<_>, _>>()?,
            file_size: self.file_size,
            padding: self.padding.clone(),
        };
        if any_dirty {
            result
                .canonicalize_layout()
                .map_err(|error| RouteAuthoringError::Format(error.to_string()))?;
        } else {
            result
                .encode()
                .map_err(|error| RouteAuthoringError::Format(error.to_string()))?;
        }
        Ok(result)
    }

    pub fn graph(&self, id: &str) -> Option<&RouteGraph> {
        self.graphs.iter().find(|graph| graph.id == id)
    }

    pub fn graph_mut(&mut self, id: &str) -> Option<&mut RouteGraph> {
        self.graphs.iter_mut().find(|graph| graph.id == id)
    }

    pub fn graph_by_name(&self, name: &str) -> Option<&RouteGraph> {
        self.graphs.iter().find(|graph| graph.name == name)
    }

    pub fn rename_graph(
        &mut self,
        id: &str,
        name: impl Into<String>,
    ) -> Result<(), RouteAuthoringError> {
        let name = name.into();
        if self
            .graphs
            .iter()
            .any(|graph| graph.id != id && graph.name == name)
        {
            return Err(RouteAuthoringError::DuplicateGraph { graph: name });
        }
        let graph = self
            .graph_mut(id)
            .ok_or_else(|| RouteAuthoringError::MissingGraph {
                graph: id.to_string(),
            })?;
        graph.name = name;
        graph.dirty = true;
        self.layout_dirty = true;
        Ok(())
    }

    pub fn add_graph(
        &mut self,
        name: impl Into<String>,
        first: [i16; 3],
        second: [i16; 3],
    ) -> Result<String, RouteAuthoringError> {
        let name = name.into();
        if self.graphs.iter().any(|graph| graph.name == name) {
            return Err(RouteAuthoringError::DuplicateGraph { graph: name });
        }
        let id = next_id("g", self.graphs.iter().map(|graph| graph.id.as_str()));
        let first_id = format!("{id}:n0000");
        let second_id = format!("{id}:n0001");
        let mut graph = RouteGraph {
            id: id.clone(),
            name,
            name_offset: 0,
            nodes_offset: 0,
            controls: vec![
                RouteControlPoint::new(first_id.clone(), first),
                RouteControlPoint::new(second_id.clone(), second),
            ],
            links: Vec::new(),
            dirty: true,
        };
        graph.connect_bidirectional(&first_id, &second_id)?;
        self.graphs.push(graph);
        self.layout_dirty = true;
        Ok(id)
    }

    pub fn duplicate_graph(
        &mut self,
        source_id: &str,
        name: impl Into<String>,
    ) -> Result<String, RouteAuthoringError> {
        let name = name.into();
        if self.graphs.iter().any(|graph| graph.name == name) {
            return Err(RouteAuthoringError::DuplicateGraph { graph: name });
        }
        let source =
            self.graph(source_id)
                .cloned()
                .ok_or_else(|| RouteAuthoringError::MissingGraph {
                    graph: source_id.to_string(),
                })?;
        let id = next_id("g", self.graphs.iter().map(|graph| graph.id.as_str()));
        let mut remap = BTreeMap::new();
        let controls = source
            .controls
            .into_iter()
            .enumerate()
            .map(|(index, mut control)| {
                let new_id = format!("{id}:n{index:04}");
                remap.insert(control.id, new_id.clone());
                control.id = new_id;
                control
            })
            .collect::<Vec<_>>();
        let links = source
            .links
            .into_iter()
            .enumerate()
            .map(|(index, mut link)| {
                link.id = format!("{id}:l{index:04}");
                link.from = remap[&link.from].clone();
                link.to = remap[&link.to].clone();
                link
            })
            .collect();
        self.graphs.push(RouteGraph {
            id: id.clone(),
            name,
            name_offset: 0,
            nodes_offset: 0,
            controls,
            links,
            dirty: true,
        });
        self.layout_dirty = true;
        Ok(id)
    }

    pub fn remove_graph(&mut self, id: &str) -> Option<RouteGraph> {
        let index = self.graphs.iter().position(|graph| graph.id == id)?;
        self.layout_dirty = true;
        Some(self.graphs.remove(index))
    }
}

impl RouteGraph {
    fn lift(graph_index: usize, graph: &RalGraph) -> Self {
        let id = format!("g{graph_index:04}");
        let controls = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(node_index, node)| RouteControlPoint {
                id: format!("{id}:n{node_index:04}"),
                node: node.clone(),
            })
            .collect::<Vec<_>>();
        let mut pairs = BTreeMap::<(usize, usize), RouteLink>::new();
        let mut link_serial = 0usize;
        for (from, node) in graph.nodes.iter().enumerate() {
            for slot in 0..node.connection_count as usize {
                let to = node.connections[slot] as usize;
                let key = if from < to { (from, to) } else { (to, from) };
                let link = pairs.entry(key).or_insert_with(|| {
                    let link = RouteLink {
                        id: format!("{id}:l{link_serial:04}"),
                        from: format!("{id}:n{:04}", key.0),
                        to: format!("{id}:n{:04}", key.1),
                        forward: None,
                        reverse: None,
                        bezier: None,
                    };
                    link_serial += 1;
                    link
                });
                let distance = node_distance(&graph.nodes[from], &graph.nodes[to]);
                let direction = RouteDirection {
                    source_slot: Some(slot as u8),
                    period: if (node.periods[slot] - distance).abs() <= 2.0 {
                        RoutePeriod::AutoDistance
                    } else {
                        RoutePeriod::Manual(node.periods[slot])
                    },
                };
                if from == key.0 {
                    link.forward = Some(direction);
                } else {
                    link.reverse = Some(direction);
                }
            }
        }
        Self {
            id,
            name: graph.name.clone(),
            name_offset: graph.name_offset,
            nodes_offset: graph.nodes_offset,
            controls,
            links: pairs.into_values().collect(),
            dirty: false,
        }
    }

    pub fn control(&self, id: &str) -> Option<&RouteControlPoint> {
        self.controls.iter().find(|control| control.id == id)
    }

    pub fn control_mut(&mut self, id: &str) -> Option<&mut RouteControlPoint> {
        self.controls.iter_mut().find(|control| control.id == id)
    }

    pub fn set_control_position(&mut self, id: &str, position: [i16; 3]) -> bool {
        let Some(index) = self.controls.iter().position(|control| control.id == id) else {
            return false;
        };
        let old = self.controls[index].node.position;
        if old == position {
            return false;
        }
        let delta = [
            f32::from(position[0]) - f32::from(old[0]),
            f32::from(position[1]) - f32::from(old[1]),
            f32::from(position[2]) - f32::from(old[2]),
        ];
        self.controls[index].node.position = position;
        for link in &mut self.links {
            if let Some(handles) = &mut link.bezier {
                if link.from == id {
                    handles.from = add(handles.from, delta);
                }
                if link.to == id {
                    handles.to = add(handles.to, delta);
                }
            }
        }
        self.dirty = true;
        true
    }

    pub fn add_control(&mut self, position: [i16; 3]) -> String {
        let prefix = format!("{}:n", self.id);
        let id = next_id(
            &prefix,
            self.controls.iter().map(|control| control.id.as_str()),
        );
        self.controls
            .push(RouteControlPoint::new(id.clone(), position));
        self.dirty = true;
        id
    }

    pub fn connect_bidirectional(
        &mut self,
        from: &str,
        to: &str,
    ) -> Result<String, RouteAuthoringError> {
        self.validate_endpoint(from, "new link")?;
        self.validate_endpoint(to, "new link")?;
        if from == to {
            return Err(RouteAuthoringError::SelfLink {
                graph: self.name.clone(),
                link: "new link".to_string(),
            });
        }
        if self.links.iter().any(|link| same_pair(link, from, to)) {
            return Err(RouteAuthoringError::DuplicateLink {
                graph: self.name.clone(),
                from: from.to_string(),
                to: to.to_string(),
            });
        }
        let prefix = format!("{}:l", self.id);
        let id = next_id(&prefix, self.links.iter().map(|link| link.id.as_str()));
        self.links.push(RouteLink {
            id: id.clone(),
            from: from.to_string(),
            to: to.to_string(),
            forward: Some(RouteDirection {
                source_slot: None,
                period: RoutePeriod::AutoDistance,
            }),
            reverse: Some(RouteDirection {
                source_slot: None,
                period: RoutePeriod::AutoDistance,
            }),
            bezier: None,
        });
        self.dirty = true;
        Ok(id)
    }

    pub fn remove_link(&mut self, id: &str) -> Option<RouteLink> {
        let index = self.links.iter().position(|link| link.id == id)?;
        self.dirty = true;
        Some(self.links.remove(index))
    }

    pub fn set_link_bezier(&mut self, link_id: &str, bezier: Option<BezierHandles>) -> bool {
        let Some(link) = self.links.iter_mut().find(|link| link.id == link_id) else {
            return false;
        };
        if link.bezier == bezier {
            return false;
        }
        link.bezier = bezier;
        self.dirty = true;
        true
    }

    pub fn reset_link_to_curve(&mut self, link_id: &str) -> bool {
        let Some(index) = self.links.iter().position(|link| link.id == link_id) else {
            return false;
        };
        let (from, to) = {
            let link = &self.links[index];
            let Some(from) = self.control(&link.from) else {
                return false;
            };
            let Some(to) = self.control(&link.to) else {
                return false;
            };
            (node_position(from), node_position(to))
        };
        let delta = scale(sub(to, from), 1.0 / 3.0);
        self.links[index].bezier = Some(BezierHandles {
            from: add(from, delta),
            to: sub(to, delta),
        });
        self.dirty = true;
        true
    }
    pub fn set_link_direction(&mut self, link_id: &str, forward: bool, reverse: bool) -> bool {
        let Some(link) = self.links.iter_mut().find(|link| link.id == link_id) else {
            return false;
        };
        let old_forward = link.forward.clone();
        let old_reverse = link.reverse.clone();
        link.forward = forward.then(|| {
            old_forward.clone().unwrap_or(RouteDirection {
                source_slot: None,
                period: RoutePeriod::AutoDistance,
            })
        });
        link.reverse = reverse.then(|| {
            old_reverse.clone().unwrap_or(RouteDirection {
                source_slot: None,
                period: RoutePeriod::AutoDistance,
            })
        });
        let changed = link.forward != old_forward || link.reverse != old_reverse;
        self.dirty |= changed;
        changed
    }

    pub fn reverse_link(&mut self, link_id: &str) -> bool {
        let Some(link) = self.links.iter_mut().find(|link| link.id == link_id) else {
            return false;
        };
        std::mem::swap(&mut link.from, &mut link.to);
        std::mem::swap(&mut link.forward, &mut link.reverse);
        if let Some(handles) = &mut link.bezier {
            std::mem::swap(&mut handles.from, &mut handles.to);
        }
        self.dirty = true;
        true
    }

    pub fn remove_control(&mut self, control_id: &str) -> Option<RouteControlPoint> {
        let index = self
            .controls
            .iter()
            .position(|control| control.id == control_id)?;
        self.links
            .retain(|link| link.from != control_id && link.to != control_id);
        self.dirty = true;
        Some(self.controls.remove(index))
    }

    pub fn split_link(&mut self, link_id: &str) -> Result<String, RouteAuthoringError> {
        let index = self
            .links
            .iter()
            .position(|link| link.id == link_id)
            .ok_or_else(|| RouteAuthoringError::MissingControl {
                graph: self.name.clone(),
                link: link_id.to_string(),
                id: "link".to_string(),
            })?;
        let link = self.links.remove(index);
        let start = node_position(self.control(&link.from).ok_or_else(|| {
            RouteAuthoringError::MissingControl {
                graph: self.name.clone(),
                link: link.id.clone(),
                id: link.from.clone(),
            }
        })?);
        let end = node_position(self.control(&link.to).ok_or_else(|| {
            RouteAuthoringError::MissingControl {
                graph: self.name.clone(),
                link: link.id.clone(),
                id: link.to.clone(),
            }
        })?);
        let (position, left, right) = if let Some(handles) = link.bezier {
            let a = midpoint(start, handles.from);
            let b = midpoint(handles.from, handles.to);
            let c = midpoint(handles.to, end);
            let d = midpoint(a, b);
            let e = midpoint(b, c);
            (midpoint(d, e), Some((a, d)), Some((e, c)))
        } else {
            (midpoint(start, end), None, None)
        };
        let middle = self.add_control(round_position(&self.name, position)?);
        let first_id = format!("{}:a", link.id);
        let second_id = format!("{}:b", link.id);
        self.links.insert(
            index,
            RouteLink {
                id: first_id,
                from: link.from.clone(),
                to: middle.clone(),
                forward: split_direction(&link.forward, true),
                reverse: split_direction(&link.reverse, false),
                bezier: left.map(|(from, to)| BezierHandles { from, to }),
            },
        );
        self.links.insert(
            index + 1,
            RouteLink {
                id: second_id,
                from: middle.clone(),
                to: link.to,
                forward: split_direction(&link.forward, false),
                reverse: split_direction(&link.reverse, true),
                bezier: right.map(|(from, to)| BezierHandles { from, to }),
            },
        );
        self.dirty = true;
        Ok(middle)
    }

    pub fn nearest_control_distance(&self, position: [f32; 3]) -> Option<f32> {
        self.controls
            .iter()
            .map(|control| distance(position, node_position(control)))
            .min_by(f32::total_cmp)
    }

    fn validate_endpoint(&self, id: &str, link: &str) -> Result<(), RouteAuthoringError> {
        self.control(id)
            .map(|_| ())
            .ok_or_else(|| RouteAuthoringError::MissingControl {
                graph: self.name.clone(),
                link: link.to_string(),
                id: id.to_string(),
            })
    }

    fn compile(&self, tolerance: f32) -> Result<RalGraph, RouteAuthoringError> {
        if self.controls.is_empty() {
            return Err(RouteAuthoringError::EmptyGraph {
                graph: self.name.clone(),
            });
        }
        let ids = self
            .controls
            .iter()
            .map(|control| control.id.as_str())
            .collect::<BTreeSet<_>>();
        if ids.len() != self.controls.len() {
            let duplicate = self
                .controls
                .iter()
                .map(|control| control.id.as_str())
                .find(|id| {
                    self.controls
                        .iter()
                        .filter(|control| control.id == *id)
                        .count()
                        > 1
                })
                .unwrap_or_default();
            return Err(RouteAuthoringError::DuplicateControl {
                graph: self.name.clone(),
                id: duplicate.to_string(),
            });
        }
        if !self.dirty && self.links.iter().all(|link| link.bezier.is_none()) {
            return Ok(RalGraph {
                name_offset: self.name_offset,
                nodes_offset: self.nodes_offset,
                name: self.name.clone(),
                nodes: self
                    .controls
                    .iter()
                    .map(|control| control.node.clone())
                    .collect(),
            });
        }

        let mut nodes = self
            .controls
            .iter()
            .map(|control| {
                let mut node = control.node.clone();
                node.connection_count = 0;
                node
            })
            .collect::<Vec<_>>();
        let control_indices = self
            .controls
            .iter()
            .enumerate()
            .map(|(index, control)| (control.id.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        let mut seen_pairs = BTreeSet::new();
        for link in &self.links {
            let Some(&from) = control_indices.get(link.from.as_str()) else {
                return Err(RouteAuthoringError::MissingControl {
                    graph: self.name.clone(),
                    link: link.id.clone(),
                    id: link.from.clone(),
                });
            };
            let Some(&to) = control_indices.get(link.to.as_str()) else {
                return Err(RouteAuthoringError::MissingControl {
                    graph: self.name.clone(),
                    link: link.id.clone(),
                    id: link.to.clone(),
                });
            };
            if from == to {
                return Err(RouteAuthoringError::SelfLink {
                    graph: self.name.clone(),
                    link: link.id.clone(),
                });
            }
            if link.forward.is_none() && link.reverse.is_none() {
                return Err(RouteAuthoringError::DirectionlessLink {
                    graph: self.name.clone(),
                    link: link.id.clone(),
                });
            }
            let pair = (from.min(to), from.max(to));
            if !seen_pairs.insert(pair) {
                return Err(RouteAuthoringError::DuplicateLink {
                    graph: self.name.clone(),
                    from: link.from.clone(),
                    to: link.to.clone(),
                });
            }

            let start = node_position(&self.controls[from]);
            let end = node_position(&self.controls[to]);
            let samples = match link.bezier {
                Some(handles) => {
                    adaptive_bezier_samples(start, handles.from, handles.to, end, tolerance)
                        .ok_or_else(|| RouteAuthoringError::TooManySamples {
                            graph: self.name.clone(),
                            link: link.id.clone(),
                        })?
                }
                None => Vec::new(),
            };
            if nodes.len() + samples.len() > MAX_RAL_NODES {
                return Err(RouteAuthoringError::TooManyNodes {
                    graph: self.name.clone(),
                    count: nodes.len() + samples.len(),
                });
            }
            let mut chain = Vec::with_capacity(samples.len() + 2);
            chain.push(from);
            for sample in samples {
                let position = round_position(&self.name, sample)?;
                let index = nodes.len();
                nodes.push(RouteControlPoint::new(String::new(), position).node);
                chain.push(index);
            }
            chain.push(to);
            let lengths = chain
                .windows(2)
                .map(|pair| node_distance(&nodes[pair[0]], &nodes[pair[1]]))
                .collect::<Vec<_>>();
            if let Some(direction) = &link.forward {
                add_chain(&self.name, &mut nodes, &chain, &lengths, direction, false)?;
            }
            if let Some(direction) = &link.reverse {
                add_chain(&self.name, &mut nodes, &chain, &lengths, direction, true)?;
            }
        }
        Ok(RalGraph {
            name_offset: self.name_offset,
            nodes_offset: self.nodes_offset,
            name: self.name.clone(),
            nodes,
        })
    }
}

impl RouteControlPoint {
    pub fn new(id: String, position: [i16; 3]) -> Self {
        Self {
            id,
            node: RalNode {
                position,
                connection_count: 0,
                flags: 0,
                pitch: u16::MAX,
                yaw: u16::MAX,
                roll: u16::MAX,
                speed: u16::MAX,
                connections: [0; 8],
                periods: [0.0; 8],
            },
        }
    }
}
fn split_direction(
    direction: &Option<RouteDirection>,
    keep_source_slot: bool,
) -> Option<RouteDirection> {
    direction.clone().map(|mut direction| {
        if let RoutePeriod::Manual(value) = &mut direction.period {
            *value *= 0.5;
        }
        if !keep_source_slot {
            direction.source_slot = None;
        }
        direction
    })
}

fn add_chain(
    graph: &str,
    nodes: &mut [RalNode],
    chain: &[usize],
    lengths: &[f32],
    direction: &RouteDirection,
    reverse: bool,
) -> Result<(), RouteAuthoringError> {
    let total_length: f32 = lengths.iter().sum();
    let total_period = match direction.period {
        RoutePeriod::AutoDistance => total_length,
        RoutePeriod::Manual(value) => value,
    };
    let node_count = nodes.len();
    for segment in 0..lengths.len() {
        let (from, to, length) = if reverse {
            (chain[segment + 1], chain[segment], lengths[segment])
        } else {
            (chain[segment], chain[segment + 1], lengths[segment])
        };
        let requested_slot =
            if (!reverse && segment == 0) || (reverse && segment + 1 == lengths.len()) {
                direction.source_slot.map(usize::from)
            } else {
                None
            };
        let node = &mut nodes[from];
        let slot = requested_slot.unwrap_or(node.connection_count as usize);
        if slot >= 8 {
            return Err(RouteAuthoringError::TooManyConnections {
                graph: graph.to_string(),
                node: from,
            });
        }
        node.connections[slot] =
            u16::try_from(to).map_err(|_| RouteAuthoringError::TooManyNodes {
                graph: graph.to_string(),
                count: node_count,
            })?;
        node.periods[slot] = if total_length > f32::EPSILON {
            total_period * length / total_length
        } else {
            0.0
        };
        node.connection_count = node.connection_count.max((slot + 1) as i16);
    }
    Ok(())
}

fn adaptive_bezier_samples(
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    p3: [f32; 3],
    tolerance: f32,
) -> Option<Vec<[f32; 3]>> {
    fn recurse(
        out: &mut Vec<[f32; 3]>,
        p0: [f32; 3],
        p1: [f32; 3],
        p2: [f32; 3],
        p3: [f32; 3],
        tolerance: f32,
        depth: u8,
    ) -> bool {
        if cubic_flatness(p0, p1, p2, p3) <= tolerance {
            return true;
        }
        if depth >= 16 || out.len() >= MAX_ROUTE_SAMPLES_PER_LINK {
            return false;
        }
        let a = midpoint(p0, p1);
        let b = midpoint(p1, p2);
        let c = midpoint(p2, p3);
        let d = midpoint(a, b);
        let e = midpoint(b, c);
        let m = midpoint(d, e);
        if !recurse(out, p0, a, d, m, tolerance, depth + 1) {
            return false;
        }
        out.push(m);
        recurse(out, m, e, c, p3, tolerance, depth + 1)
    }
    let mut out = Vec::new();
    recurse(&mut out, p0, p1, p2, p3, tolerance, 0).then_some(out)
}

fn cubic_flatness(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3], p3: [f32; 3]) -> f32 {
    point_line_distance(p1, p0, p3).max(point_line_distance(p2, p0, p3))
}

fn point_line_distance(point: [f32; 3], start: [f32; 3], end: [f32; 3]) -> f32 {
    let line = sub(end, start);
    let denom = dot(line, line);
    if denom <= f32::EPSILON {
        return distance(point, start);
    }
    let t = (dot(sub(point, start), line) / denom).clamp(0.0, 1.0);
    distance(point, add(start, scale(line, t)))
}

fn round_position(graph: &str, point: [f32; 3]) -> Result<[i16; 3], RouteAuthoringError> {
    let mut out = [0i16; 3];
    for (index, value) in point.into_iter().enumerate() {
        let rounded = value.round();
        if !rounded.is_finite() || rounded < f32::from(i16::MIN) || rounded > f32::from(i16::MAX) {
            return Err(RouteAuthoringError::PositionOutOfRange {
                graph: graph.to_string(),
                value,
            });
        }
        out[index] = rounded as i16;
    }
    Ok(out)
}

fn same_pair(link: &RouteLink, from: &str, to: &str) -> bool {
    (link.from == from && link.to == to) || (link.from == to && link.to == from)
}

fn next_id<'a>(prefix: &str, existing: impl Iterator<Item = &'a str>) -> String {
    let used = existing.collect::<BTreeSet<_>>();
    (0u32..)
        .map(|index| format!("{prefix}{index:04}"))
        .find(|candidate| !used.contains(candidate.as_str()))
        .expect("u32 id space")
}

fn node_position(control: &RouteControlPoint) -> [f32; 3] {
    control.node.position.map(f32::from)
}
fn node_distance(a: &RalNode, b: &RalNode) -> f32 {
    distance(a.position.map(f32::from), b.position.map(f32::from))
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f32; 3], value: f32) -> [f32; 3] {
    [a[0] * value, a[1] * value, a[2] * value]
}
fn midpoint(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    scale(add(a, b), 0.5)
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    dot(sub(a, b), sub(a, b)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(position: [i16; 3], connections: &[u16], periods: &[f32]) -> RalNode {
        let mut node = RouteControlPoint::new(String::new(), position).node;
        node.connection_count = connections.len() as i16;
        node.connections[..connections.len()].copy_from_slice(connections);
        node.periods[..periods.len()].copy_from_slice(periods);
        node
    }

    fn document() -> RalDocument {
        let mut document = RalDocument {
            graphs: vec![RalGraph {
                name_offset: 0,
                nodes_offset: 0,
                name: "monte0".to_string(),
                nodes: vec![
                    node([0, 0, 0], &[1], &[100.0]),
                    node([100, 0, 0], &[0], &[100.0]),
                ],
            }],
            file_size: 0,
            padding: Vec::new(),
        };
        document.canonicalize_layout().unwrap();
        document
    }

    #[test]
    fn untouched_lift_json_round_trip_is_byte_identical() {
        let source = document();
        let bytes = source.encode().unwrap();
        let lifted = RouteAuthoringDocument::lift(b"map/scene.ral".to_vec(), &source);
        let json = serde_json::to_vec(&lifted).unwrap();
        let reopened: RouteAuthoringDocument = serde_json::from_slice(&json).unwrap();
        assert_eq!(reopened.compile().unwrap().encode().unwrap(), bytes);
    }

    #[test]
    fn curved_bidirectional_link_bakes_runtime_nodes_and_weights() {
        let source = document();
        let mut lifted = RouteAuthoringDocument::lift(b"map/scene.ral".to_vec(), &source);
        let graph = &mut lifted.graphs[0];
        let link_id = graph.links[0].id.clone();
        assert!(graph.set_link_bezier(
            &link_id,
            Some(BezierHandles {
                from: [25.0, 100.0, 0.0],
                to: [75.0, 100.0, 0.0],
            })
        ));
        let compiled = lifted.compile().unwrap();
        assert!(compiled.graphs[0].nodes.len() > 2);
        assert_eq!(compiled.graphs[0].nodes[0].connection_count, 1);
        assert_eq!(compiled.graphs[0].nodes[1].connection_count, 1);
        assert_eq!(
            RalDocument::parse(compiled.encode().unwrap()).unwrap(),
            compiled
        );
    }

    #[test]
    fn new_graph_defaults_to_two_way_distance_weighted_nodes() {
        let mut routes = RouteAuthoringDocument::lift(
            b"map/scene.ral".to_vec(),
            &RalDocument::empty_canonical(),
        );
        routes.add_graph("route", [0, 0, 0], [0, 0, 500]).unwrap();
        let compiled = routes.compile().unwrap();
        assert_eq!(compiled.graphs[0].nodes.len(), 2);
        assert_eq!(compiled.graphs[0].nodes[0].periods[0], 500.0);
        assert_eq!(compiled.graphs[0].nodes[1].periods[0], 500.0);
    }

    #[test]
    fn moved_control_preserves_imported_slot_order_and_inactive_values() {
        let mut source = document();
        source.graphs[0].nodes.push(node([200, 0, 0], &[], &[]));
        source.graphs[0].nodes[0].connection_count = 2;
        source.graphs[0].nodes[0].connections = [2, 1, 0xBEEF, 7, 6, 5, 4, 3];
        source.graphs[0].nodes[0].periods = [200.0, 100.0, 91.0, 92.0, 93.0, 94.0, 95.0, 96.0];
        source.canonicalize_layout().unwrap();
        let mut routes = RouteAuthoringDocument::lift(ROUTE_RESOURCE_PATH, &source);
        let control_id = routes.graphs[0].controls[0].id.clone();
        routes.graphs[0].set_control_position(&control_id, [0, 25, 0]);
        let compiled = routes.compile().unwrap();
        assert_eq!(&compiled.graphs[0].nodes[0].connections[..2], &[2, 1]);
        assert_eq!(compiled.graphs[0].nodes[0].connections[2], 0xBEEF);
        assert_eq!(compiled.graphs[0].nodes[0].periods[2], 91.0);
    }

    #[test]
    fn manual_period_is_distributed_without_changing_its_total() {
        let mut source = document();
        source.graphs[0].nodes[0].periods[0] = 240.0;
        let mut routes = RouteAuthoringDocument::lift(ROUTE_RESOURCE_PATH, &source);
        let graph = &mut routes.graphs[0];
        let link_id = graph.links[0].id.clone();
        graph.set_link_direction(&link_id, true, false);
        graph.set_link_bezier(
            &link_id,
            Some(BezierHandles {
                from: [0.0, 200.0, 0.0],
                to: [100.0, 200.0, 0.0],
            }),
        );
        let compiled = routes.compile().unwrap();
        let nodes = &compiled.graphs[0].nodes;
        let mut index = 0usize;
        let mut total = 0.0;
        let mut visited = BTreeSet::new();
        while index != 1 {
            assert!(visited.insert(index));
            total += nodes[index].periods[0];
            index = nodes[index].connections[0] as usize;
        }
        assert!((total - 240.0).abs() < 0.01, "{total}");
    }

    #[test]
    fn ninth_outgoing_connection_blocks_export() {
        let mut routes =
            RouteAuthoringDocument::lift(ROUTE_RESOURCE_PATH, &RalDocument::empty_canonical());
        let graph_id = routes.add_graph("hub", [0, 0, 0], [10, 0, 0]).unwrap();
        let graph = routes.graph_mut(&graph_id).unwrap();
        let hub = graph.controls[0].id.clone();
        for index in 0..8 {
            let endpoint = graph.add_control([20 + index * 10, 0, 0]);
            graph.connect_bidirectional(&hub, &endpoint).unwrap();
        }
        assert!(matches!(
            routes.compile(),
            Err(RouteAuthoringError::TooManyConnections { .. })
        ));
    }

    #[test]
    fn de_casteljau_split_preserves_curve_join_and_manual_total() {
        let mut source = document();
        source.graphs[0].nodes[0].periods[0] = 300.0;
        let mut routes = RouteAuthoringDocument::lift(ROUTE_RESOURCE_PATH, &source);
        let graph = &mut routes.graphs[0];
        let link_id = graph.links[0].id.clone();
        graph.set_link_direction(&link_id, true, false);
        graph.set_link_bezier(
            &link_id,
            Some(BezierHandles {
                from: [0.0, 150.0, 0.0],
                to: [100.0, 150.0, 0.0],
            }),
        );
        let middle = graph.split_link(&link_id).unwrap();
        assert_eq!(graph.links.len(), 2);
        assert!(graph.control(&middle).is_some());
        let total = graph
            .links
            .iter()
            .map(|link| match link.forward.as_ref().unwrap().period {
                RoutePeriod::Manual(value) => value,
                RoutePeriod::AutoDistance => 0.0,
            })
            .sum::<f32>();
        assert_eq!(total, 300.0);
        routes.compile().unwrap();
    }
}
