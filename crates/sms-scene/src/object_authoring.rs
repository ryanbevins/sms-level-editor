use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sms_formats::{
    mount_scene_archive, parse_jdrama_object_records, read_stage_asset_bytes, JDramaDocument,
    JDramaField, JDramaFieldValue, JDramaRecord, JDramaRecordPayload, SceneArchiveInfo,
    StageAssetKind,
};
use sms_schema::{
    EnemyActorDefinition, EnemyManagerDefinition, ObjectDefinition, ObjectRegistry,
    ObjectResourceBinding,
};

use crate::AuthoredPlacementDependencyTarget;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectAuthoringCatalog {
    templates: BTreeMap<String, ObjectAuthoringTemplate>,
}

impl ObjectAuthoringCatalog {
    /// Builds retail-backed authoring templates without failing the whole census when an
    /// individual archive or placement document is unreadable.
    pub fn build(
        archives: &[SceneArchiveInfo],
        registry: &ObjectRegistry,
    ) -> ObjectAuthoringCatalogBuild {
        Self::build_with_common_scene_path(archives, registry, None)
    }

    /// Builds the retail catalog with the global `scenecmn.bin` character table
    /// available as the fallback after each source stage''s `map/tables.bin`.
    pub fn build_with_base_root(
        archives: &[SceneArchiveInfo],
        registry: &ObjectRegistry,
        base_root: impl AsRef<Path>,
    ) -> ObjectAuthoringCatalogBuild {
        let base_root = base_root.as_ref();
        let common_scene = [
            base_root.join("files/data/scenecmn.bin"),
            base_root.join("data/scenecmn.bin"),
            base_root.join("scenecmn.bin"),
        ]
        .into_iter()
        .find(|path| path.is_file());
        let mut build =
            Self::build_with_common_scene_path(archives, registry, common_scene.as_deref());
        if common_scene.is_none() {
            build.warnings.push(ObjectAuthoringCatalogWarning {
                source_stage: "<common>".to_string(),
                source_asset_path: None,
                message: format!(
                    "Could not find data/scenecmn.bin under {}; global character registrations are unavailable",
                    base_root.display()
                ),
            });
        }
        build
    }

    fn build_with_common_scene_path(
        archives: &[SceneArchiveInfo],
        registry: &ObjectRegistry,
        common_scene_path: Option<&Path>,
    ) -> ObjectAuthoringCatalogBuild {
        let mut warnings = Vec::new();
        let mut sources = Vec::new();
        let mut archives: Vec<_> = archives.iter().collect();
        archives.sort_by(|a, b| {
            a.stage_id
                .cmp(&b.stage_id)
                .then_with(|| a.relative_path.cmp(&b.relative_path))
                .then_with(|| a.path.cmp(&b.path))
        });
        for archive in archives {
            let mut assets = match mount_scene_archive(&archive.path) {
                Ok(assets) => assets,
                Err(error) => {
                    warnings.push(ObjectAuthoringCatalogWarning {
                        source_stage: archive.stage_id.clone(),
                        source_asset_path: Some(archive.path.clone()),
                        message: format!("Could not mount {}: {error}", archive.path.display()),
                    });
                    continue;
                }
            };
            assets.sort_by(|a, b| a.path.cmp(&b.path));
            let resources = assets
                .iter()
                .filter_map(|asset| {
                    Some(ObjectAuthoringResource {
                        raw_resource_path: archive_resource_path(&asset.path)?,
                        source_asset_path: asset.path.clone(),
                    })
                })
                .collect();
            let mut documents = Vec::new();
            for asset in assets.iter().filter(|asset| {
                asset.kind == StageAssetKind::Placement
                    && asset
                        .path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".bin")
            }) {
                let bytes = match read_stage_asset_bytes(&asset.path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        warnings.push(ObjectAuthoringCatalogWarning {
                            source_stage: archive.stage_id.clone(),
                            source_asset_path: Some(asset.path.clone()),
                            message: format!("Could not read {}: {error}", asset.path.display()),
                        });
                        continue;
                    }
                };
                let document = match JDramaDocument::parse(&bytes) {
                    Ok(document) => document,
                    Err(error) => {
                        warnings.push(ObjectAuthoringCatalogWarning {
                            source_stage: archive.stage_id.clone(),
                            source_asset_path: Some(asset.path.clone()),
                            message: format!(
                                "Could not parse strict placement document {}: {error}",
                                asset.path.display()
                            ),
                        });
                        continue;
                    }
                };
                let Some(raw_resource_path) = archive_resource_path(&asset.path) else {
                    continue;
                };
                documents.push(SourceDocument {
                    raw_resource_path,
                    source_asset_path: asset.path.clone(),
                    document,
                });
            }
            sources.push(CatalogSource {
                source_stage: archive.stage_id.clone(),
                sort_key: format!(
                    "{}\0{}\0{}",
                    archive.stage_id,
                    archive.relative_path.display(),
                    archive.path.display()
                ),
                documents,
                resources,
            });
        }
        let common_character_document = common_scene_path.and_then(|path| {
            let bytes = match read_stage_asset_bytes(path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    warnings.push(ObjectAuthoringCatalogWarning {
                        source_stage: "<common>".to_string(),
                        source_asset_path: Some(path.to_path_buf()),
                        message: format!("Could not read {}: {error}", path.display()),
                    });
                    return None;
                }
            };
            match parse_common_character_document(&bytes) {
                Ok((document, fallback_warning)) => {
                    if let Some(message) = fallback_warning {
                        warnings.push(ObjectAuthoringCatalogWarning {
                            source_stage: "<common>".to_string(),
                            source_asset_path: Some(path.to_path_buf()),
                            message,
                        });
                    }
                    Some(document)
                }
                Err(message) => {
                    warnings.push(ObjectAuthoringCatalogWarning {
                        source_stage: "<common>".to_string(),
                        source_asset_path: Some(path.to_path_buf()),
                        message: format!(
                            "Could not parse global character table {}: {message}",
                            path.display()
                        ),
                    });
                    None
                }
            }
        });
        let catalog = build_from_sources_with_common(
            &sources,
            registry,
            common_character_document.as_ref(),
            &mut warnings,
        );
        ObjectAuthoringCatalogBuild { catalog, warnings }
    }

    pub fn find(&self, factory_name: &str) -> Option<&ObjectAuthoringTemplate> {
        self.templates.get(factory_name)
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ObjectAuthoringTemplate)> {
        self.templates
            .iter()
            .map(|(name, item)| (name.as_str(), item))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectAuthoringCatalogBuild {
    pub catalog: ObjectAuthoringCatalog,
    pub warnings: Vec<ObjectAuthoringCatalogWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectAuthoringCatalogWarning {
    pub source_stage: String,
    pub source_asset_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectAuthoringTemplate {
    pub factory_name: String,
    pub group_index: u32,
    /// Exact retail record; name and transform are deliberately not reset here.
    pub record: JDramaRecord,
    pub dependencies: Vec<ObjectAuthoringDependency>,
    /// Exact `ObjChara`/`SmplChara` registrations required before this actor
    /// and its manager records are loaded.
    pub character_records: Vec<JDramaRecord>,
    /// Named records outside `map/scene.bin` that the actor reaches through
    /// fixed runtime lookups rather than serialized actor fields.
    pub table_dependencies: Vec<ObjectAuthoringTableDependency>,
    pub required_graph_names: Vec<String>,
    pub resources: Vec<ObjectAuthoringResource>,
    pub preview_resource_path: Option<Vec<u8>>,
    pub source_stage: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectAuthoringDependency {
    pub group_index: u32,
    pub target: AuthoredPlacementDependencyTarget,
    pub record: JDramaRecord,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectAuthoringTableDependency {
    pub target: AuthoredPlacementDependencyTarget,
    pub record: JDramaRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectAuthoringResource {
    pub raw_resource_path: Vec<u8>,
    pub source_asset_path: PathBuf,
}

#[derive(Debug, Clone)]
struct CatalogSource {
    source_stage: String,
    sort_key: String,
    documents: Vec<SourceDocument>,
    resources: Vec<ObjectAuthoringResource>,
}

#[derive(Debug, Clone)]
struct SourceDocument {
    raw_resource_path: Vec<u8>,
    source_asset_path: PathBuf,
    document: JDramaDocument,
}

pub const SHINE_QUICK_CAMERA_NAME: &str = "\u{30b7}\u{30e3}\u{30a4}\u{30f3}\u{ff08}\u{3044}\u{304d}\u{306a}\u{308a}\u{51fa}\u{73fe}\u{ff09}\u{30ab}\u{30e1}\u{30e9}";

struct RuntimeTableDependencySpec {
    record_type: &'static str,
    record_name: &'static str,
}

const SHINE_RUNTIME_TABLE_DEPENDENCIES: &[RuntimeTableDependencySpec] =
    &[RuntimeTableDependencySpec {
        record_type: "CameraMapInfo",
        record_name: SHINE_QUICK_CAMERA_NAME,
    }];

fn runtime_table_dependency_specs(factory_name: &str) -> &'static [RuntimeTableDependencySpec] {
    match semantic_type_name(factory_name) {
        "Shine" => SHINE_RUNTIME_TABLE_DEPENDENCIES,
        _ => &[],
    }
}

#[derive(Debug)]
struct Located<'a> {
    group_index: u32,
    path: Vec<usize>,
    record: &'a JDramaRecord,
}

#[derive(Debug)]
struct LocatedDependency<'a> {
    legacy_group_index: u32,
    target: AuthoredPlacementDependencyTarget,
    path: Vec<usize>,
    record: &'a JDramaRecord,
}

#[derive(Debug, Clone)]
struct Candidate {
    factory_name: String,
    group_index: u32,
    record: JDramaRecord,
    dependencies: Vec<ObjectAuthoringDependency>,
    character_records: Vec<JDramaRecord>,
    character_resource_records: Vec<JDramaRecord>,
    graph_names: BTreeSet<String>,
    source_stage: String,
    sort_key: String,
    raw_resource_path: Vec<u8>,
    source_asset_path: PathBuf,
    record_path: Vec<usize>,
}

#[derive(Debug)]
struct ResolvedCharacterRecords {
    target_records: Vec<JDramaRecord>,
    resource_records: Vec<JDramaRecord>,
}

fn resolve_runtime_table_dependencies(
    factory_name: &str,
    sources: &[&CatalogSource],
) -> Result<Vec<ObjectAuthoringTableDependency>, String> {
    fn collect_matches(
        parent: &JDramaRecord,
        spec: &RuntimeTableDependencySpec,
        out: &mut Vec<ObjectAuthoringTableDependency>,
    ) {
        let JDramaRecordPayload::Group { children, .. } = &parent.payload else {
            return;
        };
        for child in children {
            if semantic_type_name(&child.type_name) == spec.record_type
                && child.name == spec.record_name
            {
                out.push(ObjectAuthoringTableDependency {
                    target: AuthoredPlacementDependencyTarget::NamedGroup {
                        type_name: parent.type_name.clone(),
                        name: parent.name.clone(),
                    },
                    record: child.clone(),
                });
            }
            collect_matches(child, spec, out);
        }
    }

    let specs = runtime_table_dependency_specs(factory_name);
    let mut resolved = Vec::new();
    for spec in specs {
        let mut matches = Vec::new();
        for source in sources {
            for document in &source.documents {
                if normalized_path(&document.raw_resource_path) == "map/tables.bin" {
                    collect_matches(&document.document.root, spec, &mut matches);
                }
            }
        }
        let Some(first) = matches.first().cloned() else {
            return Err(format!(
                "retail catalog has no {} record named {:?} required by {factory_name}",
                spec.record_type, spec.record_name
            ));
        };
        if matches
            .iter()
            .any(|candidate| candidate.target != first.target || candidate.record != first.record)
        {
            return Err(format!(
                "retail catalog has incompatible {} records named {:?} required by {factory_name}",
                spec.record_type, spec.record_name
            ));
        }
        resolved.push(first);
    }
    Ok(resolved)
}

fn parse_common_character_document(
    bytes: &[u8],
) -> Result<(JDramaDocument, Option<String>), String> {
    let strict_error = match JDramaDocument::parse(bytes) {
        Ok(document) => return Ok((document, None)),
        Err(error) => error,
    };
    let loose_records = parse_jdrama_object_records(bytes).map_err(|loose_error| {
        format!(
            "strict parse failed ({strict_error}); tolerant NameRef parse also failed ({loose_error})"
        )
    })?;
    let mut character_records = Vec::new();
    for record in loose_records {
        let (field_name, value) = match semantic_type_name(&record.type_name) {
            "ObjChara" => (
                "resource_folder",
                record.obj_chara_folder.ok_or_else(|| {
                    format!(
                        "tolerant NameRef parse found ObjChara at {:#x} without its resource folder",
                        record.offset
                    )
                })?,
            ),
            "SmplChara" => (
                "archive_path",
                record.smpl_chara_archive_path.ok_or_else(|| {
                    format!(
                        "tolerant NameRef parse found SmplChara at {:#x} without its archive path",
                        record.offset
                    )
                })?,
            ),
            _ => continue,
        };
        let name = record.object_name.ok_or_else(|| {
            format!(
                "tolerant NameRef parse found {} at {:#x} without its NameRef identity",
                record.type_name, record.offset
            )
        })?;
        character_records.push(JDramaRecord {
            type_name: record.type_name,
            name,
            payload: JDramaRecordPayload::Fields {
                fields: vec![JDramaField {
                    name: field_name.to_string(),
                    value: JDramaFieldValue::String(value),
                }],
            },
        });
    }
    if character_records.is_empty() {
        return Err(format!(
            "strict parse failed ({strict_error}); tolerant NameRef parse found no ObjChara or SmplChara registrations"
        ));
    }
    let count = character_records.len();
    Ok((
        JDramaDocument {
            root: JDramaRecord {
                type_name: "NameRefGrp".to_string(),
                name: "global character registrations".to_string(),
                payload: JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: character_records,
                },
            },
        },
        Some(format!(
            "Strict global character-table parse failed ({strict_error}); recovered {count} exact ObjChara/SmplChara registration(s) with the tolerant NameRef parser"
        )),
    ))
}

#[cfg(test)]
fn build_from_sources(
    sources: &[CatalogSource],
    registry: &ObjectRegistry,
) -> ObjectAuthoringCatalog {
    let mut warnings = Vec::new();
    build_from_sources_with_common(sources, registry, None, &mut warnings)
}

fn build_from_sources_with_common(
    sources: &[CatalogSource],
    registry: &ObjectRegistry,
    common_character_document: Option<&JDramaDocument>,
    warnings: &mut Vec<ObjectAuthoringCatalogWarning>,
) -> ObjectAuthoringCatalog {
    let mut sources: Vec<_> = sources.iter().collect();
    sources.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    let mut candidates: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for source in &sources {
        let mut documents: Vec<_> = source.documents.iter().collect();
        documents.sort_by(|a, b| a.raw_resource_path.cmp(&b.raw_resource_path));
        for document in documents {
            if normalized_path(&document.raw_resource_path) != "map/scene.bin" {
                continue;
            }
            let located = located_records(&document.document);
            for item in &located {
                if let Some(reason) = known_unmodeled_runtime_dependency(item.record) {
                    if find_object(registry, &item.record.type_name).is_some() {
                        warnings.push(ObjectAuthoringCatalogWarning {
                            source_stage: source.source_stage.clone(),
                            source_asset_path: Some(document.source_asset_path.clone()),
                            message: format!(
                                "Omitted unsafe authoring candidate '{}': {reason}",
                                semantic_type_name(&item.record.type_name)
                            ),
                        });
                    }
                    continue;
                }
                let Some(definition) = authorable_definition(registry, item.record) else {
                    continue;
                };
                let factory_name = definition.factory_name.clone();
                let dependencies = match resolve_dependencies(
                    item.record,
                    &factory_name,
                    source,
                    &document.raw_resource_path,
                    registry,
                ) {
                    Ok(dependencies) => dependencies,
                    Err(reason) => {
                        warnings.push(ObjectAuthoringCatalogWarning {
                            source_stage: source.source_stage.clone(),
                            source_asset_path: Some(document.source_asset_path.clone()),
                            message: format!(
                                "Omitted unsafe authoring candidate '{factory_name}': {reason}"
                            ),
                        });
                        continue;
                    }
                };
                let character_records = match resolve_character_records(
                    item.record,
                    &dependencies,
                    source,
                    common_character_document,
                    registry,
                ) {
                    Ok(records) => records,
                    Err(reason) => {
                        warnings.push(ObjectAuthoringCatalogWarning {
                            source_stage: source.source_stage.clone(),
                            source_asset_path: Some(document.source_asset_path.clone()),
                            message: format!(
                                "Omitted unsafe authoring candidate '{factory_name}': {reason}"
                            ),
                        });
                        continue;
                    }
                };
                let graph_names = std::iter::once(item.record)
                    .chain(dependencies.iter().map(|dependency| &dependency.record))
                    .filter_map(|record| reference_field(record, "graph_name"))
                    .map(str::to_owned)
                    .collect();
                candidates
                    .entry(factory_name.clone())
                    .or_default()
                    .push(Candidate {
                        factory_name: factory_name.clone(),
                        group_index: item.group_index,
                        record: item.record.clone(),
                        dependencies,
                        character_resource_records: character_records.resource_records,
                        character_records: character_records.target_records,
                        graph_names,
                        source_stage: source.source_stage.clone(),
                        sort_key: source.sort_key.clone(),
                        raw_resource_path: document.raw_resource_path.clone(),
                        source_asset_path: document.source_asset_path.clone(),
                        record_path: item.path.clone(),
                    });
            }
        }
    }

    let mut templates = BTreeMap::new();
    for (factory_name, factory_candidates) in candidates {
        let mut variants: BTreeMap<(u32, Vec<u8>), Vec<Candidate>> = BTreeMap::new();
        for candidate in factory_candidates {
            variants
                .entry((candidate.group_index, modal_signature(&candidate.record)))
                .or_default()
                .push(candidate);
        }
        let mut winner: Option<(usize, Candidate)> = None;
        for (_, mut variant) in variants {
            variant.sort_by(compare_candidate);
            let count = variant.len();
            let candidate = variant.remove(0);
            if winner.as_ref().is_none_or(|(best_count, best)| {
                count > *best_count
                    || (count == *best_count && compare_candidate(&candidate, best).is_lt())
            }) {
                winner = Some((count, candidate));
            }
        }
        let Some((_, candidate)) = winner else {
            continue;
        };
        let resources = match resolve_resources(&candidate, &sources, registry) {
            Ok(resources) => resources,
            Err(reason) => {
                warnings.push(ObjectAuthoringCatalogWarning {
                    source_stage: candidate.source_stage.clone(),
                    source_asset_path: Some(candidate.source_asset_path.clone()),
                    message: format!(
                        "Omitted unsafe authoring candidate '{}': {reason}",
                        candidate.factory_name
                    ),
                });
                continue;
            }
        };
        let table_dependencies =
            match resolve_runtime_table_dependencies(&candidate.factory_name, &sources) {
                Ok(dependencies) => dependencies,
                Err(reason) => {
                    warnings.push(ObjectAuthoringCatalogWarning {
                        source_stage: candidate.source_stage.clone(),
                        source_asset_path: Some(candidate.source_asset_path.clone()),
                        message: format!(
                            "Omitted unsafe authoring candidate '{}': {reason}",
                            candidate.factory_name
                        ),
                    });
                    continue;
                }
            };
        let preview_resource_path = preview_resource_path(&candidate, &resources, registry);
        templates.insert(
            factory_name,
            ObjectAuthoringTemplate {
                factory_name: candidate.factory_name,
                group_index: candidate.group_index,
                record: candidate.record,
                dependencies: candidate.dependencies,
                character_records: candidate.character_records,
                table_dependencies,
                required_graph_names: candidate.graph_names.into_iter().collect(),
                resources,
                preview_resource_path,
                source_stage: candidate.source_stage,
            },
        );
    }
    ObjectAuthoringCatalog { templates }
}

fn located_records(document: &JDramaDocument) -> Vec<Located<'_>> {
    fn visit<'a>(
        record: &'a JDramaRecord,
        in_strategy: bool,
        group: Option<u32>,
        path: &mut Vec<usize>,
        out: &mut Vec<Located<'a>>,
    ) {
        let short = semantic_type_name(&record.type_name);
        let in_strategy = in_strategy || matches!(short, "Strategy" | "TStrategy");
        let group = if matches!(short, "IdxGroup" | "TIdxGroup") {
            record_group_index(record).or(group)
        } else {
            group
        };
        if in_strategy {
            if let Some(group_index) = group {
                out.push(Located {
                    group_index,
                    path: path.clone(),
                    record,
                });
            }
        }
        if let JDramaRecordPayload::Group { children, .. } = &record.payload {
            for (index, child) in children.iter().enumerate() {
                path.push(index);
                visit(child, in_strategy, group, path, out);
                path.pop();
            }
        }
    }
    let mut out = Vec::new();
    visit(&document.root, false, None, &mut Vec::new(), &mut out);
    out
}

