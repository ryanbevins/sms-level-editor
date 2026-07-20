use std::collections::{BTreeMap, BTreeSet};

use eframe::egui;
use sms_formats::{JDramaRecord, JDramaRecordPayload};
use sms_scene::{PlacementBinding, SceneObject, StageDocument, StageResourceDocument};

use crate::game_text::{bilingual_object_name, bilingual_record_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OutlinerNodeKind {
    Stage,
    Resource,
    Group,
    Object,
    Editor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutlinerNode {
    key: String,
    label: String,
    detail: String,
    search_text: String,
    kind: OutlinerNodeKind,
    object_id: Option<String>,
    children: Vec<OutlinerNode>,
}

impl OutlinerNode {
    fn object_count(&self) -> usize {
        usize::from(self.object_id.is_some())
            + self
                .children
                .iter()
                .map(OutlinerNode::object_count)
                .sum::<usize>()
    }

    fn filtered(mut self, needle: &str) -> Option<Self> {
        if needle.is_empty() || self.search_text.contains(needle) {
            return Some(self);
        }
        self.children = self
            .children
            .into_iter()
            .filter_map(|child| child.filtered(needle))
            .collect();
        (!self.children.is_empty()).then_some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutlinerTree {
    roots: Vec<OutlinerNode>,
    pub(super) total_objects: usize,
    pub(super) visible_objects: usize,
}

pub(super) fn build_outliner_tree(document: &StageDocument, filter: &str) -> OutlinerTree {
    let total_objects = document.objects.len();
    let mut builder = OutlinerBuilder::new(document);
    let mut stage_children = builder.semantic_resource_nodes();
    stage_children.extend(builder.remaining_object_nodes());

    let stage = OutlinerNode {
        key: format!("stage:{}", document.stage_id),
        label: document.stage_id.clone(),
        detail: format!("{} objects", total_objects),
        search_text: format!("{} stage level", document.stage_id.to_ascii_lowercase()),
        kind: OutlinerNodeKind::Stage,
        object_id: None,
        children: stage_children,
    };

    let needle = filter.trim().to_ascii_lowercase();
    let roots: Vec<_> = stage.filtered(&needle).into_iter().collect();
    let visible_objects = roots.iter().map(OutlinerNode::object_count).sum();
    OutlinerTree {
        roots,
        total_objects,
        visible_objects,
    }
}

struct OutlinerBuilder<'a> {
    document: &'a StageDocument,
    existing_by_address: BTreeMap<(Vec<u8>, Vec<usize>), Vec<usize>>,
    clones_by_parent: BTreeMap<(Vec<u8>, Vec<usize>), Vec<usize>>,
    related_children: BTreeMap<String, Vec<usize>>,
    consumed: BTreeSet<String>,
    building: BTreeSet<String>,
}

impl<'a> OutlinerBuilder<'a> {
    fn new(document: &'a StageDocument) -> Self {
        let parent_ids = related_object_parent_ids(document);
        let mut related_children = BTreeMap::<String, Vec<usize>>::new();
        let mut existing_by_address = BTreeMap::<(Vec<u8>, Vec<usize>), Vec<usize>>::new();
        let mut clones_by_parent = BTreeMap::<(Vec<u8>, Vec<usize>), Vec<usize>>::new();

        for (index, object) in document.objects.iter().enumerate() {
            if let Some(parent_id) = parent_ids.get(&object.id) {
                related_children
                    .entry(parent_id.clone())
                    .or_default()
                    .push(index);
                continue;
            }
            let Some(placement) = object.placement.as_ref() else {
                continue;
            };
            match placement {
                PlacementBinding::Existing(address) => {
                    existing_by_address
                        .entry((
                            address.raw_resource_path.clone(),
                            address.record_path.clone(),
                        ))
                        .or_default()
                        .push(index);
                }
                PlacementBinding::CloneOf(address) => {
                    let mut parent_path = address.record_path.clone();
                    parent_path.pop();
                    clones_by_parent
                        .entry((address.raw_resource_path.clone(), parent_path))
                        .or_default()
                        .push(index);
                }
                PlacementBinding::Authored(_) => {}
            }
        }

        Self {
            document,
            existing_by_address,
            clones_by_parent,
            related_children,
            consumed: BTreeSet::new(),
            building: BTreeSet::new(),
        }
    }

    fn semantic_resource_nodes(&mut self) -> Vec<OutlinerNode> {
        let resources = self
            .document
            .stage_archive
            .as_ref()
            .map(|archive| archive.resources())
            .unwrap_or_default();
        let mut nodes = Vec::new();
        for resource in resources {
            let StageResourceDocument::Placement(placement) = &resource.document else {
                continue;
            };
            let children = self.record_nodes(&resource.raw_path, &placement.root, &mut Vec::new());
            if children.is_empty() {
                continue;
            }
            let path = normalized_raw_path(&resource.raw_path);
            let label = path.rsplit('/').next().unwrap_or(path.as_str()).to_string();
            nodes.push(OutlinerNode {
                key: format!("resource:{path}"),
                label,
                detail: path.clone(),
                search_text: format!("{path} placement resource"),
                kind: OutlinerNodeKind::Resource,
                object_id: None,
                children,
            });
        }
        nodes
    }

    fn record_nodes(
        &mut self,
        raw_resource_path: &[u8],
        record: &JDramaRecord,
        path: &mut Vec<usize>,
    ) -> Vec<OutlinerNode> {
        match &record.payload {
            JDramaRecordPayload::Actor { .. } => self
                .existing_by_address
                .remove(&(raw_resource_path.to_vec(), path.clone()))
                .unwrap_or_default()
                .into_iter()
                .map(|index| self.object_node(index))
                .collect(),
            JDramaRecordPayload::Group { children, .. } => {
                let mut nodes = Vec::new();
                for (index, child) in children.iter().enumerate() {
                    path.push(index);
                    nodes.extend(self.record_nodes(raw_resource_path, child, path));
                    path.pop();
                }
                if let Some(clones) = self
                    .clones_by_parent
                    .remove(&(raw_resource_path.to_vec(), path.clone()))
                {
                    nodes.extend(clones.into_iter().map(|index| self.object_node(index)));
                }
                if nodes.is_empty() {
                    return Vec::new();
                }
                let label = display_record_name(record);
                let path_key = path
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(".");
                vec![OutlinerNode {
                    key: format!(
                        "group:{}:{path_key}",
                        normalized_raw_path(raw_resource_path)
                    ),
                    label: label.clone(),
                    detail: record.type_name.clone(),
                    search_text: format!(
                        "{} {} group",
                        label.to_ascii_lowercase(),
                        record.type_name.to_ascii_lowercase()
                    ),
                    kind: OutlinerNodeKind::Group,
                    object_id: None,
                    children: nodes,
                }]
            }
            JDramaRecordPayload::Fields { .. } | JDramaRecordPayload::Empty => Vec::new(),
        }
    }

    fn object_node(&mut self, index: usize) -> OutlinerNode {
        let object = &self.document.objects[index];
        self.consumed.insert(object.id.clone());
        let entered = self.building.insert(object.id.clone());
        let related = if entered {
            self.related_children
                .remove(&object.id)
                .unwrap_or_default()
                .into_iter()
                .map(|child| self.object_node(child))
                .collect()
        } else {
            Vec::new()
        };
        self.building.remove(&object.id);

        let label = bilingual_object_name(object);
        let class_name = object.class_name.as_deref().unwrap_or("Unknown class");
        let detail = if label.eq_ignore_ascii_case(&object.factory_name) {
            class_name.to_string()
        } else {
            format!("{} · {class_name}", object.factory_name)
        };
        OutlinerNode {
            key: format!("object:{}", object.id),
            label: label.clone(),
            detail: detail.clone(),
            search_text: format!(
                "{} {} {} {}",
                label.to_ascii_lowercase(),
                detail.to_ascii_lowercase(),
                object.factory_name.to_ascii_lowercase(),
                object.id.to_ascii_lowercase()
            ),
            kind: OutlinerNodeKind::Object,
            object_id: Some(object.id.clone()),
            children: related,
        }
    }

    fn remaining_object_nodes(&mut self) -> Vec<OutlinerNode> {
        let mut grouped = BTreeMap::<(String, OutlinerNodeKind), Vec<usize>>::new();
        let related_ids = self
            .related_children
            .values()
            .flatten()
            .map(|index| self.document.objects[*index].id.as_str())
            .collect::<BTreeSet<_>>();
        for (index, object) in self.document.objects.iter().enumerate() {
            if self.consumed.contains(&object.id) || related_ids.contains(object.id.as_str()) {
                continue;
            }
            let key = match object.placement.as_ref() {
                Some(placement) => (
                    normalized_raw_path(placement.raw_resource_path()),
                    OutlinerNodeKind::Resource,
                ),
                None => ("Editor Objects".to_string(), OutlinerNodeKind::Editor),
            };
            grouped.entry(key).or_default().push(index);
        }

        grouped
            .into_iter()
            .map(|((path, kind), indices)| {
                let label = if kind == OutlinerNodeKind::Resource {
                    path.rsplit('/').next().unwrap_or(path.as_str()).to_string()
                } else {
                    path.clone()
                };
                let children = indices
                    .into_iter()
                    .map(|index| self.object_node(index))
                    .collect();
                OutlinerNode {
                    key: format!("remaining:{path}"),
                    label,
                    detail: if kind == OutlinerNodeKind::Resource {
                        path.clone()
                    } else {
                        "Objects created or duplicated in this project".to_string()
                    },
                    search_text: format!("{} objects", path.to_ascii_lowercase()),
                    kind,
                    object_id: None,
                    children,
                }
            })
            .collect()
    }
}

fn related_object_parent_ids(document: &StageDocument) -> BTreeMap<String, String> {
    let mut parents = BTreeMap::new();
    for object in &document.objects {
        let Some(preview) = document.actor_preview(object) else {
            continue;
        };
        if preview.manager_factory == object.factory_name {
            continue;
        }
        let closest = document
            .objects
            .iter()
            .filter(|candidate| {
                candidate.id != object.id && candidate.factory_name == preview.manager_factory
            })
            .min_by(|left, right| {
                squared_distance(object, left)
                    .total_cmp(&squared_distance(object, right))
                    .then_with(|| left.id.cmp(&right.id))
            });
        if let Some(manager) = closest {
            parents.insert(object.id.clone(), manager.id.clone());
        }
    }
    parents
}

fn squared_distance(left: &SceneObject, right: &SceneObject) -> f32 {
    left.transform
        .translation
        .into_iter()
        .zip(right.transform.translation)
        .map(|(left, right)| (left - right).powi(2))
        .sum()
}

fn display_record_name(record: &JDramaRecord) -> String {
    bilingual_record_name(&record.name, &record.type_name)
}

fn normalized_raw_path(path: &[u8]) -> String {
    String::from_utf8_lossy(path).replace('\\', "/")
}

pub(super) fn show_outliner_tree(
    ui: &mut egui::Ui,
    tree: &OutlinerTree,
    selected_id: Option<&str>,
    force_open: bool,
) -> Option<String> {
    let mut clicked = None;
    for root in &tree.roots {
        show_outliner_node(ui, root, selected_id, force_open, 0, &mut clicked);
    }
    clicked
}

fn show_outliner_node(
    ui: &mut egui::Ui,
    node: &OutlinerNode,
    selected_id: Option<&str>,
    force_open: bool,
    depth: usize,
    clicked: &mut Option<String>,
) {
    if node.kind == OutlinerNodeKind::Object {
        show_object_node(ui, node, selected_id, force_open, depth, clicked);
        return;
    }

    let (icon, color) = match node.kind {
        OutlinerNodeKind::Stage => ("◆", egui::Color32::from_rgb(111, 205, 191)),
        OutlinerNodeKind::Resource => ("▰", egui::Color32::from_rgb(114, 166, 206)),
        OutlinerNodeKind::Group => ("◇", egui::Color32::from_rgb(178, 184, 187)),
        OutlinerNodeKind::Editor => ("✦", egui::Color32::from_rgb(220, 169, 93)),
        OutlinerNodeKind::Object => unreachable!(),
    };
    let title = egui::RichText::new(format!("{icon}  {}   {}", node.label, node.object_count()))
        .color(color)
        .strong();
    let response = egui::CollapsingHeader::new(title)
        .id_salt(&node.key)
        .default_open(depth < 2)
        .open(force_open.then_some(true))
        .show_background(true)
        .show(ui, |ui| {
            for child in &node.children {
                show_outliner_node(ui, child, selected_id, force_open, depth + 1, clicked);
            }
        });
    response.header_response.on_hover_text(&node.detail);
}

fn show_object_node(
    ui: &mut egui::Ui,
    node: &OutlinerNode,
    selected_id: Option<&str>,
    force_open: bool,
    depth: usize,
    clicked: &mut Option<String>,
) {
    let selected = node.object_id.as_deref() == selected_id;
    let label = format!("●  {}\n    {}", node.label, node.detail);
    if node.children.is_empty() {
        let response = ui
            .add_sized(
                [ui.available_width(), 38.0],
                egui::Button::selectable(selected, label),
            )
            .on_hover_text(format!("{}\n{}", node.detail, node.key));
        if response.clicked() {
            *clicked = node.object_id.clone();
        }
        return;
    }

    let id = ui.make_persistent_id(("outliner-related", &node.key));
    let mut header =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 2);
    if force_open {
        header.set_open(true);
    }
    let (_toggle, header, _body) = header
        .show_header(ui, |ui| {
            ui.add_sized(
                [ui.available_width(), 38.0],
                egui::Button::selectable(
                    selected,
                    format!(
                        "●  {}   {} related\n    {}",
                        node.label,
                        node.children.len(),
                        node.detail
                    ),
                ),
            )
        })
        .body(|ui| {
            for child in &node.children {
                show_outliner_node(ui, child, selected_id, force_open, depth + 1, clicked);
            }
        });
    if header.inner.clicked() {
        *clicked = node.object_id.clone();
    }
    header.response.on_hover_text(format!(
        "{}\nOwns {} related object(s)",
        node.detail,
        node.children.len()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use sms_formats::{JDramaLightMap, JDramaTransform};
    use sms_scene::{
        ActorPreview, PlacementAddress, PlacementBinding, StageArchiveEdits, Transform,
    };
    use std::path::PathBuf;

    fn document(objects: Vec<SceneObject>) -> StageDocument {
        StageDocument {
            stage_id: "bianco3".to_string(),
            base_root: PathBuf::from("."),
            assets: Vec::new(),
            objects,
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        }
    }

    fn find_object<'a>(nodes: &'a [OutlinerNode], id: &str) -> Option<&'a OutlinerNode> {
        nodes.iter().find_map(|node| {
            (node.object_id.as_deref() == Some(id))
                .then_some(node)
                .or_else(|| find_object(&node.children, id))
        })
    }

    #[test]
    fn manager_owned_actors_are_nested_under_the_closest_manager() {
        let mut far_manager = SceneObject::new("manager-far", "HamuKuriManager");
        far_manager.transform.translation = [1_000.0, 0.0, 0.0];
        let mut near_manager = SceneObject::new("manager-near", "HamuKuriManager");
        near_manager.transform.translation = [10.0, 0.0, 0.0];
        let mut actor = SceneObject::new("actor", "HamuKuri");
        actor.transform = Transform {
            translation: [12.0, 0.0, 0.0],
            ..Transform::default()
        };
        let mut document = document(vec![far_manager, near_manager, actor]);
        document.actor_previews.insert(
            "factory:HamuKuri".to_string(),
            ActorPreview {
                model_path: "/scene/hamukuri/hamukuri.bmd".to_string(),
                load_flags: 0,
                manager_factory: "HamuKuriManager".to_string(),
                runtime_uniform_scale: None,
            },
        );

        let tree = build_outliner_tree(&document, "");
        let near = find_object(&tree.roots, "manager-near").unwrap();
        let far = find_object(&tree.roots, "manager-far").unwrap();
        assert!(find_object(&near.children, "actor").is_some());
        assert!(far.children.is_empty());
        assert_eq!(tree.visible_objects, 3);
    }

    #[test]
    fn placement_groups_preserve_the_jdrama_record_hierarchy() {
        let raw_resource_path = b"map/scene.bin".to_vec();
        let mut coin = SceneObject::new("coin", "Coin");
        coin.placement = Some(PlacementBinding::Existing(PlacementAddress {
            raw_resource_path: raw_resource_path.clone(),
            record_path: vec![0, 0],
        }));
        let document = document(vec![coin]);
        let actor = JDramaRecord {
            type_name: "Coin".to_string(),
            name: "Coin 1".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: JDramaLightMap::default(),
                fields: Vec::new(),
            },
        };
        let root = JDramaRecord {
            type_name: "MarScene".to_string(),
            name: "Root".to_string(),
            payload: JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![JDramaRecord {
                    type_name: "TIdxGroupObj".to_string(),
                    name: "Map Objects".to_string(),
                    payload: JDramaRecordPayload::Group {
                        fields: Vec::new(),
                        children: vec![actor],
                    },
                }],
            },
        };

        let mut builder = OutlinerBuilder::new(&document);
        let nodes = builder.record_nodes(&raw_resource_path, &root, &mut Vec::new());

        assert_eq!(nodes[0].label, "Root");
        assert_eq!(nodes[0].children[0].label, "Map Objects");
        assert_eq!(
            nodes[0].children[0].children[0].object_id.as_deref(),
            Some("coin")
        );
    }

    #[test]
    fn filtering_keeps_matching_objects_and_their_ancestors() {
        let document = document(vec![
            SceneObject::new("coin", "Coin"),
            SceneObject::new("shine", "Shine"),
        ]);

        let tree = build_outliner_tree(&document, "shine");
        assert_eq!(tree.total_objects, 2);
        assert_eq!(tree.visible_objects, 1);
        assert!(find_object(&tree.roots, "shine").is_some());
        assert!(find_object(&tree.roots, "coin").is_none());
    }

    #[test]
    fn authored_placement_is_grouped_under_its_raw_resource() {
        let mut authored = SceneObject::new("authored", "FixtureActor");
        authored.placement = Some(PlacementBinding::Authored(sms_scene::AuthoredPlacement {
            raw_resource_path: b"map/scene.bin".to_vec(),
            target_group_index: 4,
            prototype: JDramaRecord {
                type_name: "FixtureActor".to_string(),
                name: "fixture actor".to_string(),
                payload: JDramaRecordPayload::Actor {
                    transform: JDramaTransform {
                        translation: [0.0; 3],
                        rotation: [0.0; 3],
                        scale: [1.0; 3],
                    },
                    character_name: String::new(),
                    light_map: JDramaLightMap::default(),
                    fields: Vec::new(),
                },
            },
            dependencies: Vec::new(),
        }));
        let tree = build_outliner_tree(&document(vec![authored]), "");
        let stage = &tree.roots[0];
        let resource = stage
            .children
            .iter()
            .find(|node| node.detail == "map/scene.bin")
            .expect("authored placement resource group");
        assert_eq!(resource.kind, OutlinerNodeKind::Resource);
        assert!(find_object(&resource.children, "authored").is_some());
    }
}