fn located_dependency_records(document: &JDramaDocument) -> Vec<LocatedDependency<'_>> {
    fn parent_target(parent: &JDramaRecord) -> Option<(u32, AuthoredPlacementDependencyTarget)> {
        if !matches!(parent.payload, JDramaRecordPayload::Group { .. }) {
            return None;
        }
        if semantic_type_name(&parent.type_name) == "IdxGroup" {
            let group_index = record_group_index(parent)?;
            return Some((
                group_index,
                AuthoredPlacementDependencyTarget::IndexedGroup { group_index },
            ));
        }
        Some((
            0,
            AuthoredPlacementDependencyTarget::NamedGroup {
                type_name: semantic_type_name(&parent.type_name).to_string(),
                name: parent.name.clone(),
            },
        ))
    }

    fn visit<'a>(
        parent: &'a JDramaRecord,
        path: &mut Vec<usize>,
        out: &mut Vec<LocatedDependency<'a>>,
    ) {
        let JDramaRecordPayload::Group { children, .. } = &parent.payload else {
            return;
        };
        let target = parent_target(parent);
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            if let Some((legacy_group_index, target)) = &target {
                out.push(LocatedDependency {
                    legacy_group_index: *legacy_group_index,
                    target: target.clone(),
                    path: path.clone(),
                    record: child,
                });
            }
            visit(child, path, out);
            path.pop();
        }
    }

    let mut out = Vec::new();
    visit(&document.root, &mut Vec::new(), &mut out);
    out
}

fn record_group_index(record: &JDramaRecord) -> Option<u32> {
    let JDramaRecordPayload::Group { fields, .. } = &record.payload else {
        return None;
    };
    fields.iter().find_map(|field| match field.value {
        JDramaFieldValue::U32(value) if field.name == "group_index" => Some(value),
        _ => None,
    })
}

fn authorable_definition<'a>(
    registry: &'a ObjectRegistry,
    record: &JDramaRecord,
) -> Option<&'a ObjectDefinition> {
    let editable = matches!(&record.payload, JDramaRecordPayload::Actor { .. })
        || matches!(&record.payload, JDramaRecordPayload::Fields { fields }
            if field_transform_names(&record.type_name).is_some_and(|names|
                vec3_field(fields, names.0).is_some()
                && vec3_field(fields, names.1).is_some()
                && vec3_field(fields, names.2).is_some()));
    if !editable {
        return None;
    }
    let definition = find_object(registry, &record.type_name)?;
    let factory = semantic_type_name(&definition.factory_name);
    let class = semantic_type_name(&definition.class_name);
    (!definition.unsafe_to_edit
        && !factory.ends_with("Manager")
        && !class.ends_with("Manager")
        && !factory.ends_with("Director")
        && !class.ends_with("Director")
        && find_enemy_manager(registry, factory).is_none())
    .then_some(definition)
}

fn known_unmodeled_runtime_dependency(record: &JDramaRecord) -> Option<&'static str> {
    match semantic_type_name(&record.type_name) {
        "SwitchHelp" | "BalloonHelp" => Some(
            "target_actor_name is an unconditional live-actor dependency that is not yet transplanted",
        ),
        "LeanMirror" => Some(
            "retail unconditionally requires the hardcoded ShiningStone live actor, which is not yet transplanted",
        ),
        _ => None,
    }
}

fn resolve_dependencies(
    actor: &JDramaRecord,
    actor_factory: &str,
    source: &CatalogSource,
    candidate_resource_path: &[u8],
    registry: &ObjectRegistry,
) -> Result<Vec<ObjectAuthoringDependency>, String> {
    let mut manager_names = reference_field(actor, "manager_name")
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if semantic_type_name(&actor.type_name) == "CommonLauncher" {
        if let Some(name) = reference_field(actor, "launched_enemy_name") {
            manager_names.insert(name.to_owned());
        }
    }
    if manager_names.is_empty() {
        manager_names.extend(
            reference_field(actor, "resource_name")
                .and_then(|resource_name| registry.find_map_obj_resource(resource_name))
                .map(|resource| resource.required_manager_name.as_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        );
    }
    if manager_names.is_empty() {
        return Ok(Vec::new());
    }
    let enemy = find_enemy_actor(registry, actor_factory);
    let mut dependencies = Vec::new();
    for name in manager_names {
        let mut found: Vec<_> = source
            .documents
            .iter()
            .flat_map(|document| {
                let raw_path = document.raw_resource_path.clone();
                located_dependency_records(&document.document)
                    .into_iter()
                    .map(move |item| (raw_path.clone(), item))
            })
            .filter(|(_, item)| item.record.name == name)
            .filter(|(_, item)| {
                let factory = semantic_type_name(&item.record.type_name);
                enemy.is_some_and(|enemy| {
                    enemy
                        .manager_factories
                        .iter()
                        .any(|expected| semantic_type_name(expected) == factory)
                }) || (enemy.is_none()
                    && (factory.ends_with("Manager")
                        || find_enemy_manager(registry, factory).is_some()))
            })
            .map(|(raw_path, item)| {
                (
                    raw_path,
                    item.path.clone(),
                    ObjectAuthoringDependency {
                        group_index: item.legacy_group_index,
                        target: item.target.clone(),
                        record: item.record.clone(),
                    },
                )
            })
            .collect();
        if found
            .iter()
            .any(|(raw_path, _, _)| raw_path == candidate_resource_path)
        {
            found.retain(|(raw_path, _, _)| raw_path == candidate_resource_path);
        }
        found.sort_by(|a, b| {
            a.2.target
                .cmp(&b.2.target)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        match found.len() {
            1 => dependencies.push(found.remove(0).2),
            0 => {
                return Err(format!(
                    "required manager {name:?} has no exact compatible record in source stage {}",
                    source.source_stage
                ));
            }
            count => {
                return Err(format!(
                    "required manager {name:?} has {count} exact compatible records in source stage {}; a unique dependency cannot be authored safely",
                    source.source_stage
                ));
            }
        }
    }
    dependencies.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then_with(|| a.record.name.cmp(&b.record.name))
            .then_with(|| a.record.type_name.cmp(&b.record.type_name))
    });
    dependencies.dedup_by(|a, b| a.target == b.target && a.record == b.record);
    Ok(dependencies)
}

fn resolve_character_records(
    actor: &JDramaRecord,
    dependencies: &[ObjectAuthoringDependency],
    source: &CatalogSource,
    common_character_document: Option<&JDramaDocument>,
    registry: &ObjectRegistry,
) -> Result<ResolvedCharacterRecords, String> {
    let mut names = BTreeSet::new();
    if !uses_resolved_primary_manager(actor, dependencies, registry) {
        if let Some(name) = record_character_name(actor) {
            names.insert(name.to_string());
        }
    }
    for dependency in dependencies {
        if let Some(name) = record_character_name(&dependency.record) {
            names.insert(name.to_string());
        }
    }

    let mut target_records = Vec::new();
    let mut resource_records = Vec::new();
    for name in names {
        let mut local = source
            .documents
            .iter()
            .filter(|document| normalized_path(&document.raw_resource_path) == "map/tables.bin")
            .flat_map(|document| {
                records_with_paths(&document.document)
                    .into_iter()
                    .map(move |(path, record)| {
                        (document.raw_resource_path.as_slice(), path, record)
                    })
            })
            .filter(|(_, _, record)| record.name == name && is_character_registration(record))
            .collect::<Vec<_>>();
        local.sort_by(|a, b| {
            a.0.cmp(b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.type_name.cmp(&b.2.type_name))
                .then_with(|| modal_signature(a.2).cmp(&modal_signature(b.2)))
        });
        if let Some((_, _, first)) = local.first() {
            if local
                .iter()
                .any(|(_, _, record)| !same_character_registration(first, record))
            {
                return Err(format!(
                    "character {name:?} has conflicting registrations in source-stage map/tables.bin"
                ));
            }
            let record = (*first).clone();
            target_records.push(record.clone());
            resource_records.push(record);
            continue;
        }

        let mut common = common_character_document
            .into_iter()
            .flat_map(records_with_paths)
            .filter(|(_, record)| record.name == name && is_character_registration(record))
            .collect::<Vec<_>>();
        common.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.type_name.cmp(&b.1.type_name))
                .then_with(|| modal_signature(a.1).cmp(&modal_signature(b.1)))
        });
        let Some((_, first)) = common.first() else {
            return Err(format!(
                "required character registration {name:?} was not found in source-stage map/tables.bin or global scenecmn.bin"
            ));
        };
        if common
            .iter()
            .any(|(_, record)| !same_character_registration(first, record))
        {
            return Err(format!(
                "character {name:?} has conflicting registrations in global scenecmn.bin"
            ));
        }
        // Global `scenecmn.bin` is retained by managed builds and loads before
        // stage managers, so no target-local duplicate is necessary. Its
        // referenced assets remain stage-local and must still be transplanted.
        resource_records.push((*first).clone());
    }
    Ok(ResolvedCharacterRecords {
        target_records,
        resource_records,
    })
}

fn uses_resolved_primary_manager(
    actor: &JDramaRecord,
    dependencies: &[ObjectAuthoringDependency],
    registry: &ObjectRegistry,
) -> bool {
    // `TLiveActor::init(manager)` creates its MActor from the manager's model
    // keeper and never reads the `TActor` character pointer. `TMapObjBase`
    // similarly obtains its manager from the decomp-derived map-object table.
    // Only suppress the actor character lookup after that exact primary
    // manager dependency has actually been resolved; standalone actors still
    // require their own ObjChara/SmplChara registration.
    if let Some(name) = reference_field(actor, "manager_name") {
        return dependencies
            .iter()
            .any(|dependency| dependency.record.name == name);
    }

    let Some(resource_name) = reference_field(actor, "resource_name") else {
        return false;
    };
    let Some(resource) = registry.find_map_obj_resource(resource_name) else {
        return false;
    };
    !resource.required_manager_name.is_empty()
        && dependencies
            .iter()
            .any(|dependency| dependency.record.name == resource.required_manager_name)
}

fn same_character_registration(left: &JDramaRecord, right: &JDramaRecord) -> bool {
    semantic_type_name(&left.type_name) == semantic_type_name(&right.type_name)
        && left.name == right.name
        && left.payload == right.payload
}

fn records_with_paths(document: &JDramaDocument) -> Vec<(Vec<usize>, &JDramaRecord)> {
    fn visit<'a>(
        record: &'a JDramaRecord,
        path: &mut Vec<usize>,
        out: &mut Vec<(Vec<usize>, &'a JDramaRecord)>,
    ) {
        out.push((path.clone(), record));
        if let JDramaRecordPayload::Group { children, .. } = &record.payload {
            for (index, child) in children.iter().enumerate() {
                path.push(index);
                visit(child, path, out);
                path.pop();
            }
        }
    }
    let mut out = Vec::new();
    visit(&document.root, &mut Vec::new(), &mut out);
    out
}

fn is_character_registration(record: &JDramaRecord) -> bool {
    matches!(
        semantic_type_name(&record.type_name),
        "ObjChara" | "SmplChara"
    )
}

fn resolve_resources(
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
) -> Result<Vec<ObjectAuthoringResource>, String> {
    let mut out = BTreeSet::new();
    for registration in &candidate.character_resource_records {
        let folder = match semantic_type_name(&registration.type_name) {
            "ObjChara" => reference_field(registration, "resource_folder"),
            "SmplChara" => reference_field(registration, "archive_path"),
            _ => None,
        };
        if let Some(folder) = folder {
            add_stage_folder(
                &mut out,
                sources,
                &candidate.source_stage,
                folder,
                &format!("character {} folder", registration.name),
            )?;
        }
    }
    for dependency in &candidate.dependencies {
        let Some(manager) = find_enemy_manager(registry, &dependency.record.type_name) else {
            continue;
        };
        if let Some(path) = manager.parameter_path.as_deref() {
            // TParams::load keeps the constructor's PARAM_INIT defaults when
            // neither the stage override nor the global params archive has
            // this file. Copy an exact stage override when one exists, but do
            // not make its absence a catalog blocker.
            add_stage_reference(
                &mut out,
                sources,
                &candidate.source_stage,
                path,
                &format!("manager parameter for {}", dependency.record.name),
                false,
            )?;
        }
        for animation in registry
            .enemy_manager_animation_folders
            .iter()
            .filter(|animation| animation.factory_name == manager.factory_name)
        {
            add_stage_folder(
                &mut out,
                sources,
                &candidate.source_stage,
                &animation.folder,
                &format!("manager animation folder for {}", dependency.record.name),
            )?;
        }
    }
    if !candidate.graph_names.is_empty() {
        add_stage_reference(
            &mut out,
            sources,
            &candidate.source_stage,
            "map/scene.ral",
            "rail graph document",
            true,
        )?;
    }

    let mut runtime_factories = BTreeSet::from([candidate.factory_name.clone()]);
    let mut runtime_classes = BTreeSet::new();
    if let Some(definition) = find_object(registry, &candidate.factory_name) {
        runtime_classes.insert(definition.class_name.clone());
    }
    for dependency in &candidate.dependencies {
        if let Some(definition) = find_object(registry, &dependency.record.type_name) {
            runtime_factories.insert(definition.factory_name.clone());
            runtime_classes.insert(definition.class_name.clone());
        }
    }
    for factory_name in &runtime_factories {
        let bindings = registry
            .object_resources_for(factory_name)
            .collect::<Vec<_>>();
        let bound_names = bindings
            .iter()
            .map(|binding| normalize_text_path(&binding.model_name))
            .collect::<BTreeSet<_>>();
        for binding in bindings {
            add_stage_reference(
                &mut out,
                sources,
                &candidate.source_stage,
                &object_resource_path(binding),
                &format!("{} model {}", factory_name, binding.model_index),
                false,
            )?;
        }
        if let Some(manager) = find_enemy_manager(registry, factory_name) {
            for model in &manager.models {
                if !bound_names.contains(&normalize_text_path(&model.model_name)) {
                    add_stage_reference(
                        &mut out,
                        sources,
                        &candidate.source_stage,
                        &model.model_name,
                        &format!("{factory_name} manager model"),
                        false,
                    )?;
                }
            }
        }
    }

    resolve_runtime_map_obj_dependencies(
        &mut out,
        candidate,
        sources,
        registry,
        &runtime_factories,
    )?;

    if let Some(actor) = find_enemy_actor(registry, &candidate.factory_name) {
        let mut paths = BTreeSet::new();
        paths.extend(actor.primary_model.iter().cloned());
        paths.extend(
            actor
                .fallback_models
                .iter()
                .map(|model| model.model_name.clone()),
        );
        paths.extend(
            actor
                .named_models
                .iter()
                .map(|model| model.model_path.clone()),
        );
        paths.extend(
            actor
                .indexed_models
                .iter()
                .map(|model| model.model_path.clone()),
        );
        let bound_names = registry
            .object_resources_for(&candidate.factory_name)
            .map(|binding| normalize_text_path(&binding.model_name))
            .collect::<BTreeSet<_>>();
        for path in paths {
            if !bound_names.contains(&normalize_text_path(&path)) {
                add_stage_reference(
                    &mut out,
                    sources,
                    &candidate.source_stage,
                    &path,
                    &format!("{} enemy model", candidate.factory_name),
                    false,
                )?;
            }
        }
    }

    resolve_map_static_resources(&mut out, candidate, sources, registry)?;
    resolve_map_obj_resources(&mut out, candidate, sources, registry)?;
    resolve_map_obj_flag_resources(&mut out, candidate, sources, registry)?;
    resolve_particle_resources(&mut out, candidate, sources, registry, &runtime_classes)?;
    add_preview_resources(&mut out, candidate, sources, registry);
    ensure_unique_resource_paths(&out, "runtime resource closure")?;
    Ok(out.into_iter().collect())
}

fn object_resource_path(binding: &ObjectResourceBinding) -> String {
    if binding.model_name.starts_with(['/', '\\'])
        || normalize_text_path(&binding.model_name).contains('/')
    {
        return binding.model_name.clone();
    }
    binding
        .resource_base
        .as_deref()
        .map(|base| {
            format!(
                "{}/{}",
                base.trim_end_matches(['/', '\\']),
                binding.model_name.trim_start_matches(['/', '\\'])
            )
        })
        .unwrap_or_else(|| binding.model_name.clone())
}

fn resolve_map_static_resources(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
) -> Result<(), String> {
    let Some(actor_name) = reference_field(&candidate.record, "resource_name") else {
        return Ok(());
    };
    let paths = registry
        .map_static_models
        .iter()
        .filter(|model| model.actor_name == actor_name)
        .filter_map(|model| model.model_path.as_deref())
        .collect::<BTreeSet<_>>();
    if paths.len() > 1 {
        return Err(format!(
            "map-static actor {actor_name:?} has conflicting exact model paths in the schema"
        ));
    }
    if let Some(path) = paths.into_iter().next() {
        add_stage_reference(
            out,
            sources,
            &candidate.source_stage,
            path,
            &format!("map-static actor {actor_name}"),
            false,
        )?;
    }
    Ok(())
}

fn resolve_map_obj_resources(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
) -> Result<(), String> {
    let Some(resource_name) = reference_field(&candidate.record, "resource_name") else {
        return Ok(());
    };
    let Some(resource) = registry.find_map_obj_resource(resource_name) else {
        return Ok(());
    };
    resolve_map_obj_resource_definition(
        out,
        sources,
        &candidate.source_stage,
        registry,
        &candidate.factory_name,
        resource,
        false,
    )
}

fn resolve_runtime_map_obj_dependencies(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
    runtime_factories: &BTreeSet<String>,
) -> Result<(), String> {
    for dependency in registry
        .runtime_map_obj_dependencies
        .iter()
        .filter(|dependency| runtime_factories.contains(&dependency.factory_name))
    {
        let resource = registry
            .find_map_obj_resource(&dependency.resource_name)
            .ok_or_else(|| {
                format!(
                    "runtime factory {} instantiates unknown map-object resource {:?}",
                    dependency.factory_name, dependency.resource_name
                )
            })?;
        let factory_name = registry
            .find_object(&dependency.resource_name)
            .map(|object| object.factory_name.as_str())
            .unwrap_or(dependency.resource_name.as_str());
        resolve_map_obj_resource_definition(
            out,
            sources,
            &candidate.source_stage,
            registry,
            factory_name,
            resource,
            true,
        )?;
    }
    Ok(())
}

fn resolve_map_obj_resource_definition(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    sources: &[&CatalogSource],
    stage: &str,
    registry: &ObjectRegistry,
    factory_name: &str,
    resource: &sms_schema::MapObjResourceDefinition,
    required_stage_local: bool,
) -> Result<(), String> {
    let resource_name = &resource.resource_name;
    let model = registry
        .find_map_obj_model_override(factory_name, resource_name)
        .map(|definition| definition.model_path.as_str())
        .or(resource.primary_model.as_deref());
    if let Some(model) = model {
        add_map_obj_reference(
            out,
            sources,
            stage,
            model,
            "map-object primary model",
            required_stage_local,
        )?;
    }
    for animation in &resource.animation_resources {
        if let Some(model) = animation.model_name.as_deref() {
            add_map_obj_reference(
                out,
                sources,
                stage,
                model,
                "map-object animation model",
                required_stage_local,
            )?;
        }
        if animation
            .extra_name
            .as_deref()
            .is_some_and(|name| !name.is_empty())
        {
            return Err(format!("map-object resource {resource_name:?} has unresolved animation extra_name metadata"));
        }
        if let Some(name) = animation.animation_name.as_deref() {
            add_map_obj_animation_stem(out, sources, stage, name)?;
        }
        if let Some(path) = animation.bas_path.as_deref() {
            add_stage_reference(
                out,
                sources,
                stage,
                path,
                "map-object animation sound",
                required_stage_local,
            )?;
        }
    }
    if resource.has_hold_dependency && resource.hold_model_path.is_none() {
        return Err(format!(
            "map-object resource {resource_name:?} has a hold dependency without an exact model path"
        ));
    }
    if let Some(path) = resource.hold_model_path.as_deref() {
        add_stage_reference(
            out,
            sources,
            stage,
            path,
            "map-object hold model",
            required_stage_local,
        )?;
    }
    if resource.has_move_dependency && resource.move_bck_path.is_none() {
        return Err(format!(
            "map-object resource {resource_name:?} has a move dependency without an exact BCK path"
        ));
    }
    if let Some(path) = resource.move_bck_path.as_deref() {
        add_stage_reference(
            out,
            sources,
            stage,
            path,
            "map-object move animation",
            required_stage_local,
        )?;
    }
    for collision in &resource.collision_resources {
        let name = if collision
            .resource_name
            .to_ascii_lowercase()
            .ends_with(".col")
        {
            collision.resource_name.clone()
        } else {
            format!("{}.col", collision.resource_name)
        };
        add_map_obj_reference(
            out,
            sources,
            stage,
            &name,
            "map-object collision",
            required_stage_local,
        )?;
    }
    Ok(())
}
fn resolve_map_obj_flag_resources(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
) -> Result<(), String> {
    for flag in registry
        .map_obj_flags
        .iter()
        .filter(|flag| flag.factory_name == candidate.factory_name)
    {
        let selector = reference_field(&candidate.record, "resource_name")
            .filter(|selector| !selector.is_empty())
            .ok_or_else(|| "map-object flag has no exact resource_name selector".to_string())?;
        if !flag
            .registered_texture_names
            .iter()
            .any(|name| name == selector)
        {
            return Err(format!(
                "map-object flag selector {selector:?} is not registered by the retail flag table"
            ));
        }
        if flag.texture_path_pattern.match_indices("%s").count() != 1 {
            return Err(format!(
                "map-object flag texture pattern {:?} is not safe to instantiate",
                flag.texture_path_pattern
            ));
        }
        let path = flag.texture_path_pattern.replacen("%s", selector, 1);
        add_stage_reference(
            out,
            sources,
            &candidate.source_stage,
            &path,
            "map-object flag texture",
            false,
        )?;
    }
    Ok(())
}

fn resolve_particle_resources(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
    class_names: &BTreeSet<String>,
) -> Result<(), String> {
    let effect_ids = registry
        .actor_particle_bindings
        .iter()
        .filter(|binding| class_names.contains(&binding.class_name))
        .map(|binding| binding.effect_id)
        .collect::<BTreeSet<_>>();
    for effect_id in effect_ids {
        let paths = registry
            .particle_resources
            .iter()
            .filter(|resource| resource.effect_id == effect_id)
            .map(|resource| resource.path.as_str())
            .collect::<BTreeSet<_>>();
        if paths.len() != 1 {
            return Err(format!(
                "particle effect {effect_id} used by {} has {} exact JPA paths in the schema",
                candidate.factory_name,
                paths.len()
            ));
        }
        add_stage_reference(
            out,
            sources,
            &candidate.source_stage,
            paths.into_iter().next().expect("one path checked above"),
            &format!("particle effect {effect_id}"),
            false,
        )?;
    }
    Ok(())
}

fn add_map_obj_reference(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    sources: &[&CatalogSource],
    stage: &str,
    reference: &str,
    description: &str,
    required_stage_local: bool,
) -> Result<(), String> {
    let normalized = normalize_runtime_reference(reference);
    let path = if normalized.contains('/') {
        reference.to_string()
    } else {
        format!("/scene/mapObj/{reference}")
    };
    add_stage_reference(
        out,
        sources,
        stage,
        &path,
        description,
        required_stage_local,
    )
}

fn add_map_obj_animation_stem(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    sources: &[&CatalogSource],
    stage: &str,
    animation_name: &str,
) -> Result<(), String> {
    let normalized = normalize_runtime_reference(animation_name);
    let stem = if normalized.contains('/') {
        normalized
    } else {
        format!("mapobj/{normalized}")
    };
    let mut by_path: BTreeMap<String, Vec<&ObjectAuthoringResource>> = BTreeMap::new();
    for resource in stage_resources(sources, stage) {
        let path = normalized_path(&resource.raw_resource_path);
        let Some((path_stem, extension)) = path.rsplit_once('.') else {
            continue;
        };
        if path_stem == stem
            && matches!(
                extension,
                "bck" | "bpk" | "btp" | "btk" | "brk" | "blk" | "bva"
            )
        {
            by_path.entry(path).or_default().push(resource);
        }
    }
    for (path, matches) in by_path {
        if matches.len() != 1 {
            return Err(format!(
                "map-object animation {animation_name:?} resolves ambiguously to {path} in source stage {stage}"
            ));
        }
        out.insert(matches[0].clone());
    }
    Ok(())
}

fn add_stage_folder(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    sources: &[&CatalogSource],
    stage: &str,
    folder: &str,
    description: &str,
) -> Result<(), String> {
    let matches = stage_resources(sources, stage)
        .into_iter()
        .filter(|resource| is_under_folder(&resource.raw_resource_path, folder))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        if normalize_text_path(folder).starts_with("common/") {
            return Ok(());
        }
        return Err(format!(
            "required stage-local {description} {folder:?} has no resources in source stage {stage}"
        ));
    }
    for resource in matches {
        out.insert(resource.clone());
    }
    ensure_unique_resource_paths(out, description)
}

fn add_stage_reference(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    sources: &[&CatalogSource],
    stage: &str,
    reference: &str,
    description: &str,
    required_stage_local: bool,
) -> Result<(), String> {
    let expected = normalize_runtime_reference(reference);
    let exact = expected.contains('/');
    let matches = stage_resources(sources, stage)
        .into_iter()
        .filter(|resource| {
            let path = normalized_path(&resource.raw_resource_path);
            if exact {
                path == expected || path.ends_with(&format!("/{expected}"))
            } else {
                path.rsplit('/').next() == Some(expected.as_str())
            }
        })
        .collect::<Vec<_>>();
    match matches.len() {
        0 if required_stage_local => Err(format!(
            "required stage-local {description} {reference:?} was not found in source stage {stage}"
        )),
        0 => Ok(()),
        1 => {
            out.insert(matches[0].clone());
            Ok(())
        }
        count => Err(format!(
            "{description} {reference:?} matched {count} resources in source stage {stage}; an exact source cannot be selected safely"
        )),
    }
}

fn stage_resources<'a>(
    sources: &'a [&CatalogSource],
    stage: &str,
) -> Vec<&'a ObjectAuthoringResource> {
    sources
        .iter()
        .filter(|source| source.source_stage == stage)
        .flat_map(|source| source.resources.iter())
        .collect()
}

fn ensure_unique_resource_paths(
    resources: &BTreeSet<ObjectAuthoringResource>,
    description: &str,
) -> Result<(), String> {
    let mut paths = BTreeMap::<String, usize>::new();
    for resource in resources {
        *paths
            .entry(normalized_path(&resource.raw_resource_path))
            .or_default() += 1;
    }
    if let Some((path, count)) = paths.into_iter().find(|(_, count)| *count > 1) {
        return Err(format!(
            "{description} contains {count} distinct sources for exact archive path {path:?}"
        ));
    }
    Ok(())
}

fn add_preview_resources(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    candidate: &Candidate,
    sources: &[&CatalogSource],
    registry: &ObjectRegistry,
) {
    let mut model_names = BTreeSet::new();
    let mut collision_names = BTreeSet::new();
    if let Some(binding) = registry.primary_object_resource(&candidate.factory_name) {
        model_names.insert(binding.model_name.to_ascii_lowercase());
    }
    if let Some(actor) = find_enemy_actor(registry, &candidate.factory_name) {
        if let Some(name) = &actor.primary_model {
            model_names.insert(name.to_ascii_lowercase());
        }
        model_names.extend(
            actor
                .fallback_models
                .iter()
                .map(|model| model.model_name.to_ascii_lowercase()),
        );
    }
    if let Some(resource_name) = reference_field(&candidate.record, "resource_name") {
        let override_model = registry
            .find_map_obj_model_override(&candidate.factory_name, resource_name)
            .map(|definition| definition.model_path.as_str());
        if let Some(name) = override_model {
            model_names.insert(name.to_ascii_lowercase());
        } else if let Some(name) = registry
            .find_map_obj_resource(resource_name)
            .and_then(|resource| resource.primary_model.as_deref())
        {
            model_names.insert(name.to_ascii_lowercase());
        }
        if let Some(resource) = registry.find_map_obj_resource(resource_name) {
            collision_names.extend(resource.collision_resources.iter().map(|collision| {
                let name = collision.resource_name.to_ascii_lowercase();
                if name.ends_with(".col") {
                    name
                } else {
                    format!("{name}.col")
                }
            }));
        }
    }
    if !model_names.is_empty() {
        add_matching(out, sources, &candidate.source_stage, |path| {
            path_matches_one_of(path, &model_names)
        });
    }
    if !collision_names.is_empty() {
        add_matching(out, sources, &candidate.source_stage, |path| {
            path_matches_one_of(path, &collision_names)
        });
    }
    let primary_models: Vec<_> = out
        .iter()
        .filter(|resource| normalized_path(&resource.raw_resource_path).ends_with(".bmd"))
        .cloned()
        .collect();
    for primary in primary_models {
        let normalized = normalized_path(&primary.raw_resource_path);
        let Some((directory, file_name)) = normalized.rsplit_once('/') else {
            continue;
        };
        let Some((stem, extension)) = file_name.rsplit_once('.') else {
            continue;
        };
        if extension != "bmd" {
            continue;
        }
        let archive = virtual_archive_path(&primary.source_asset_path);
        for source in sources {
            if source.source_stage != candidate.source_stage {
                continue;
            }
            for resource in &source.resources {
                if virtual_archive_path(&resource.source_asset_path) != archive {
                    continue;
                }
                let path = normalized_path(&resource.raw_resource_path);
                let Some((candidate_directory, candidate_file)) = path.rsplit_once('/') else {
                    continue;
                };
                let Some((candidate_stem, candidate_extension)) = candidate_file.rsplit_once('.')
                else {
                    continue;
                };
                if candidate_directory == directory
                    && candidate_stem == stem
                    && matches!(
                        candidate_extension,
                        "bck" | "btk" | "brk" | "btp" | "bpk" | "blk" | "bva" | "bas" | "bmt"
                    )
                {
                    out.insert(resource.clone());
                }
            }
        }
    }
}

fn preview_resource_path(
    candidate: &Candidate,
    resources: &[ObjectAuthoringResource],
    registry: &ObjectRegistry,
) -> Option<Vec<u8>> {
    let preferred = reference_field(&candidate.record, "resource_name")
        .and_then(|resource_name| {
            registry
                .find_map_obj_model_override(&candidate.factory_name, resource_name)
                .map(|definition| definition.model_path.as_str())
                .or_else(|| {
                    registry
                        .find_map_obj_resource(resource_name)
                        .and_then(|resource| resource.primary_model.as_deref())
                })
        })
        .or_else(|| {
            registry
                .primary_object_resource(&candidate.factory_name)
                .map(|binding| binding.model_name.as_str())
        })
        .or_else(|| {
            find_enemy_actor(registry, &candidate.factory_name)
                .and_then(|actor| actor.primary_model.as_deref())
        });
    preferred
        .and_then(|preferred| {
            resources.iter().find(|resource| {
                let path = normalized_path(&resource.raw_resource_path);
                let preferred = normalize_text_path(preferred);
                path == preferred || path.ends_with(&format!("/{preferred}"))
            })
        })
        .or_else(|| {
            resources
                .iter()
                .find(|resource| normalized_path(&resource.raw_resource_path).ends_with(".bmd"))
        })
        .map(|resource| resource.raw_resource_path.clone())
}

fn path_matches_one_of(path: &[u8], names: &BTreeSet<String>) -> bool {
    let path = normalized_path(path);
    names
        .iter()
        .any(|name| path == *name || path.ends_with(&format!("/{name}")))
}

fn virtual_archive_path(path: &Path) -> &str {
    path.to_str()
        .and_then(|path| path.split_once("!/").map(|(archive, _)| archive))
        .unwrap_or_default()
}

fn add_matching(
    out: &mut BTreeSet<ObjectAuthoringResource>,
    sources: &[&CatalogSource],
    stage: &str,
    matches: impl Fn(&[u8]) -> bool,
) {
    out.extend(
        sources
            .iter()
            .filter(|source| source.source_stage == stage)
            .flat_map(|source| source.resources.iter())
            .filter(|resource| matches(&resource.raw_resource_path))
            .cloned(),
    );
}

fn record_character_name(record: &JDramaRecord) -> Option<&str> {
    match &record.payload {
        JDramaRecordPayload::Actor { character_name, .. } => runtime_reference(character_name),
        _ => reference_field(record, "character_name"),
    }
}

fn runtime_reference(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "(null)").then_some(value)
}

fn reference_field<'a>(record: &'a JDramaRecord, name: &str) -> Option<&'a str> {
    string_field(record, name).and_then(runtime_reference)
}
fn string_field<'a>(record: &'a JDramaRecord, name: &str) -> Option<&'a str> {
    let fields = match &record.payload {
        JDramaRecordPayload::Actor { fields, .. } | JDramaRecordPayload::Fields { fields } => {
            fields
        }
        _ => return None,
    };
    fields.iter().find_map(|field| match &field.value {
        JDramaFieldValue::String(value) if field.name == name => Some(value.as_str()),
        _ => None,
    })
}

fn modal_signature(record: &JDramaRecord) -> Vec<u8> {
    let mut normalized = record.clone();
    normalized.name.clear();
    match &mut normalized.payload {
        JDramaRecordPayload::Actor { transform, .. } => {
            transform.translation = [0.0; 3];
            transform.rotation = [0.0; 3];
            transform.scale = [1.0; 3];
        }
        JDramaRecordPayload::Fields { fields } => {
            if let Some(names) = field_transform_names(&normalized.type_name) {
                set_vec3(fields, names.0, [0.0; 3]);
                set_vec3(fields, names.1, [0.0; 3]);
                set_vec3(fields, names.2, [1.0; 3]);
            }
        }
        _ => {}
    }
    serde_json::to_vec(&normalized).unwrap_or_else(|_| format!("{normalized:?}").into_bytes())
}

fn compare_candidate(a: &Candidate, b: &Candidate) -> Ordering {
    a.sort_key
        .cmp(&b.sort_key)
        .then_with(|| a.raw_resource_path.cmp(&b.raw_resource_path))
        .then_with(|| a.source_asset_path.cmp(&b.source_asset_path))
        .then_with(|| a.record_path.cmp(&b.record_path))
}

fn find_object<'a>(registry: &'a ObjectRegistry, name: &str) -> Option<&'a ObjectDefinition> {
    registry
        .find_object(name)
        .or_else(|| registry.find_object(semantic_type_name(name)))
}

fn find_enemy_actor<'a>(
    registry: &'a ObjectRegistry,
    name: &str,
) -> Option<&'a EnemyActorDefinition> {
    registry
        .find_enemy_actor(name)
        .or_else(|| registry.find_enemy_actor(semantic_type_name(name)))
}

fn find_enemy_manager<'a>(
    registry: &'a ObjectRegistry,
    name: &str,
) -> Option<&'a EnemyManagerDefinition> {
    registry
        .find_enemy_manager(name)
        .or_else(|| registry.find_enemy_manager(semantic_type_name(name)))
}

fn field_transform_names(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match semantic_type_name(name) {
        "AreaCylinder" => Some(("center", "authoring_vector", "cylinder_parameters")),
        "Generator" => Some(("position", "rotation", "authoring_vector")),
        _ => None,
    }
}

fn vec3_field(fields: &[JDramaField], name: &str) -> Option<[f32; 3]> {
    fields.iter().find_map(|field| match field.value {
        JDramaFieldValue::Vec3F32(value) if field.name == name => Some(value),
        _ => None,
    })
}

fn set_vec3(fields: &mut [JDramaField], name: &str, value: [f32; 3]) {
    if let Some(JDramaField {
        value: JDramaFieldValue::Vec3F32(current),
        ..
    }) = fields.iter_mut().find(|field| field.name == name)
    {
        *current = value;
    }
}

fn is_under_folder(raw: &[u8], folder: &str) -> bool {
    let path = normalized_path(raw);
    let folder = normalize_text_path(folder);
    let short = folder.strip_prefix("scene/").unwrap_or(&folder);
    let matches = [folder.as_str(), short]
        .into_iter()
        .filter(|candidate| !candidate.is_empty())
        .any(|candidate| path == candidate || path.starts_with(&format!("{candidate}/")));
    matches
}

fn normalize_runtime_reference(path: &str) -> String {
    let path = normalize_text_path(path);
    path.strip_prefix("scene/").unwrap_or(&path).to_string()
}

fn normalized_path(path: &[u8]) -> String {
    normalize_text_path(&String::from_utf8_lossy(path))
}

fn normalize_text_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase()
}

fn archive_resource_path(path: &Path) -> Option<Vec<u8>> {
    let text = path.to_string_lossy().replace('\\', "/");
    Some(
        text.split_once("!/")?
            .1
            .trim_start_matches('/')
            .as_bytes()
            .to_vec(),
    )
}

fn semantic_type_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sms_formats::{JDramaLightMap, JDramaTransform};
    use sms_schema::{
        ActorParticleBinding, EnemyActorDefinition, EnemyManagerDefinition,
        MapObjAnimationResourceDefinition, MapObjCollisionResourceDefinition,
        MapObjModelOverrideDefinition, MapObjResourceDefinition, ObjectResourceBinding,
        ObjectResourceRole, ParticleBindingTarget, ParticleResourceDefinition, SchemaSource,
    };

    fn field(name: &str, value: JDramaFieldValue) -> JDramaField {
        JDramaField {
            name: name.into(),
            value,
        }
    }

    fn actor(factory: &str, name: &str, parameter: u32) -> JDramaRecord {
        JDramaRecord {
            type_name: factory.into(),
            name: name.into(),
            payload: JDramaRecordPayload::Actor {
                transform: JDramaTransform {
                    translation: [parameter as f32, 2.0, 3.0],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: JDramaLightMap::default(),
                fields: vec![field("parameter", JDramaFieldValue::U32(parameter))],
            },
        }
    }

    fn fields(factory: &str, name: &str, fields: Vec<JDramaField>) -> JDramaRecord {
        JDramaRecord {
            type_name: factory.into(),
            name: name.into(),
            payload: JDramaRecordPayload::Fields { fields },
        }
    }

    fn group(factory: &str, children: Vec<JDramaRecord>) -> JDramaRecord {
        JDramaRecord {
            type_name: factory.into(),
            name: factory.into(),
            payload: JDramaRecordPayload::Group {
                fields: Vec::new(),
                children,
            },
        }
    }

    fn document(groups: Vec<(u32, Vec<JDramaRecord>)>) -> JDramaDocument {
        JDramaDocument {
            root: group(
                "Strategy",
                groups
                    .into_iter()
                    .map(|(index, children)| JDramaRecord {
                        type_name: "IdxGroup".into(),
                        name: format!("group {index}"),
                        payload: JDramaRecordPayload::Group {
                            fields: vec![field("group_index", JDramaFieldValue::U32(index))],
                            children,
                        },
                    })
                    .collect(),
            ),
        }
    }

    fn character_registration(
        type_name: &str,
        name: &str,
        field_name: &str,
        value: &str,
    ) -> JDramaRecord {
        fields(
            type_name,
            name,
            vec![field(
                field_name,
                JDramaFieldValue::String(value.to_string()),
            )],
        )
    }

    fn force_tolerant_common_parse(mut document: JDramaDocument) -> Vec<u8> {
        let JDramaRecordPayload::Group { children, .. } = &mut document.root.payload else {
            unreachable!()
        };
        children.insert(
            0,
            character_registration(
                "ObjChara",
                "unsupported",
                "resource_folder",
                "/scene/unused",
            ),
        );
        let mut bytes = document.to_bytes().unwrap();
        let offset = bytes
            .windows(b"ObjChara".len())
            .position(|window| window == b"ObjChara")
            .unwrap();
        bytes[offset..offset + b"BadChara".len()].copy_from_slice(b"BadChara");
        let key = sms_formats::jdrama_key_code("BadChara").unwrap();
        bytes[offset - 4..offset - 2].copy_from_slice(&key.to_be_bytes());
        assert!(JDramaDocument::parse(&bytes).is_err());
        bytes
    }

    fn object(factory: &str, class_name: &str) -> ObjectDefinition {
        ObjectDefinition {
            factory_name: factory.into(),
            class_name: class_name.into(),
            category: "fixture".into(),
            source: SchemaSource::MarNameRefGen,
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        }
    }

    fn source(stage: &str, document: JDramaDocument) -> CatalogSource {
        CatalogSource {
            source_stage: stage.into(),
            sort_key: stage.into(),
            documents: vec![SourceDocument {
                raw_resource_path: b"map/scene.bin".to_vec(),
                source_asset_path: PathBuf::from(format!("{stage}.szs!/map/scene.bin")),
                document,
            }],
            resources: Vec::new(),
        }
    }

    fn enemy_registry() -> ObjectRegistry {
        ObjectRegistry {
            objects: vec![
                object("FixtureEnemy", "TFixtureEnemy"),
                object("FixtureManager", "TFixtureManager"),
            ],
            enemy_actors: vec![EnemyActorDefinition {
                factory_name: "FixtureEnemy".into(),
                class_name: "TFixtureEnemy".into(),
                model_index: None,
                fallback_models: Vec::new(),
                primary_model: None,
                named_models: Vec::new(),
                indexed_models: Vec::new(),
                manager_factories: vec!["FixtureManager".into()],
                runtime_uniform_scale: None,
            }],
            enemy_managers: vec![EnemyManagerDefinition {
                factory_name: "FixtureManager".into(),
                class_name: "TFixtureManager".into(),
                model_index: None,
                spawned_actor_class: None,
                parameter_path: Some("/enemy/fixture.prm".into()),
                models: Vec::new(),
            }],
            ..ObjectRegistry::default()
        }
    }

    fn quick_camera_record() -> JDramaRecord {
        fields(
            "CameraMapInfo",
            SHINE_QUICK_CAMERA_NAME,
            vec![
                field(
                    "position",
                    JDramaFieldValue::Vec3F32([-3700.0, 900.0, -11000.0]),
                ),
                field("pitch_yaw", JDramaFieldValue::Vec2F32([0.0, 122.0])),
                field("flags", JDramaFieldValue::I32(0)),
                field("camera_mode", JDramaFieldValue::I32(33)),
                field("camera_parameter", JDramaFieldValue::I32(-1)),
                field("demo_length_frames", JDramaFieldValue::I32(900)),
            ],
        )
    }

    fn source_with_shine_and_quick_camera() -> CatalogSource {
        let mut source = source(
            "pinnaParco7",
            document(vec![(5, vec![actor("Shine", "shine", 1)])]),
        );
        source.documents.push(SourceDocument {
            raw_resource_path: b"map/tables.bin".to_vec(),
            source_asset_path: PathBuf::from("pinnaParco7.szs!/map/tables.bin"),
            document: JDramaDocument {
                root: group(
                    "NameRefGrp",
                    vec![JDramaRecord {
                        type_name: "CameraMapToolTable".to_string(),
                        name: "camera map tool table".to_string(),
                        payload: JDramaRecordPayload::Group {
                            fields: Vec::new(),
                            children: vec![quick_camera_record()],
                        },
                    }],
                ),
            },
        });
        source
    }

    #[test]
    fn shine_catalog_owns_the_retail_quick_camera_lookup_record() {
        let registry = ObjectRegistry {
            objects: vec![object("Shine", "TShine")],
            ..ObjectRegistry::default()
        };
        let source = source_with_shine_and_quick_camera();
        let catalog = build_from_sources(&[source], &registry);
        let template = catalog.find("Shine").unwrap();

        assert_eq!(template.table_dependencies.len(), 1);
        let dependency = &template.table_dependencies[0];
        assert_eq!(dependency.record, quick_camera_record());
        assert!(matches!(
            &dependency.target,
            AuthoredPlacementDependencyTarget::NamedGroup { type_name, name }
                if type_name == "CameraMapToolTable" && name == "camera map tool table"
        ));
    }

    #[test]
    fn shine_catalog_is_omitted_when_the_fixed_quick_camera_lookup_is_missing() {
        let registry = ObjectRegistry {
            objects: vec![object("Shine", "TShine")],
            ..ObjectRegistry::default()
        };
        let source = source(
            "stage",
            document(vec![(5, vec![actor("Shine", "shine", 1)])]),
        );
        let mut warnings = Vec::new();
        let catalog = build_from_sources_with_common(&[source], &registry, None, &mut warnings);

        assert!(catalog.find("Shine").is_none());
        assert!(warnings.iter().any(|warning| {
            warning.message.contains("CameraMapInfo")
                && warning.message.contains(SHINE_QUICK_CAMERA_NAME)
        }));
    }

    #[test]
    fn common_character_parser_preserves_a_strict_typed_document() {
        let document = JDramaDocument {
            root: group(
                "NameRefGrp",
                vec![character_registration(
                    "ObjChara",
                    "FixtureChara",
                    "resource_folder",
                    "/scene/fixture",
                )],
            ),
        };
        let (parsed, warning) = parse_common_character_document(&document.to_bytes().unwrap())
            .expect("strict common character table");
        assert_eq!(parsed, document);
        assert!(warning.is_none());
    }

    #[test]
    fn common_character_parser_recovers_exact_obj_and_simple_registrations() {
        let bytes = force_tolerant_common_parse(JDramaDocument {
            root: group(
                "NameRefGrp",
                vec![
                    character_registration(
                        "JDrama::ObjChara",
                        "FixtureObject",
                        "resource_folder",
                        "/scene/fixture",
                    ),
                    character_registration(
                        "SmplChara",
                        "FixtureSimple",
                        "archive_path",
                        "/scene/map/map.arc",
                    ),
                ],
            ),
        });
        let (parsed, warning) =
            parse_common_character_document(&bytes).expect("tolerant common character table");
        let records = records_with_paths(&parsed)
            .into_iter()
            .map(|(_, record)| record)
            .filter(|record| is_character_registration(record))
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].type_name, "JDrama::ObjChara");
        assert_eq!(records[0].name, "FixtureObject");
        assert_eq!(
            string_field(records[0], "resource_folder"),
            Some("/scene/fixture")
        );
        assert_eq!(records[1].type_name, "SmplChara");
        assert_eq!(records[1].name, "FixtureSimple");
        assert_eq!(
            string_field(records[1], "archive_path"),
            Some("/scene/map/map.arc")
        );
        assert!(warning.is_some_and(|message| message.contains("recovered 2 exact")));
    }

    #[test]
    fn tolerant_common_character_conflicts_still_omit_the_candidate() {
        let bytes = force_tolerant_common_parse(JDramaDocument {
            root: group(
                "NameRefGrp",
                vec![
                    character_registration(
                        "ObjChara",
                        "FixtureChara",
                        "resource_folder",
                        "/scene/one",
                    ),
                    character_registration(
                        "ObjChara",
                        "FixtureChara",
                        "resource_folder",
                        "/scene/two",
                    ),
                ],
            ),
        });
        let (common, _) = parse_common_character_document(&bytes).unwrap();
        let mut fixture_actor = actor("FixtureActor", "actor", 1);
        let JDramaRecordPayload::Actor { character_name, .. } = &mut fixture_actor.payload else {
            unreachable!()
        };
        *character_name = "FixtureChara".to_string();
        let sources = vec![source("stage", document(vec![(4, vec![fixture_actor])]))];
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };
        let mut warnings = Vec::new();
        let catalog =
            build_from_sources_with_common(&sources, &registry, Some(&common), &mut warnings);
        assert!(catalog.find("FixtureActor").is_none());
        assert!(warnings.iter().any(|warning| warning
            .message
            .contains("conflicting registrations in global scenecmn.bin")));
    }

    #[test]
    fn global_character_registration_keeps_stage_local_resource_folder() {
        let common = JDramaDocument {
            root: group(
                "NameRefGrp",
                vec![character_registration(
                    "ObjChara",
                    "FixtureChara",
                    "resource_folder",
                    "/scene/fixture",
                )],
            ),
        };
        let mut fixture_actor = actor("FixtureActor", "actor", 1);
        let JDramaRecordPayload::Actor { character_name, .. } = &mut fixture_actor.payload else {
            unreachable!()
        };
        *character_name = "FixtureChara".to_string();
        let mut fixture = source("stage", document(vec![(4, vec![fixture_actor])]));
        fixture.resources = resources(
            "stage.szs",
            &[
                "fixture/default.bmd",
                "fixture/walk.bck",
                "fixture/bas/walk.bas",
                "unrelated/unused.bmd",
            ],
        );
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };
        let mut warnings = Vec::new();

        let catalog =
            build_from_sources_with_common(&[fixture], &registry, Some(&common), &mut warnings);
        let template = catalog.find("FixtureActor").unwrap();

        assert!(warnings.is_empty());
        assert!(template.character_records.is_empty());
        assert_eq!(
            raw_paths(&template.resources),
            [
                "fixture/bas/walk.bas",
                "fixture/default.bmd",
                "fixture/walk.bck",
            ]
        );
    }
    #[test]
    fn selects_nearest_group_from_map_scene_only() {
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };
        let mut fixture = source(
            "stage",
            JDramaDocument {
                root: group(
                    "MarScene",
                    vec![
                        actor("FixtureActor", "outside", 9),
                        group(
                            "Strategy",
                            vec![JDramaRecord {
                                type_name: "IdxGroup".into(),
                                name: "nested".into(),
                                payload: JDramaRecordPayload::Group {
                                    fields: vec![field("group_index", JDramaFieldValue::U32(7))],
                                    children: vec![actor("FixtureActor", "retail prototype", 1)],
                                },
                            }],
                        ),
                    ],
                ),
            },
        );
        fixture.documents.push(SourceDocument {
            raw_resource_path: b"map/sceneCmn.bin".to_vec(),
            source_asset_path: "stage.szs!/map/sceneCmn.bin".into(),
            document: document(vec![(3, vec![actor("FixtureActor", "wrong target", 2)])]),
        });
        let catalog = build_from_sources(&[fixture], &registry);
        let template = catalog.find("FixtureActor").unwrap();
        assert_eq!(template.group_index, 7);
        assert_eq!(template.record.name, "retail prototype");
    }

    #[test]
    fn resolves_exact_enemy_manager_name() {
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        actor_fields.push(field(
            "manager_name",
            JDramaFieldValue::String("wanted manager".into()),
        ));
        let fixture = document(vec![
            (
                2,
                vec![
                    fields("FixtureManager", "other manager", Vec::new()),
                    fields("FixtureManager", "wanted manager", Vec::new()),
                ],
            ),
            (4, vec![enemy]),
        ]);
        let mut source = source("stage", fixture);
        source.resources = resources("stage.szs", &["map/params/enemy/fixture.prm"]);
        let catalog = build_from_sources(&[source], &enemy_registry());
        let dependencies = &catalog.find("FixtureEnemy").unwrap().dependencies;
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].group_index, 2);
        assert_eq!(dependencies[0].record.name, "wanted manager");
    }

    #[test]
    fn resolves_enemy_manager_from_retail_conductor_container() {
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        actor_fields.push(field(
            "manager_name",
            JDramaFieldValue::String("wanted manager".into()),
        ));
        let conductor = JDramaRecord {
            type_name: "GroupObj".into(),
            name: "conductor initialization".into(),
            payload: JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![fields("FixtureManager", "wanted manager", Vec::new())],
            },
        };
        let strategy = document(vec![(7, vec![enemy])]).root;
        let fixture = JDramaDocument {
            root: JDramaRecord {
                type_name: "GroupObj".into(),
                name: "whole scene".into(),
                payload: JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: vec![conductor, group("MarScene", vec![strategy])],
                },
            },
        };
        let mut source = source("stage", fixture);
        source.resources = resources("stage.szs", &["map/params/enemy/fixture.prm"]);

        let catalog = build_from_sources(&[source], &enemy_registry());
        let dependencies = &catalog.find("FixtureEnemy").unwrap().dependencies;
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].group_index, 0);
        assert_eq!(
            dependencies[0].target,
            AuthoredPlacementDependencyTarget::NamedGroup {
                type_name: "GroupObj".into(),
                name: "conductor initialization".into(),
            }
        );
        assert_eq!(dependencies[0].record.name, "wanted manager");
    }

    #[test]
    fn manager_parameter_override_is_optional_but_copied_when_present() {
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        actor_fields.push(field(
            "manager_name",
            JDramaFieldValue::String("wanted manager".into()),
        ));
        let document = document(vec![
            (
                2,
                vec![fields("FixtureManager", "wanted manager", Vec::new())],
            ),
            (4, vec![enemy]),
        ]);

        let without_override = source("stage", document.clone());
        let template = build_from_sources(&[without_override], &enemy_registry())
            .find("FixtureEnemy")
            .cloned()
            .expect("compiled manager defaults keep the actor authorable");
        assert!(template.resources.is_empty());

        let mut with_override = source("stage", document);
        with_override.resources = resources("stage.szs", &["map/params/enemy/fixture.prm"]);
        let template = build_from_sources(&[with_override], &enemy_registry())
            .find("FixtureEnemy")
            .cloned()
            .expect("exact stage parameter override keeps the actor authorable");
        assert_eq!(
            raw_paths(&template.resources),
            vec!["map/params/enemy/fixture.prm"]
        );
    }

    #[test]
    fn copies_manager_animation_and_runtime_map_object_dependencies() {
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        actor_fields.push(field(
            "manager_name",
            JDramaFieldValue::String("wanted manager".into()),
        ));
        let document = document(vec![
            (
                2,
                vec![fields("FixtureManager", "wanted manager", Vec::new())],
            ),
            (4, vec![enemy]),
        ]);
        let mut source = source("stage", document);
        source.resources = resources(
            "stage.szs",
            &[
                "fixtureanm/walk.bck",
                "fixtureanm/bas/walk.bas",
                "map/params/enemy/fixture.prm",
                "mapobj/support.bmd",
            ],
        );
        let mut registry = enemy_registry();
        registry.enemy_manager_animation_folders.push(
            sms_schema::EnemyManagerAnimationFolderDefinition {
                factory_name: "FixtureManager".into(),
                folder: "/scene/fixtureanm".into(),
                source_file: "src/Enemy/fixture.cpp".into(),
            },
        );
        registry
            .runtime_map_obj_dependencies
            .push(sms_schema::RuntimeMapObjDependencyDefinition {
                factory_name: "FixtureManager".into(),
                resource_name: "support".into(),
                source_file: "src/Enemy/fixture.cpp".into(),
            });
        registry.map_obj_resources.push(MapObjResourceDefinition {
            resource_name: "support".into(),
            actor_type: 0,
            object_flags: 0,
            required_manager_name: "map object manager".into(),
            has_hold_dependency: false,
            has_move_dependency: false,
            uses_resource_name_model_fallback: false,
            primary_model: Some("support.bmd".into()),
            animation_resources: Vec::new(),
            hold_model_path: None,
            move_bck_path: None,
            load_flags: 0,
            collision_resources: Vec::new(),
            source_file: "src/MoveBG/MapObjInit.cpp".into(),
        });

        let template = build_from_sources(&[source], &registry)
            .find("FixtureEnemy")
            .cloned()
            .expect("manager animation folder keeps the actor authorable");
        assert_eq!(
            raw_paths(&template.resources),
            vec![
                "fixtureanm/bas/walk.bas",
                "fixtureanm/walk.bck",
                "map/params/enemy/fixture.prm",
                "mapobj/support.bmd",
            ]
        );
    }

    #[test]
    fn omits_manager_dependent_candidates_without_one_exact_manager() {
        let enemy = |managers: Vec<JDramaRecord>| {
            let mut actor = actor("FixtureEnemy", "enemy", 1);
            let JDramaRecordPayload::Actor { fields, .. } = &mut actor.payload else {
                unreachable!()
            };
            fields.push(field(
                "manager_name",
                JDramaFieldValue::String("wanted manager".into()),
            ));
            document(vec![(2, managers), (4, vec![actor])])
        };

        let missing_sources = vec![source("stage", enemy(Vec::new()))];
        let mut warnings = Vec::new();
        let missing = build_from_sources_with_common(
            &missing_sources,
            &enemy_registry(),
            None,
            &mut warnings,
        );
        assert!(missing.find("FixtureEnemy").is_none());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("no exact compatible record")));

        let duplicate_sources = vec![source(
            "stage",
            enemy(vec![
                fields("FixtureManager", "wanted manager", Vec::new()),
                fields("FixtureManager", "wanted manager", Vec::new()),
            ]),
        )];
        warnings.clear();
        let duplicate = build_from_sources_with_common(
            &duplicate_sources,
            &enemy_registry(),
            None,
            &mut warnings,
        );
        assert!(duplicate.find("FixtureEnemy").is_none());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("2 exact compatible records")));
    }

    #[test]
    fn modal_selection_is_order_independent_and_keeps_retail_transform() {
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };
        let sources = vec![
            source(
                "stage-b",
                document(vec![(4, vec![actor("FixtureActor", "b", 7)])]),
            ),
            source(
                "stage-c",
                document(vec![(4, vec![actor("FixtureActor", "c", 2)])]),
            ),
            source(
                "stage-a",
                document(vec![(4, vec![actor("FixtureActor", "a", 7)])]),
            ),
        ];
        let mut reversed = sources.clone();
        reversed.reverse();
        let forward = build_from_sources(&sources, &registry);
        assert_eq!(forward, build_from_sources(&reversed, &registry));
        let template = forward.find("FixtureActor").unwrap();
        assert_eq!(template.source_stage, "stage-a");
        assert_eq!(template.record.name, "a");
        let JDramaRecordPayload::Actor { transform, .. } = &template.record.payload else {
            unreachable!()
        };
        assert_eq!(transform.translation, [7.0, 2.0, 3.0]);
    }

    #[test]
    fn maps_scene_cmn_chara_parameters_and_graph_resources() {
        let manager = fields(
            "FixtureManager",
            "fixture manager",
            vec![field(
                "character_name",
                JDramaFieldValue::String("FixtureChara".into()),
            )],
        );
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        actor_fields.extend([
            field(
                "manager_name",
                JDramaFieldValue::String("fixture manager".into()),
            ),
            field(
                "graph_name",
                JDramaFieldValue::String("fixture graph".into()),
            ),
        ]);
        let mut fixture = source(
            "stage",
            document(vec![(2, vec![manager]), (4, vec![enemy])]),
        );
        fixture.documents.push(SourceDocument {
            raw_resource_path: b"map/tables.bin".to_vec(),
            source_asset_path: "stage.szs!/map/tables.bin".into(),
            document: document(vec![(
                0,
                vec![fields(
                    "ObjChara",
                    "FixtureChara",
                    vec![field(
                        "resource_folder",
                        JDramaFieldValue::String("/scene/fixture".into()),
                    )],
                )],
            )]),
        });
        fixture.resources = resources(
            "stage.szs",
            &[
                "fixture/body.bmd",
                "fixture/body.bck",
                "fixtureish/wrong.bmd",
                "map/params/enemy/fixture.prm",
                "map/params/enemy/fixture-extra.prm",
                "map/scene.ral",
            ],
        );
        let catalog = build_from_sources(&[fixture], &enemy_registry());
        let template = catalog.find("FixtureEnemy").unwrap();
        assert_eq!(template.character_records.len(), 1);
        assert_eq!(template.character_records[0].name, "FixtureChara");
        assert_eq!(template.required_graph_names, vec!["fixture graph"]);
        assert_eq!(
            raw_paths(&template.resources),
            vec![
                "fixture/body.bck",
                "fixture/body.bmd",
                "map/params/enemy/fixture.prm",
                "map/scene.ral",
            ]
        );
    }

    #[test]
    fn retail_null_references_are_not_promoted_to_owned_dependencies() {
        let mut fixture_actor = actor("FixtureActor", "fixture", 1);
        let JDramaRecordPayload::Actor {
            character_name,
            fields,
            ..
        } = &mut fixture_actor.payload
        else {
            unreachable!()
        };
        *character_name = "(null)".into();
        fields.extend([
            field("manager_name", JDramaFieldValue::String("(null)".into())),
            field("graph_name", JDramaFieldValue::String("(null)".into())),
            field("resource_name", JDramaFieldValue::String("(null)".into())),
        ]);
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };

        let catalog = build_from_sources(
            &[source("stage", document(vec![(4, vec![fixture_actor])]))],
            &registry,
        );
        let template = catalog.find("FixtureActor").unwrap();

        assert!(template.dependencies.is_empty());
        assert!(template.character_records.is_empty());
        assert!(template.required_graph_names.is_empty());
        assert!(template.resources.is_empty());
        let JDramaRecordPayload::Actor { character_name, .. } = &template.record.payload else {
            unreachable!()
        };
        assert_eq!(character_name, "(null)");
        assert_eq!(
            string_field(&template.record, "manager_name"),
            Some("(null)")
        );
        assert_eq!(string_field(&template.record, "graph_name"), Some("(null)"));
        assert_eq!(
            string_field(&template.record, "resource_name"),
            Some("(null)")
        );
    }
    #[test]
    fn manager_backed_actor_uses_manager_character_not_actor_character() {
        let manager = fields(
            "FixtureManager",
            "fixture manager",
            vec![field(
                "character_name",
                JDramaFieldValue::String("ManagerChara".into()),
            )],
        );
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            character_name,
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        *character_name = "ActorCharaNotRegistered".into();
        actor_fields.push(field(
            "manager_name",
            JDramaFieldValue::String("fixture manager".into()),
        ));

        let mut fixture = source(
            "stage",
            document(vec![(2, vec![manager]), (4, vec![enemy])]),
        );
        fixture.documents.push(SourceDocument {
            raw_resource_path: b"map/tables.bin".to_vec(),
            source_asset_path: "stage.szs!/map/tables.bin".into(),
            document: document(vec![(
                0,
                vec![character_registration(
                    "ObjChara",
                    "ManagerChara",
                    "resource_folder",
                    "/scene/manager",
                )],
            )]),
        });
        fixture.resources = resources(
            "stage.szs",
            &["manager/body.bmd", "map/params/enemy/fixture.prm"],
        );

        let mut warnings = Vec::new();
        let catalog =
            build_from_sources_with_common(&[fixture], &enemy_registry(), None, &mut warnings);
        let template = catalog
            .find("FixtureEnemy")
            .expect("resolved manager makes the actor authorable");
        assert_eq!(template.character_records.len(), 1);
        assert_eq!(template.character_records[0].name, "ManagerChara");
        assert!(!warnings
            .iter()
            .any(|warning| warning.message.contains("ActorCharaNotRegistered")));
    }

    #[test]
    fn manager_character_registration_remains_required() {
        let manager = fields(
            "FixtureManager",
            "fixture manager",
            vec![field(
                "character_name",
                JDramaFieldValue::String("ManagerCharaNotRegistered".into()),
            )],
        );
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor {
            character_name,
            fields: actor_fields,
            ..
        } = &mut enemy.payload
        else {
            unreachable!()
        };
        *character_name = "ActorChara".into();
        actor_fields.push(field(
            "manager_name",
            JDramaFieldValue::String("fixture manager".into()),
        ));

        let mut fixture = source(
            "stage",
            document(vec![(2, vec![manager]), (4, vec![enemy])]),
        );
        fixture.documents.push(SourceDocument {
            raw_resource_path: b"map/tables.bin".to_vec(),
            source_asset_path: "stage.szs!/map/tables.bin".into(),
            document: document(vec![(
                0,
                vec![character_registration(
                    "ObjChara",
                    "ActorChara",
                    "resource_folder",
                    "/scene/actor",
                )],
            )]),
        });
        fixture.resources = resources(
            "stage.szs",
            &["actor/body.bmd", "map/params/enemy/fixture.prm"],
        );

        let mut warnings = Vec::new();
        let catalog =
            build_from_sources_with_common(&[fixture], &enemy_registry(), None, &mut warnings);
        assert!(catalog.find("FixtureEnemy").is_none());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("ManagerCharaNotRegistered")));
    }

    #[test]
    fn standalone_actor_character_registration_remains_required() {
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };
        let mut fixture_actor = actor("FixtureActor", "actor", 1);
        let JDramaRecordPayload::Actor { character_name, .. } = &mut fixture_actor.payload else {
            unreachable!()
        };
        *character_name = "StandaloneCharaNotRegistered".into();
        let sources = vec![source("stage", document(vec![(4, vec![fixture_actor])]))];
        let mut warnings = Vec::new();
        let catalog = build_from_sources_with_common(&sources, &registry, None, &mut warnings);
        assert!(catalog.find("FixtureActor").is_none());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("StandaloneCharaNotRegistered")));
    }

    #[test]
    fn map_object_closure_uses_required_manager_override_collision_and_companions() {
        let registry = ObjectRegistry {
            objects: vec![
                object("FixtureMapObj", "TFixtureMapObj"),
                object("MapObjManager", "TMapObjManager"),
            ],
            map_obj_resources: vec![MapObjResourceDefinition {
                resource_name: "FixtureSlot".into(),
                actor_type: 0,
                object_flags: 0,
                required_manager_name: "fixture map manager".into(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: Some("base.bmd".into()),
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0,
                collision_resources: vec![MapObjCollisionResourceDefinition {
                    resource_name: "HitBox".into(),
                    flags: 0,
                    collision_kind: 0,
                    max_vertices: None,
                }],
                source_file: "fixture.cpp".into(),
            }],
            map_obj_model_overrides: vec![MapObjModelOverrideDefinition {
                resource_name: "FixtureSlot".into(),
                class_name: "TFixtureMapObj".into(),
                model_path: "override.bmd".into(),
                load_flags: 0,
                tev_color: None,
                binding_source_file: "fixture.cpp".into(),
                model_source_file: "fixture.cpp".into(),
            }],
            ..ObjectRegistry::default()
        };
        let manager = fields("MapObjManager", "fixture map manager", Vec::new());
        let mut map_object = actor("FixtureMapObj", "map object", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut map_object.payload
        else {
            unreachable!()
        };
        actor_fields.push(field(
            "resource_name",
            JDramaFieldValue::String("FixtureSlot".into()),
        ));
        let mut fixture = source(
            "stage",
            document(vec![(2, vec![manager]), (4, vec![map_object])]),
        );
        fixture.resources = resources(
            "stage.szs",
            &[
                "mapobj/override.bmd",
                "mapobj/override.bck",
                "mapobj/override.btk",
                "mapobj/base.bmd",
                "mapobj/HitBox.col",
                "mapobj/Other.col",
            ],
        );
        fixture
            .resources
            .extend(resources("other.szs", &["mapobj/override.brk"]));
        let catalog = build_from_sources(&[fixture], &registry);
        let template = catalog.find("FixtureMapObj").unwrap();
        assert_eq!(template.dependencies.len(), 1);
        assert_eq!(template.dependencies[0].record.name, "fixture map manager");
        assert_eq!(
            raw_paths(&template.resources),
            vec![
                "mapobj/HitBox.col",
                "mapobj/override.bck",
                "mapobj/override.bmd",
                "mapobj/override.btk",
            ]
        );
        assert_eq!(
            template.preview_resource_path.as_deref(),
            Some(b"mapobj/override.bmd".as_slice())
        );
    }

    #[test]
    fn closes_actor_and_owned_manager_models_particles_and_dependency_graphs() {
        let registry = ObjectRegistry {
            objects: vec![
                object("FixtureEnemy", "TFixtureEnemy"),
                object("FixtureManager", "TFixtureManager"),
            ],
            object_resources: vec![
                ObjectResourceBinding {
                    factory_name: "FixtureEnemy".into(),
                    model_index: 0,
                    role: ObjectResourceRole::Primary,
                    model_name: "/scene/actors/actor.bmd".into(),
                    resource_base: None,
                    load_flags: 0,
                    source_file: "fixture.cpp".into(),
                },
                ObjectResourceBinding {
                    factory_name: "FixtureEnemy".into(),
                    model_index: 1,
                    role: ObjectResourceRole::Secondary,
                    model_name: "actor_extra.bmd".into(),
                    resource_base: Some("/scene/actors".into()),
                    load_flags: 0,
                    source_file: "fixture.cpp".into(),
                },
                ObjectResourceBinding {
                    factory_name: "FixtureManager".into(),
                    model_index: 0,
                    role: ObjectResourceRole::Primary,
                    model_name: "manager.bmd".into(),
                    resource_base: Some("/scene/managers".into()),
                    load_flags: 0,
                    source_file: "fixture.cpp".into(),
                },
            ],
            enemy_actors: vec![EnemyActorDefinition {
                factory_name: "FixtureEnemy".into(),
                class_name: "TFixtureEnemy".into(),
                model_index: None,
                fallback_models: Vec::new(),
                primary_model: None,
                named_models: Vec::new(),
                indexed_models: Vec::new(),
                manager_factories: vec!["FixtureManager".into()],
                runtime_uniform_scale: None,
            }],
            enemy_managers: vec![EnemyManagerDefinition {
                factory_name: "FixtureManager".into(),
                class_name: "TFixtureManager".into(),
                model_index: None,
                spawned_actor_class: None,
                parameter_path: None,
                models: Vec::new(),
            }],
            actor_particle_bindings: vec![
                ActorParticleBinding {
                    class_name: "TFixtureEnemy".into(),
                    effect_id: 7,
                    target: ParticleBindingTarget::ActorOrigin,
                    source_file: "fixture.cpp".into(),
                },
                ActorParticleBinding {
                    class_name: "TFixtureManager".into(),
                    effect_id: 8,
                    target: ParticleBindingTarget::ActorOrigin,
                    source_file: "fixture.cpp".into(),
                },
            ],
            particle_resources: vec![
                ParticleResourceDefinition {
                    effect_id: 7,
                    path: "actor.jpa".into(),
                    source_file: "fixture.cpp".into(),
                },
                ParticleResourceDefinition {
                    effect_id: 8,
                    path: "/scene/effects/manager.jpa".into(),
                    source_file: "fixture.cpp".into(),
                },
            ],
            ..ObjectRegistry::default()
        };
        let manager = fields(
            "FixtureManager",
            "fixture manager",
            vec![field(
                "graph_name",
                JDramaFieldValue::String("manager graph".into()),
            )],
        );
        let mut enemy = actor("FixtureEnemy", "enemy", 1);
        let JDramaRecordPayload::Actor { fields, .. } = &mut enemy.payload else {
            unreachable!()
        };
        fields.push(field(
            "manager_name",
            JDramaFieldValue::String("fixture manager".into()),
        ));
        let mut local = source(
            "stage",
            document(vec![(2, vec![manager]), (4, vec![enemy])]),
        );
        local.resources = resources(
            "stage.szs",
            &[
                "actors/actor.bmd",
                "actors/actor.bck",
                "actors/actor.blk",
                "actors/actor_extra.bmd",
                "managers/manager.bmd",
                "managers/manager.btk",
                "effects/actor.jpa",
                "effects/manager.jpa",
                "map/scene.ral",
            ],
        );
        let foreign = CatalogSource {
            source_stage: "other".into(),
            sort_key: "other".into(),
            documents: Vec::new(),
            resources: resources("other.szs", &["different/actor.jpa"]),
        };
        let catalog = build_from_sources(&[local, foreign], &registry);
        let template = catalog.find("FixtureEnemy").unwrap();
        assert_eq!(template.required_graph_names, ["manager graph"]);
        assert_eq!(
            raw_paths(&template.resources),
            vec![
                "actors/actor.bck",
                "actors/actor.blk",
                "actors/actor.bmd",
                "actors/actor_extra.bmd",
                "effects/actor.jpa",
                "effects/manager.jpa",
                "managers/manager.bmd",
                "managers/manager.btk",
                "map/scene.ral",
            ]
        );
    }

    fn map_obj_animation_registry(extra_name: Option<&str>) -> ObjectRegistry {
        ObjectRegistry {
            objects: vec![object("FixtureMapObj", "TFixtureMapObj")],
            map_obj_resources: vec![MapObjResourceDefinition {
                resource_name: "FixtureSlot".into(),
                actor_type: 0,
                object_flags: 0,
                required_manager_name: String::new(),
                has_hold_dependency: true,
                has_move_dependency: true,
                uses_resource_name_model_fallback: false,
                primary_model: Some("/scene/mapObj/base.bmd".into()),
                animation_resources: vec![MapObjAnimationResourceDefinition {
                    model_name: Some("switch.bmd".into()),
                    animation_name: Some("spin".into()),
                    animation_channel: 0,
                    extra_name: extra_name.map(str::to_owned),
                    bas_path: Some("/scene/mapObj/spin.bas".into()),
                }],
                hold_model_path: Some("/scene/mapObj/hold.bmd".into()),
                move_bck_path: Some("/scene/mapObj/move.bck".into()),
                load_flags: 0,
                collision_resources: vec![MapObjCollisionResourceDefinition {
                    resource_name: "hit".into(),
                    flags: 0,
                    collision_kind: 0,
                    max_vertices: None,
                }],
                source_file: "fixture.cpp".into(),
            }],
            ..ObjectRegistry::default()
        }
    }

    fn map_obj_animation_source() -> CatalogSource {
        let mut map_object = actor("FixtureMapObj", "map object", 1);
        let JDramaRecordPayload::Actor { fields, .. } = &mut map_object.payload else {
            unreachable!()
        };
        fields.push(field(
            "resource_name",
            JDramaFieldValue::String("FixtureSlot".into()),
        ));
        let mut fixture = source("stage", document(vec![(4, vec![map_object])]));
        fixture.resources = resources(
            "stage.szs",
            &[
                "mapobj/base.bmd",
                "mapobj/base.btk",
                "mapobj/switch.bmd",
                "mapobj/spin.bck",
                "mapobj/spin.brk",
                "mapobj/spin.blk",
                "mapobj/spin.bas",
                "mapobj/hold.bmd",
                "mapobj/move.bck",
                "mapobj/hit.col",
            ],
        );
        fixture
    }

    #[test]
    fn closes_exact_map_obj_animation_hold_move_collision_and_scene_paths() {
        let catalog = build_from_sources(
            &[map_obj_animation_source()],
            &map_obj_animation_registry(None),
        );
        let template = catalog.find("FixtureMapObj").unwrap();
        assert_eq!(
            raw_paths(&template.resources),
            vec![
                "mapobj/base.bmd",
                "mapobj/base.btk",
                "mapobj/hit.col",
                "mapobj/hold.bmd",
                "mapobj/move.bck",
                "mapobj/spin.bas",
                "mapobj/spin.bck",
                "mapobj/spin.blk",
                "mapobj/spin.brk",
                "mapobj/switch.bmd",
            ]
        );
    }

    #[test]
    fn omits_unresolved_map_obj_animation_metadata() {
        let sources = vec![map_obj_animation_source()];
        let mut warnings = Vec::new();
        let catalog = build_from_sources_with_common(
            &sources,
            &map_obj_animation_registry(Some("unknown")),
            None,
            &mut warnings,
        );
        assert!(catalog.find("FixtureMapObj").is_none());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("unresolved animation extra_name")));
    }

    #[test]
    fn missing_stage_local_graph_does_not_fall_back_to_another_stage() {
        let registry = ObjectRegistry {
            objects: vec![object("FixtureActor", "TFixtureActor")],
            ..ObjectRegistry::default()
        };
        let mut actor = actor("FixtureActor", "actor", 1);
        let JDramaRecordPayload::Actor { fields, .. } = &mut actor.payload else {
            unreachable!()
        };
        fields.push(field(
            "graph_name",
            JDramaFieldValue::String("required graph".into()),
        ));
        let local = source("stage", document(vec![(4, vec![actor])]));
        let foreign = CatalogSource {
            source_stage: "other".into(),
            sort_key: "other".into(),
            documents: Vec::new(),
            resources: resources("other.szs", &["map/scene.ral"]),
        };
        let sources = vec![local, foreign];
        let mut warnings = Vec::new();
        let catalog = build_from_sources_with_common(&sources, &registry, None, &mut warnings);
        assert!(catalog.find("FixtureActor").is_none());
        assert!(warnings.iter().any(|warning| {
            warning
                .message
                .contains("required stage-local rail graph document")
        }));
    }

    #[test]
    fn common_launcher_closes_both_exact_manager_dependencies() {
        let registry = ObjectRegistry {
            objects: vec![
                object("CommonLauncher", "TCommonLauncher"),
                object("LauncherManager", "TLauncherManager"),
                object("SpawnManager", "TSpawnManager"),
            ],
            enemy_managers: vec![
                EnemyManagerDefinition {
                    factory_name: "LauncherManager".into(),
                    class_name: "TLauncherManager".into(),
                    model_index: None,
                    spawned_actor_class: None,
                    parameter_path: None,
                    models: Vec::new(),
                },
                EnemyManagerDefinition {
                    factory_name: "SpawnManager".into(),
                    class_name: "TSpawnManager".into(),
                    model_index: None,
                    spawned_actor_class: None,
                    parameter_path: None,
                    models: Vec::new(),
                },
            ],
            ..ObjectRegistry::default()
        };
        let mut launcher = actor("CommonLauncher", "launcher", 1);
        let JDramaRecordPayload::Actor {
            fields: actor_fields,
            ..
        } = &mut launcher.payload
        else {
            unreachable!()
        };
        actor_fields.extend([
            field(
                "manager_name",
                JDramaFieldValue::String("launcher manager".into()),
            ),
            field(
                "launched_enemy_name",
                JDramaFieldValue::String("spawn manager".into()),
            ),
        ]);
        let fixture = source(
            "stage",
            document(vec![
                (
                    2,
                    vec![
                        fields("LauncherManager", "launcher manager", Vec::new()),
                        fields("SpawnManager", "spawn manager", Vec::new()),
                    ],
                ),
                (4, vec![launcher]),
            ]),
        );
        let catalog = build_from_sources(&[fixture], &registry);
        let dependencies = &catalog.find("CommonLauncher").unwrap().dependencies;
        assert_eq!(
            dependencies
                .iter()
                .map(|dependency| dependency.record.name.as_str())
                .collect::<Vec<_>>(),
            ["launcher manager", "spawn manager"]
        );
    }

    #[test]
    fn rejects_schema_unsafe_and_known_unmodeled_live_actor_dependencies() {
        let mut unsafe_definition = object("UnsafeActor", "TUnsafeActor");
        unsafe_definition.unsafe_to_edit = true;
        let registry = ObjectRegistry {
            objects: vec![
                unsafe_definition,
                object("SwitchHelp", "TSwitchHelp"),
                object("LeanMirror", "TLeanMirror"),
            ],
            ..ObjectRegistry::default()
        };
        let sources = vec![source(
            "stage",
            document(vec![(
                4,
                vec![
                    actor("UnsafeActor", "unsafe", 1),
                    actor("SwitchHelp", "help", 1),
                    actor("LeanMirror", "mirror", 1),
                ],
            )]),
        )];
        let mut warnings = Vec::new();
        let catalog = build_from_sources_with_common(&sources, &registry, None, &mut warnings);
        assert!(catalog.is_empty());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("target_actor_name")));
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("ShiningStone")));
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stages and the neighboring SMS decomp"]
    fn retail_catalog_enables_representative_objects_and_manager_backed_enemies() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game's root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let archives = sms_formats::discover_scene_archives(&base_root)
            .expect("discover extracted retail stage archives");
        let registry = sms_schema::SchemaGenerator::new(decomp_root)
            .generate()
            .expect("generate the decomp-derived object registry");
        let build = ObjectAuthoringCatalog::build_with_base_root(&archives, &registry, &base_root);

        assert!(
            build.catalog.len() >= 200,
            "retail-backed catalog unexpectedly contains only {} templates; first warnings: {:?}",
            build.catalog.len(),
            build.warnings.iter().take(10).collect::<Vec<_>>()
        );
        for factory_name in [
            "Palm",
            "WoodBox",
            "HamuKuri",
            "PoiHana",
            "BossGesso",
            "BossEel",
            "BossHanachan",
        ] {
            assert!(
                build.catalog.find(factory_name).is_some(),
                "missing representative retail-backed template {factory_name}"
            );
        }
        assert!(build.catalog.iter().all(|(_, template)| template
            .required_graph_names
            .iter()
            .all(|name| runtime_reference(name).is_some())));
        let shine = build.catalog.find("Shine").expect("Shine retail template");
        assert!(shine.table_dependencies.iter().any(|dependency| {
            semantic_type_name(&dependency.record.type_name) == "CameraMapInfo"
                && dependency.record.name == SHINE_QUICK_CAMERA_NAME
        }));
        let boss_gesso = build
            .catalog
            .find("BossGesso")
            .expect("BossGesso retail template");
        assert!(
            boss_gesso.required_graph_names.is_empty(),
            "BossGesso promoted retail null graph sentinels into dependencies: {:?}",
            boss_gesso.required_graph_names
        );
        let poihana = build
            .catalog
            .find("PoiHana")
            .expect("PoiHana retail template");
        assert!(
            poihana.resources.iter().any(|resource| {
                let path = normalized_path(&resource.raw_resource_path);
                path.starts_with("poihana/") && path.ends_with(".bck")
            }),
            "PoiHana template omitted BCK files from its global character resource folder"
        );
        assert!(
            poihana.resources.iter().any(|resource| {
                let path = normalized_path(&resource.raw_resource_path);
                path.starts_with("poihana/bas/") && path.ends_with(".bas")
            }),
            "PoiHana template omitted BAS files from its global character resource folder"
        );
        let hamukuri = build
            .catalog
            .find("HamuKuri")
            .expect("HamuKuri retail template");
        assert!(
            hamukuri.resources.iter().any(|resource| {
                normalized_path(&resource.raw_resource_path).starts_with("hamukurianm/")
            }),
            "HamuKuri template omitted its manager's direct /scene/hamukurianm runtime folder"
        );
        assert!(
            hamukuri.resources.iter().any(|resource| {
                normalized_path(&resource.raw_resource_path) == "mapobj/mushroom1up.bmd"
            }),
            "HamuKuri template omitted the mushroom1up instantiated by its manager's loadAfter"
        );
    }

    fn resources(archive: &str, paths: &[&str]) -> Vec<ObjectAuthoringResource> {
        paths
            .iter()
            .map(|path| ObjectAuthoringResource {
                raw_resource_path: path.as_bytes().to_vec(),
                source_asset_path: PathBuf::from(format!("{archive}!/{path}")),
            })
            .collect()
    }

    fn raw_paths(resources: &[ObjectAuthoringResource]) -> Vec<String> {
        resources
            .iter()
            .map(|resource| String::from_utf8_lossy(&resource.raw_resource_path).into_owned())
            .collect()
    }
}
