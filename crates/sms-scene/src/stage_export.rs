//! Applies editor-authored semantic changes to the strict stage archive and
//! writes a rebuilt archive outside the extracted base game.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sms_formats::{
    discover_scene_archives, ColFile, J3dRebuildDocument, JDramaDocument, JDramaField,
    JDramaFieldValue, JDramaRecord, JDramaRecordPayload, JDramaTransform,
};

use crate::{
    ObjectRegistry, PlacementAddress, PlacementBinding, Result, SceneError, SceneObject,
    SourceFreeStageArchive, StageDocument, StageObjectPlacement, StageOrigin,
    StageResourceDocument,
};

const WORLD_COLLISION_PATH: &[u8] = b"map/map.col";
const WORLD_SCENE_PATH: &[u8] = b"map/scene.bin";
const COLLISION_GRID_WIDTH_FIELD: &str = "collision_grid_width";
const COLLISION_GRID_HEIGHT_FIELD: &str = "collision_grid_height";
const COLLISION_TRIANGLE_CAPACITY_FIELD: &str = "collision_triangle_capacity";
const COLLISION_LIST_CAPACITY_FIELD: &str = "collision_list_capacity";
const COLLISION_WARP_CAPACITY_FIELD: &str = "collision_warp_capacity";
const COLLISION_GRID_CELL_SIZE: f32 = 1024.0;
const COLLISION_GRID_CELL_RECIPROCAL: f32 = 1.0 / COLLISION_GRID_CELL_SIZE;
const COLLISION_WALL_PADDING: f32 = 80.0;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageArchiveEdits {
    /// Source-free resources removed before insert/upsert edits are applied.
    /// Missing paths are ignored so one bundle can replace stages with
    /// different optional companion files.
    #[serde(default)]
    pub resource_removals: Vec<Vec<u8>>,
    #[serde(default)]
    pub resources: Vec<StageResourceEdit>,
    #[serde(default)]
    pub models: Vec<StageModelEdit>,
    #[serde(default)]
    pub collisions: Vec<StageCollisionEdit>,
    #[serde(default)]
    pub placement_inserts: Vec<StagePlacementInsert>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageResourceEdit {
    pub raw_resource_path: Vec<u8>,
    pub document: StageResourceDocument,
    #[serde(default)]
    pub mode: StageResourceEditMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageResourceEditMode {
    #[default]
    Insert,
    Upsert,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageModelEdit {
    pub raw_resource_path: Vec<u8>,
    pub document: J3dRebuildDocument,
    #[serde(default)]
    pub mode: StageModelEditMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageModelEditMode {
    #[default]
    Replace,
    Upsert,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageCollisionEdit {
    pub raw_resource_path: Vec<u8>,
    pub document: ColFile,
    #[serde(default)]
    pub mode: StageCollisionEditMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageCollisionEditMode {
    #[default]
    Replace,
    Upsert,
    Append,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StagePlacementInsert {
    pub raw_resource_path: Vec<u8>,
    pub parent_record_path: Vec<usize>,
    pub record: JDramaRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageArchiveExportOutcome {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub size_bytes: usize,
    pub changed: bool,
}

impl StageArchiveEdits {
    pub fn remove_resource(&mut self, raw_resource_path: impl Into<Vec<u8>>) {
        let raw_resource_path = raw_resource_path.into();
        self.resources
            .retain(|edit| edit.raw_resource_path != raw_resource_path);
        self.models
            .retain(|edit| edit.raw_resource_path != raw_resource_path);
        self.collisions
            .retain(|edit| edit.raw_resource_path != raw_resource_path);
        if !self
            .resource_removals
            .iter()
            .any(|path| path == &raw_resource_path)
        {
            self.resource_removals.push(raw_resource_path);
        }
    }

    pub fn insert_resource(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        document: StageResourceDocument,
    ) {
        self.set_resource_edit(
            raw_resource_path.into(),
            document,
            StageResourceEditMode::Insert,
        );
    }

    pub fn upsert_resource(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        document: StageResourceDocument,
    ) {
        self.set_resource_edit(
            raw_resource_path.into(),
            document,
            StageResourceEditMode::Upsert,
        );
    }

    fn set_resource_edit(
        &mut self,
        raw_resource_path: Vec<u8>,
        document: StageResourceDocument,
        mode: StageResourceEditMode,
    ) {
        self.resource_removals
            .retain(|path| path != &raw_resource_path);
        if let Some(edit) = self
            .resources
            .iter_mut()
            .find(|edit| edit.raw_resource_path == raw_resource_path)
        {
            edit.document = document;
            edit.mode = mode;
        } else {
            self.resources.push(StageResourceEdit {
                raw_resource_path,
                document,
                mode,
            });
        }
    }

    pub fn replace_model(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        document: J3dRebuildDocument,
    ) {
        self.set_model_edit(
            raw_resource_path.into(),
            document,
            StageModelEditMode::Replace,
        );
    }

    pub fn upsert_model(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        document: J3dRebuildDocument,
    ) {
        self.set_model_edit(
            raw_resource_path.into(),
            document,
            StageModelEditMode::Upsert,
        );
    }

    fn set_model_edit(
        &mut self,
        raw_resource_path: Vec<u8>,
        document: J3dRebuildDocument,
        mode: StageModelEditMode,
    ) {
        self.resource_removals
            .retain(|path| path != &raw_resource_path);
        if let Some(edit) = self
            .models
            .iter_mut()
            .find(|edit| edit.raw_resource_path == raw_resource_path)
        {
            edit.document = document;
            edit.mode = mode;
        } else {
            self.models.push(StageModelEdit {
                raw_resource_path,
                document,
                mode,
            });
        }
    }

    pub fn replace_collision(&mut self, raw_resource_path: impl Into<Vec<u8>>, document: ColFile) {
        self.set_collision_edit(
            raw_resource_path.into(),
            document,
            StageCollisionEditMode::Replace,
        );
    }

    pub fn upsert_collision(&mut self, raw_resource_path: impl Into<Vec<u8>>, document: ColFile) {
        self.set_collision_edit(
            raw_resource_path.into(),
            document,
            StageCollisionEditMode::Upsert,
        );
    }

    /// Appends a collision document after the current document at this path.
    /// Multiple append edits for one path are retained and applied in order.
    pub fn append_collision(&mut self, raw_resource_path: impl Into<Vec<u8>>, document: ColFile) {
        let raw_resource_path = raw_resource_path.into();
        self.resource_removals
            .retain(|path| path != &raw_resource_path);
        self.collisions.push(StageCollisionEdit {
            raw_resource_path,
            document,
            mode: StageCollisionEditMode::Append,
        });
    }

    fn set_collision_edit(
        &mut self,
        raw_resource_path: Vec<u8>,
        document: ColFile,
        mode: StageCollisionEditMode,
    ) {
        self.resource_removals
            .retain(|path| path != &raw_resource_path);
        // A replacement/upsert starts a new base for this path. Appends made
        // after it remain ordered, while earlier operations are superseded.
        self.collisions
            .retain(|edit| edit.raw_resource_path != raw_resource_path);
        self.collisions.push(StageCollisionEdit {
            raw_resource_path,
            document,
            mode,
        });
    }

    /// Appends a complete typed JDrama record beneath an existing semantic
    /// group path. The record is rebuilt from fields and never carries an
    /// imported record buffer or stored key-code metadata.
    pub fn insert_placement(
        &mut self,
        raw_resource_path: impl Into<Vec<u8>>,
        parent_record_path: impl Into<Vec<usize>>,
        record: JDramaRecord,
    ) {
        self.placement_inserts.push(StagePlacementInsert {
            raw_resource_path: raw_resource_path.into(),
            parent_record_path: parent_record_path.into(),
            record,
        });
    }
}

impl StageDocument {
    /// Rebuilds the currently open stage with object transforms, deletions,
    /// typed duplicates, and fully typed placement inserts applied. The
    /// returned bytes never consult an imported record or archive buffer as an
    /// export fallback.
    pub fn build_stage_archive(&self) -> Result<Vec<u8>> {
        self.build_stage_archive_with_edits(&self.archive_edits)
    }

    /// Also applies explicitly supplied model, collision, and placement
    /// documents.
    pub fn build_stage_archive_with_edits(&self, edits: &StageArchiveEdits) -> Result<Vec<u8>> {
        self.reject_uncompiled_dialogue_export()?;
        Ok(self.build_stage_archive_inner(edits)?.rebuilt)
    }

    /// Rebuilds a stage after the caller has compiled dialogue authoring and
    /// merged [`CompiledDialogueEdits::stage_edits`] into `edits`.
    ///
    /// This explicit boundary prevents generic stage-only exporters from
    /// silently dropping common-BMG or managed-DOL work. Callers using this
    /// method are responsible for applying those non-stage outputs as part of
    /// the same managed build transaction.
    pub fn build_stage_archive_with_compiled_dialogue_edits(
        &self,
        edits: &StageArchiveEdits,
    ) -> Result<Vec<u8>> {
        Ok(self.build_stage_archive_inner(edits)?.rebuilt)
    }

    /// Creates a new external archive. Existing outputs and every path inside
    /// the extracted base directory are rejected.
    pub fn export_stage_archive_new(
        &self,
        output_path: impl AsRef<Path>,
    ) -> Result<StageArchiveExportOutcome> {
        self.export_stage_archive_with_edits_new(output_path, &self.archive_edits)
    }

    pub fn export_stage_archive_with_edits_new(
        &self,
        output_path: impl AsRef<Path>,
        edits: &StageArchiveEdits,
    ) -> Result<StageArchiveExportOutcome> {
        self.reject_uncompiled_dialogue_export()?;
        let built = self.build_stage_archive_inner(edits)?;
        let output_path = checked_external_output(&self.base_root, output_path.as_ref())?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        output.write_all(&built.rebuilt)?;
        output.sync_all()?;
        Ok(StageArchiveExportOutcome {
            source_path: built.source_path,
            output_path,
            size_bytes: built.rebuilt.len(),
            changed: built.changed,
        })
    }

    fn reject_uncompiled_dialogue_export(&self) -> Result<()> {
        let has_stage_overrides = self.dialogue_authoring.as_ref().is_some_and(|authoring| {
            authoring
                .objects
                .values()
                .any(|object| !object.overrides.is_empty() || !object.stable_allocations.is_empty())
        });
        if has_stage_overrides || !self.dialogue_library.is_empty() {
            return Err(stage_export_error(
                "dialogue authoring cannot be emitted by a stage-only export; compile it and use the managed game build so stage, common.szs, and DOL outputs remain atomic",
            ));
        }
        Ok(())
    }

    fn build_stage_archive_inner(&self, edits: &StageArchiveEdits) -> Result<BuiltStageArchive> {
        self.validate_for_export()?;
        let source_path = self.stage_archive_source_path.clone().ok_or_else(|| {
            stage_export_error("the stage has no semantic archive source identity")
        })?;
        let mut archive = self.stage_archive.clone().ok_or_else(|| {
            stage_export_error(
                "the stage has no detached semantic archive; reopen it with a supported strict importer",
            )
        })?;

        // This is regenerated from the pristine semantic import, not read from
        // the retail source path. It is both the edit baseline and proof that
        // the retained document remains independently encodable.
        let baseline = archive.encode()?;

        apply_resource_edits(&mut archive, edits)?;
        let has_goop = self
            .goop_authoring
            .as_ref()
            .is_some_and(|authoring| !authoring.layers.is_empty());
        apply_runtime_texture_replacements(
            &mut archive,
            &self.objects,
            self.registry.as_ref(),
            has_goop,
        )?;
        reconcile_scene_lighting(&mut archive, &self.lighting)?;
        let inserted_placement_roots = apply_placement_inserts(&mut archive, edits)?;
        let dialogue_owned_runtime_names = self
            .objects
            .iter()
            .filter(|object| self.owns_generated_dialogue_runtime_name(&object.id))
            .map(|object| object.id.clone())
            .collect::<BTreeSet<_>>();
        reconcile_scene_objects_with_owned_dialogue_names(
            &mut archive,
            &self.objects,
            &inserted_placement_roots,
            self.registry.as_ref(),
            &dialogue_owned_runtime_names,
        )?;
        let rebuilt = archive.encode()?;
        let reopened = SourceFreeStageArchive::parse(&rebuilt)?;
        if reopened.encode()? != rebuilt {
            return Err(stage_export_error(format!(
                "the edited semantic rebuild of '{}' was not stable after reimport",
                self.stage_id
            )));
        }
        let changed = rebuilt != baseline;
        Ok(BuiltStageArchive {
            source_path,
            rebuilt,
            changed,
        })
    }
}

fn apply_runtime_texture_replacements(
    archive: &mut SourceFreeStageArchive,
    objects: &[SceneObject],
    registry: Option<&ObjectRegistry>,
    has_goop: bool,
) -> Result<()> {
    // Retail-derived stages already exercise the engine's stock absolute
    // resource lookup. Blank/custom archives also bake the declared binding so
    // their first runtime frame cannot expose the model's tiny black dummy.
    if !matches!(archive.origin(), StageOrigin::Blank { .. }) {
        return Ok(());
    }
    let Some(registry) = registry else {
        return Ok(());
    };
    let factories = objects
        .iter()
        .map(|object| object.factory_name.as_str())
        .collect::<BTreeSet<_>>();
    let replacements = factories
        .into_iter()
        .flat_map(|factory| registry.runtime_texture_replacements_for(factory))
        .map(|replacement| {
            (
                replacement.dummy_texture_name.clone(),
                replacement.resource_path.clone(),
            )
        })
        .collect::<BTreeSet<_>>();

    for (dummy_texture_name, resource_path) in replacements {
        // A global actor such as Mario can consume the declared texture even
        // when its model is not stored in the stage archive. A blank stage with
        // no pollution layers cannot exercise that lookup and should not be
        // forced to carry an otherwise-unused resource.
        if !has_goop {
            continue;
        }
        let archive_path = runtime_texture_archive_path(&resource_path);
        let mut candidates = archive.resources().iter().filter_map(|resource| {
            let StageResourceDocument::Texture(texture) = &resource.document else {
                return None;
            };
            Some((
                String::from_utf8_lossy(&resource.raw_path).replace('\\', "/"),
                texture,
            ))
        });
        let texture = candidates
            .clone()
            .find_map(|(path, texture)| (path == archive_path).then(|| texture.clone()))
            .or_else(|| {
                candidates.find_map(|(path, texture)| {
                    path.eq_ignore_ascii_case(&archive_path)
                        .then(|| texture.clone())
                })
            })
            .ok_or_else(|| {
                stage_export_error(format!(
                    "runtime texture {resource_path:?} for dummy {dummy_texture_name:?} is missing from the stage archive"
                ))
            })?;

        for resource in archive.resources_mut() {
            let StageResourceDocument::Model(model) = &mut resource.document else {
                continue;
            };
            model
                .replace_named_texture_from_bti(&dummy_texture_name, &texture)
                .map_err(|source| SceneError::StageResource {
                    path: display_raw_path(&resource.raw_path),
                    source,
                })?;
        }
    }
    Ok(())
}

fn runtime_texture_archive_path(resource_path: &str) -> String {
    let normalized = resource_path.trim_start_matches('/').replace('\\', "/");
    normalized
        .strip_prefix("scene/")
        .unwrap_or(&normalized)
        .to_string()
}

fn reconcile_scene_lighting(
    archive: &mut SourceFreeStageArchive,
    lighting: &crate::StageLighting,
) -> Result<()> {
    if lighting.lights.is_empty() && lighting.ambients.is_empty() {
        return Ok(());
    }
    let Some(StageResourceDocument::Placement(document)) = archive.resource_mut(WORLD_SCENE_PATH)
    else {
        return Err(stage_export_error(
            "the archive has no typed map/scene.bin for authored lighting",
        ));
    };
    let mut light_index = 0_usize;
    let mut ambient_index = 0_usize;
    reconcile_lighting_record(
        &mut document.root,
        lighting,
        &mut light_index,
        &mut ambient_index,
    )?;
    if light_index != lighting.lights.len() || ambient_index != lighting.ambients.len() {
        return Err(stage_export_error(format!(
            "authored lighting count does not match map/scene.bin ({} of {} lights, {} of {} ambients)",
            light_index,
            lighting.lights.len(),
            ambient_index,
            lighting.ambients.len()
        )));
    }
    Ok(())
}

fn reconcile_lighting_record(
    record: &mut JDramaRecord,
    lighting: &crate::StageLighting,
    light_index: &mut usize,
    ambient_index: &mut usize,
) -> Result<()> {
    let short_type = record
        .type_name
        .rsplit("::")
        .next()
        .unwrap_or(&record.type_name);
    match short_type {
        "Light" => {
            let authored = lighting.lights.get(*light_index).ok_or_else(|| {
                stage_export_error(format!(
                    "map/scene.bin contains more typed lights than the authored scene (first extra: {:?})",
                    record.name
                ))
            })?;
            if authored
                .name
                .as_deref()
                .is_some_and(|name| name != record.name)
            {
                return Err(stage_export_error(format!(
                    "authored light order drifted: expected {:?}, found {:?}",
                    authored.name, record.name
                )));
            }
            set_typed_lighting_field(
                record,
                "position",
                JDramaFieldValue::Vec3F32(authored.position),
            )?;
            set_typed_lighting_field(
                record,
                "color",
                JDramaFieldValue::ColorRgba8(authored.color),
            )?;
            *light_index += 1;
        }
        "AmbColor" => {
            let authored = lighting.ambients.get(*ambient_index).ok_or_else(|| {
                stage_export_error(format!(
                    "map/scene.bin contains more typed ambients than the authored scene (first extra: {:?})",
                    record.name
                ))
            })?;
            if authored
                .name
                .as_deref()
                .is_some_and(|name| name != record.name)
            {
                return Err(stage_export_error(format!(
                    "authored ambient order drifted: expected {:?}, found {:?}",
                    authored.name, record.name
                )));
            }
            set_typed_lighting_field(
                record,
                "color",
                JDramaFieldValue::ColorRgba8(authored.color),
            )?;
            *ambient_index += 1;
        }
        _ => {}
    }
    if let JDramaRecordPayload::Group { children, .. } = &mut record.payload {
        for child in children {
            reconcile_lighting_record(child, lighting, light_index, ambient_index)?;
        }
    }
    Ok(())
}

fn set_typed_lighting_field(
    record: &mut JDramaRecord,
    field_name: &str,
    value: JDramaFieldValue,
) -> Result<()> {
    let fields = match &mut record.payload {
        JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Group { fields, .. } => fields,
        JDramaRecordPayload::Empty => {
            return Err(stage_export_error(format!(
                "typed lighting record {:?} has no fields",
                record.name
            )));
        }
    };
    let field = fields
        .iter_mut()
        .find(|field| field.name == field_name)
        .ok_or_else(|| {
            stage_export_error(format!(
                "typed lighting record {:?} has no {field_name} field",
                record.name
            ))
        })?;
    field.value = value;
    Ok(())
}

struct BuiltStageArchive {
    source_path: PathBuf,
    rebuilt: Vec<u8>,
    changed: bool,
}

fn apply_resource_edits(
    archive: &mut SourceFreeStageArchive,
    edits: &StageArchiveEdits,
) -> Result<()> {
    reject_duplicate_edit_paths(
        edits.resource_removals.iter().map(Vec::as_slice),
        "resource removal",
    )?;
    reject_duplicate_edit_paths(
        edits
            .resources
            .iter()
            .map(|edit| edit.raw_resource_path.as_slice()),
        "resource",
    )?;
    reject_duplicate_edit_paths(
        edits
            .models
            .iter()
            .map(|edit| edit.raw_resource_path.as_slice()),
        "model",
    )?;
    reject_duplicate_collision_bases(&edits.collisions)?;

    for raw_path in &edits.resource_removals {
        if archive.resource(raw_path).is_some() {
            archive.remove_resource(raw_path)?;
        }
    }

    // General resources are applied first so later typed edits can update or
    // append to a resource intentionally created by this transaction.
    for edit in &edits.resources {
        match edit.mode {
            StageResourceEditMode::Insert => {
                archive.insert_resource(edit.raw_resource_path.clone(), edit.document.clone())?
            }
            StageResourceEditMode::Upsert => {
                if archive.resource(&edit.raw_resource_path).is_some() {
                    archive.replace_resource(&edit.raw_resource_path, edit.document.clone())?;
                } else {
                    archive
                        .insert_resource(edit.raw_resource_path.clone(), edit.document.clone())?;
                }
            }
        }
    }

    for edit in &edits.models {
        let mut replacement = edit.document.clone();
        replacement
            .canonicalize_geometry_layout()
            .map_err(|source| SceneError::StageResource {
                path: display_raw_path(&edit.raw_resource_path),
                source,
            })?;
        match archive.resource(&edit.raw_resource_path) {
            Some(StageResourceDocument::Model(_)) => {
                archive.replace_resource(
                    &edit.raw_resource_path,
                    StageResourceDocument::Model(replacement),
                )?;
            }
            Some(_) => {
                return Err(stage_export_error(format!(
                    "{} is not a model resource",
                    display_raw_path(&edit.raw_resource_path)
                )));
            }
            None if edit.mode == StageModelEditMode::Upsert => {
                archive.insert_resource(
                    edit.raw_resource_path.clone(),
                    StageResourceDocument::Model(replacement),
                )?;
            }
            None => {
                return Err(stage_export_error(format!(
                    "model resource {} was not found",
                    display_raw_path(&edit.raw_resource_path)
                )));
            }
        }
    }
    for edit in &edits.collisions {
        match archive.resource(&edit.raw_resource_path) {
            Some(StageResourceDocument::Collision(document)) => {
                let replacement = match edit.mode {
                    StageCollisionEditMode::Replace | StageCollisionEditMode::Upsert => {
                        edit.document.clone()
                    }
                    StageCollisionEditMode::Append => append_collision_document(
                        document,
                        &edit.document,
                        &edit.raw_resource_path,
                    )?,
                };
                if edit.mode == StageCollisionEditMode::Append
                    && edit.raw_resource_path == WORLD_COLLISION_PATH
                {
                    preserve_world_collision_runtime_headroom(archive, &edit.document)?;
                }
                archive.replace_resource(
                    &edit.raw_resource_path,
                    StageResourceDocument::Collision(replacement),
                )?;
            }
            Some(_) => {
                return Err(stage_export_error(format!(
                    "{} is not a collision resource",
                    display_raw_path(&edit.raw_resource_path)
                )));
            }
            None if edit.mode == StageCollisionEditMode::Upsert => {
                archive.insert_resource(
                    edit.raw_resource_path.clone(),
                    StageResourceDocument::Collision(edit.document.clone()),
                )?;
            }
            None => {
                return Err(stage_export_error(format!(
                    "collision resource {} was not found",
                    display_raw_path(&edit.raw_resource_path)
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn append_collision_document(
    existing: &ColFile,
    authored: &ColFile,
    raw_resource_path: &[u8],
) -> Result<ColFile> {
    // Validate the authored document in its own index space before remapping.
    authored
        .encode()
        .map_err(|source| SceneError::StageResource {
            path: display_raw_path(raw_resource_path),
            source,
        })?;

    let vertex_base = existing.vertices().len();
    let mut appended_groups = authored.groups().to_vec();
    for (group_index, group) in appended_groups.iter_mut().enumerate() {
        for (triangle_index, triangle) in group.triangles.iter_mut().enumerate() {
            for vertex_index in &mut triangle.vertex_indices {
                let remapped = vertex_base
                    .checked_add(usize::from(*vertex_index))
                    .ok_or_else(|| {
                        stage_export_error(format!(
                            "collision append for {} overflowed while remapping group {group_index} triangle {triangle_index}",
                            display_raw_path(raw_resource_path)
                        ))
                    })?;
                if remapped > i16::MAX as usize {
                    return Err(stage_export_error(format!(
                        "collision append for {} cannot remap group {group_index} triangle {triangle_index} vertex {vertex_index}: index {remapped} exceeds the retail COL signed-index limit {}",
                        display_raw_path(raw_resource_path),
                        i16::MAX
                    )));
                }
                *vertex_index = remapped as u16;
            }
        }
    }

    let mut merged = existing.clone();
    merged.vertices_mut().extend_from_slice(authored.vertices());
    // Retail groups stay byte-semantically unchanged and in their original
    // order. Authored groups are appended rather than coalesced by surface.
    merged.groups_mut().append(&mut appended_groups);
    merged
        .encode()
        .map_err(|source| SceneError::StageResource {
            path: display_raw_path(raw_resource_path),
            source,
        })?;
    Ok(merged)
}

#[derive(Debug, Clone, Copy)]
struct MapCollisionRuntimeConfig {
    grid_width: i32,
    grid_height: i32,
    triangle_capacity: i32,
    list_capacity: i32,
}

#[derive(Debug, Clone, Copy)]
struct CollisionTrianglePoints {
    points: [[f32; 3]; 3],
    normal_y: f32,
}

fn preserve_world_collision_runtime_headroom(
    archive: &mut SourceFreeStageArchive,
    authored: &ColFile,
) -> Result<()> {
    let placement = match archive.resource_mut(WORLD_SCENE_PATH) {
        Some(StageResourceDocument::Placement(document)) => document,
        Some(_) => {
            return Err(stage_export_error(format!(
                "{} is not a typed placement resource required by {}",
                display_raw_path(WORLD_SCENE_PATH),
                display_raw_path(WORLD_COLLISION_PATH)
            )));
        }
        None => {
            return Err(stage_export_error(format!(
                "world collision append requires placement resource {}",
                display_raw_path(WORLD_SCENE_PATH)
            )));
        }
    };
    let map_record = unique_map_record_mut(placement)?;
    let JDramaRecordPayload::Fields { fields } = &mut map_record.payload else {
        return Err(stage_export_error(format!(
            "the unique Map record in {} does not have a typed fields payload",
            display_raw_path(WORLD_SCENE_PATH)
        )));
    };
    let config = read_map_collision_runtime_config(fields)?;
    let triangle_delta = authored_collision_triangle_count(authored)?;
    let list_delta = authored_collision_grid_link_count(authored, config)?;
    let triangle_capacity = config
        .triangle_capacity
        .checked_add(triangle_delta)
        .ok_or_else(|| {
            stage_export_error(format!(
                "Map field '{COLLISION_TRIANGLE_CAPACITY_FIELD}' overflows i32 while preserving {triangle_delta} authored collision triangles"
            ))
        })?;
    let list_capacity = config.list_capacity.checked_add(list_delta).ok_or_else(|| {
        stage_export_error(format!(
            "Map field '{COLLISION_LIST_CAPACITY_FIELD}' overflows i32 while preserving {list_delta} authored collision grid links"
        ))
    })?;

    set_unique_i32_field(fields, COLLISION_TRIANGLE_CAPACITY_FIELD, triangle_capacity)?;
    set_unique_i32_field(fields, COLLISION_LIST_CAPACITY_FIELD, list_capacity)?;
    Ok(())
}

fn unique_map_record_mut(document: &mut JDramaDocument) -> Result<&mut JDramaRecord> {
    let mut paths = Vec::new();
    collect_record_paths_by_type(&document.root, "Map", &mut Vec::new(), &mut paths);
    let path = match paths.as_slice() {
        [path] => path.as_slice(),
        [] => {
            return Err(stage_export_error(format!(
                "{} has no typed Map record for world collision capacities",
                display_raw_path(WORLD_SCENE_PATH)
            )));
        }
        _ => {
            return Err(stage_export_error(format!(
                "{} has {} typed Map records; world collision capacity target is ambiguous",
                display_raw_path(WORLD_SCENE_PATH),
                paths.len()
            )));
        }
    };
    jdrama_record_mut_at(&mut document.root, path)
}

fn collect_record_paths_by_type(
    record: &JDramaRecord,
    expected_type: &str,
    path: &mut Vec<usize>,
    matches: &mut Vec<Vec<usize>>,
) {
    if semantic_record_type_name(&record.type_name) == expected_type {
        matches.push(path.clone());
    }
    if let JDramaRecordPayload::Group { children, .. } = &record.payload {
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            collect_record_paths_by_type(child, expected_type, path, matches);
            path.pop();
        }
    }
}

fn jdrama_record_mut_at<'a>(
    mut record: &'a mut JDramaRecord,
    path: &[usize],
) -> Result<&'a mut JDramaRecord> {
    for (depth, index) in path.iter().copied().enumerate() {
        let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return Err(stage_export_error(format!(
                "Map record path {} crosses a non-group at depth {depth}",
                display_record_path(path)
            )));
        };
        record = children.get_mut(index).ok_or_else(|| {
            stage_export_error(format!(
                "Map record path {} has no child {index} at depth {depth}",
                display_record_path(path)
            ))
        })?;
    }
    Ok(record)
}

fn semantic_record_type_name(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

fn read_map_collision_runtime_config(fields: &[JDramaField]) -> Result<MapCollisionRuntimeConfig> {
    let grid_width = unique_i32_field(fields, COLLISION_GRID_WIDTH_FIELD)?;
    let grid_height = unique_i32_field(fields, COLLISION_GRID_HEIGHT_FIELD)?;
    let triangle_capacity = unique_i32_field(fields, COLLISION_TRIANGLE_CAPACITY_FIELD)?;
    let list_capacity = unique_i32_field(fields, COLLISION_LIST_CAPACITY_FIELD)?;
    let warp_capacity = unique_i32_field(fields, COLLISION_WARP_CAPACITY_FIELD)?;

    if grid_width <= 0 || grid_height <= 0 {
        return Err(stage_export_error(format!(
            "Map collision grid dimensions must be positive, got {grid_width}x{grid_height}"
        )));
    }
    grid_width.checked_mul(grid_height).ok_or_else(|| {
        stage_export_error(format!(
            "Map collision grid dimensions {grid_width}x{grid_height} overflow the runtime cell count"
        ))
    })?;
    for (name, value) in [
        (COLLISION_TRIANGLE_CAPACITY_FIELD, triangle_capacity),
        (COLLISION_LIST_CAPACITY_FIELD, list_capacity),
        (COLLISION_WARP_CAPACITY_FIELD, warp_capacity),
    ] {
        if value < 0 {
            return Err(stage_export_error(format!(
                "Map field '{name}' must be non-negative, got {value}"
            )));
        }
    }

    Ok(MapCollisionRuntimeConfig {
        grid_width,
        grid_height,
        triangle_capacity,
        list_capacity,
    })
}

fn unique_i32_field(fields: &[JDramaField], name: &str) -> Result<i32> {
    let index = unique_field_index(fields, name)?;
    match fields[index].value {
        JDramaFieldValue::I32(value) => Ok(value),
        _ => Err(stage_export_error(format!(
            "Map field '{name}' in {} is not typed i32",
            display_raw_path(WORLD_SCENE_PATH)
        ))),
    }
}

fn set_unique_i32_field(fields: &mut [JDramaField], name: &str, value: i32) -> Result<()> {
    let index = unique_field_index(fields, name)?;
    let JDramaFieldValue::I32(current) = &mut fields[index].value else {
        return Err(stage_export_error(format!(
            "Map field '{name}' in {} is not typed i32",
            display_raw_path(WORLD_SCENE_PATH)
        )));
    };
    *current = value;
    Ok(())
}

fn unique_field_index(fields: &[JDramaField], name: &str) -> Result<usize> {
    let matches = fields
        .iter()
        .enumerate()
        .filter_map(|(index, field)| (field.name == name).then_some(index))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(stage_export_error(format!(
            "Map record in {} is missing field '{name}'",
            display_raw_path(WORLD_SCENE_PATH)
        ))),
        _ => Err(stage_export_error(format!(
            "Map record in {} has {} fields named '{name}'",
            display_raw_path(WORLD_SCENE_PATH),
            matches.len()
        ))),
    }
}

fn authored_collision_triangle_count(authored: &ColFile) -> Result<i32> {
    let count = authored.groups().iter().try_fold(0usize, |count, group| {
        count.checked_add(group.triangles.len()).ok_or_else(|| {
            stage_export_error("authored world collision triangle count overflowed usize")
        })
    })?;
    i32::try_from(count).map_err(|_| {
        stage_export_error(format!(
            "authored world collision has {count} triangles, exceeding the runtime i32 capacity"
        ))
    })
}

fn authored_collision_grid_link_count(
    authored: &ColFile,
    config: MapCollisionRuntimeConfig,
) -> Result<i32> {
    let extent_x = (config.grid_width / 2) as f32 * COLLISION_GRID_CELL_SIZE;
    let extent_z = (config.grid_height / 2) as f32 * COLLISION_GRID_CELL_SIZE;
    if !extent_x.is_finite() || !extent_z.is_finite() {
        return Err(stage_export_error(
            "Map collision grid extents are not finite",
        ));
    }

    let mut link_count = 0i32;
    for (group_index, group) in authored.groups().iter().enumerate() {
        for (triangle_index, triangle) in group.triangles.iter().enumerate() {
            let points = collision_triangle_points(
                authored,
                triangle.vertex_indices,
                group_index,
                triangle_index,
            )?;
            let plane_type = collision_plane_type(group.surface_type, points.normal_y);
            let Some([min_x, min_z, max_x, max_z]) = collision_grid_bounds(
                points.points,
                plane_type,
                extent_x,
                extent_z,
                config.grid_width,
                config.grid_height,
                group_index,
                triangle_index,
            )?
            else {
                continue;
            };

            for z_index in min_z..=max_z {
                for x_index in min_x..=max_x {
                    let cell_min_x = x_index as f32 * COLLISION_GRID_CELL_SIZE - extent_x;
                    let cell_min_z = z_index as f32 * COLLISION_GRID_CELL_SIZE - extent_z;
                    let cell_max_x = (x_index + 1) as f32 * COLLISION_GRID_CELL_SIZE - extent_x;
                    let cell_max_z = (z_index + 1) as f32 * COLLISION_GRID_CELL_SIZE - extent_z;
                    let (cell_min_x, cell_min_z, cell_max_x, cell_max_z) =
                        if plane_type == CollisionPlaneType::Wall {
                            (
                                cell_min_x - COLLISION_WALL_PADDING,
                                cell_min_z - COLLISION_WALL_PADDING,
                                cell_max_x + COLLISION_WALL_PADDING,
                                cell_max_z + COLLISION_WALL_PADDING,
                            )
                        } else {
                            (cell_min_x, cell_min_z, cell_max_x, cell_max_z)
                        };
                    if polygon_is_in_grid(cell_min_x, cell_min_z, cell_max_x, cell_max_z, points) {
                        link_count = link_count.checked_add(1).ok_or_else(|| {
                            stage_export_error(format!(
                                "authored world collision grid-link count exceeds i32 at group {group_index} triangle {triangle_index}"
                            ))
                        })?;
                    }
                }
            }
        }
    }
    Ok(link_count)
}

fn collision_triangle_points(
    authored: &ColFile,
    indices: [u16; 3],
    group_index: usize,
    triangle_index: usize,
) -> Result<CollisionTrianglePoints> {
    let mut points = [[0.0; 3]; 3];
    for (point, index) in points.iter_mut().zip(indices) {
        *point = authored
            .vertices()
            .get(usize::from(index))
            .ok_or_else(|| {
                stage_export_error(format!(
                    "authored world collision group {group_index} triangle {triangle_index} references missing vertex {index}"
                ))
            })?
            .position;
        if point.iter().any(|component| !component.is_finite()) {
            return Err(stage_export_error(format!(
                "authored world collision group {group_index} triangle {triangle_index} has a non-finite vertex"
            )));
        }
    }

    let [point_1, point_2, point_3] = points;
    let normal = [
        (point_2[1] - point_1[1]) * (point_3[2] - point_2[2])
            - (point_2[2] - point_1[2]) * (point_3[1] - point_2[1]),
        (point_2[2] - point_1[2]) * (point_3[0] - point_2[0])
            - (point_2[0] - point_1[0]) * (point_3[2] - point_2[2]),
        (point_2[0] - point_1[0]) * (point_3[1] - point_2[1])
            - (point_2[1] - point_1[1]) * (point_3[0] - point_2[0]),
    ];
    if normal.iter().any(|component| !component.is_finite()) {
        return Err(stage_export_error(format!(
            "authored world collision group {group_index} triangle {triangle_index} overflows while calculating its normal"
        )));
    }
    let normal_y = if normal.iter().any(|component| *component != 0.0) {
        let magnitude_squared =
            normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2];
        if !magnitude_squared.is_finite() || magnitude_squared <= 0.0 {
            return Err(stage_export_error(format!(
                "authored world collision group {group_index} triangle {triangle_index} has an unrepresentable normal"
            )));
        }
        normal[1] / magnitude_squared.sqrt()
    } else {
        0.0
    };
    if !normal_y.is_finite() {
        return Err(stage_export_error(format!(
            "authored world collision group {group_index} triangle {triangle_index} has a non-finite normalized normal"
        )));
    }
    Ok(CollisionTrianglePoints { points, normal_y })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollisionPlaneType {
    Ground,
    Roof,
    Wall,
}

fn collision_plane_type(surface_type: u16, normal_y: f32) -> CollisionPlaneType {
    if surface_type == 0x0801 || normal_y > 0.2 {
        CollisionPlaneType::Ground
    } else if normal_y < -0.2 {
        CollisionPlaneType::Roof
    } else {
        CollisionPlaneType::Wall
    }
}

#[allow(clippy::too_many_arguments)]
fn collision_grid_bounds(
    points: [[f32; 3]; 3],
    plane_type: CollisionPlaneType,
    extent_x: f32,
    extent_z: f32,
    grid_width: i32,
    grid_height: i32,
    group_index: usize,
    triangle_index: usize,
) -> Result<Option<[i32; 4]>> {
    let mut min_x = points[0][0].min(points[1][0]).min(points[2][0]);
    let mut min_z = points[0][2].min(points[1][2]).min(points[2][2]);
    let mut max_x = points[0][0].max(points[1][0]).max(points[2][0]);
    let mut max_z = points[0][2].max(points[1][2]).max(points[2][2]);
    if max_x < -extent_x || max_z < -extent_z || min_x > extent_x || min_z > extent_z {
        return Ok(None);
    }
    if plane_type == CollisionPlaneType::Wall {
        min_x -= COLLISION_WALL_PADDING;
        min_z -= COLLISION_WALL_PADDING;
        max_x += COLLISION_WALL_PADDING;
        max_z += COLLISION_WALL_PADDING;
    }
    if [min_x, min_z, max_x, max_z]
        .iter()
        .any(|value| !value.is_finite())
    {
        return Err(stage_export_error(format!(
            "authored world collision group {group_index} triangle {triangle_index} has non-finite grid bounds"
        )));
    }

    let min_x = checked_trunc_grid_index(
        (min_x + extent_x) * COLLISION_GRID_CELL_RECIPROCAL,
        group_index,
        triangle_index,
    )?
    .max(0);
    let min_z = checked_trunc_grid_index(
        (min_z + extent_z) * COLLISION_GRID_CELL_RECIPROCAL,
        group_index,
        triangle_index,
    )?
    .max(0);
    let max_x = checked_trunc_grid_index(
        (max_x + extent_x) * COLLISION_GRID_CELL_RECIPROCAL,
        group_index,
        triangle_index,
    )?
    .min(grid_width - 1);
    let max_z = checked_trunc_grid_index(
        (max_z + extent_z) * COLLISION_GRID_CELL_RECIPROCAL,
        group_index,
        triangle_index,
    )?
    .min(grid_height - 1);
    if min_x > max_x || min_z > max_z {
        return Ok(None);
    }
    Ok(Some([min_x, min_z, max_x, max_z]))
}

fn checked_trunc_grid_index(value: f32, group_index: usize, triangle_index: usize) -> Result<i32> {
    if !value.is_finite() || (value as f64) < i32::MIN as f64 || (value as f64) > i32::MAX as f64 {
        return Err(stage_export_error(format!(
            "authored world collision group {group_index} triangle {triangle_index} has a grid index outside i32"
        )));
    }
    Ok(value as i32)
}

fn polygon_is_in_grid(
    min_x: f32,
    min_z: f32,
    max_x: f32,
    max_z: f32,
    triangle: CollisionTrianglePoints,
) -> bool {
    if triangle.normal_y < 0.0 {
        return true;
    }
    if triangle
        .points
        .iter()
        .any(|point| point_is_in_grid(point[0], point[2], min_x, min_z, max_x, max_z))
    {
        return true;
    }
    if [
        (min_x, min_z),
        (max_x, min_z),
        (min_x, max_z),
        (max_x, max_z),
    ]
    .into_iter()
    .any(|(x, z)| point_is_in_polygon(x, z, triangle.points))
    {
        return true;
    }
    check_line_polygon_collision(min_x, min_z, max_x, min_z, triangle.points)
        || check_line_polygon_collision(min_x, max_z, max_x, max_z, triangle.points)
        || check_line_polygon_collision(min_x, min_z, min_x, max_z, triangle.points)
        || check_line_polygon_collision(max_x, min_z, max_x, max_z, triangle.points)
}

fn point_is_in_grid(x: f32, z: f32, min_x: f32, min_z: f32, max_x: f32, max_z: f32) -> bool {
    min_x <= x && x <= max_x && min_z <= z && z <= max_z
}

fn point_is_in_polygon(x: f32, z: f32, points: [[f32; 3]; 3]) -> bool {
    let [point_1, point_2, point_3] = points;
    if (point_1[2] - z) * (point_2[0] - point_1[0]) - (point_1[0] - x) * (point_2[2] - point_1[2])
        < 0.0
    {
        return false;
    }
    if (point_2[2] - z) * (point_3[0] - point_2[0]) - (point_2[0] - x) * (point_3[2] - point_2[2])
        < 0.0
    {
        return false;
    }
    (point_3[2] - z) * (point_1[0] - point_3[0]) - (point_3[0] - x) * (point_1[2] - point_3[2])
        >= 0.0
}

fn check_line_polygon_collision(
    start_x: f32,
    start_z: f32,
    end_x: f32,
    end_z: f32,
    points: [[f32; 3]; 3],
) -> bool {
    let [point_1, point_2, point_3] = points;
    check_lines_collision(
        start_x, start_z, end_x, end_z, point_1[0], point_1[2], point_2[0], point_2[2],
    ) || check_lines_collision(
        start_x, start_z, end_x, end_z, point_2[0], point_2[2], point_3[0], point_3[2],
    ) || check_lines_collision(
        start_x, start_z, end_x, end_z, point_3[0], point_3[2], point_1[0], point_1[2],
    )
}

#[allow(clippy::too_many_arguments)]
fn check_lines_collision(
    a_x: f32,
    a_z: f32,
    b_x: f32,
    b_z: f32,
    c_x: f32,
    c_z: f32,
    d_x: f32,
    d_z: f32,
) -> bool {
    let delta_ab_x = b_x - a_x;
    let delta_ab_z = b_z - a_z;
    let cross_c = delta_ab_z * (c_x - b_x) - delta_ab_x * (c_z - b_z);
    let cross_d = delta_ab_z * (d_x - b_x) - delta_ab_x * (d_z - b_z);
    if (cross_c >= 0.0 && cross_d >= 0.0) || (cross_c < 0.0 && cross_d < 0.0) {
        return false;
    }

    let delta_cd_x = d_x - c_x;
    let delta_cd_z = d_z - c_z;
    let cross_a = delta_cd_z * (a_x - d_x) - delta_cd_x * (a_z - d_z);
    let cross_b = delta_cd_z * (b_x - d_x) - delta_cd_x * (b_z - d_z);
    !((cross_a >= 0.0 && cross_b >= 0.0) || (cross_a < 0.0 && cross_b < 0.0))
}

fn reject_duplicate_collision_bases(edits: &[StageCollisionEdit]) -> Result<()> {
    let mut unique = BTreeSet::new();
    for edit in edits {
        if edit.mode != StageCollisionEditMode::Append
            && !unique.insert(edit.raw_resource_path.clone())
        {
            return Err(stage_export_error(format!(
                "duplicate collision base edit for {}",
                display_raw_path(&edit.raw_resource_path)
            )));
        }
    }
    Ok(())
}

fn reject_duplicate_edit_paths<'a>(
    paths: impl Iterator<Item = &'a [u8]>,
    kind: &str,
) -> Result<()> {
    let mut unique = BTreeSet::new();
    for path in paths {
        if !unique.insert(path.to_vec()) {
            return Err(stage_export_error(format!(
                "duplicate {kind} edit for {}",
                display_raw_path(path)
            )));
        }
    }
    Ok(())
}

fn apply_placement_inserts(
    archive: &mut SourceFreeStageArchive,
    edits: &StageArchiveEdits,
) -> Result<Vec<PlacementAddress>> {
    let mut inserted_roots = Vec::with_capacity(edits.placement_inserts.len());
    for insert in &edits.placement_inserts {
        let record_path = archive
            .insert_placement_record(
                &insert.raw_resource_path,
                &insert.parent_record_path,
                insert.record.clone(),
            )
            .map_err(|error| {
                stage_export_error(format!(
                    "could not insert typed placement under {}:{}: {error}",
                    display_raw_path(&insert.raw_resource_path),
                    display_record_path(&insert.parent_record_path)
                ))
            })?;
        inserted_roots.push(PlacementAddress {
            raw_resource_path: insert.raw_resource_path.clone(),
            record_path,
        });
    }
    Ok(inserted_roots)
}
fn resolve_runtime_actor_names(objects: &[SceneObject]) -> Result<BTreeMap<String, String>> {
    let by_id = objects
        .iter()
        .map(|object| (object.id.as_str(), object))
        .collect::<BTreeMap<_, _>>();
    let mut by_target = BTreeMap::<String, String>::new();
    let mut by_runtime_name = BTreeMap::<String, String>::new();

    for owner in objects {
        for reference in &owner.runtime_references {
            let Some(target_id) = reference.target_object_id.as_deref() else {
                if reference.required {
                    return Err(stage_export_error(format!(
                        "object '{}' requires a {} actor for runtime lookup {:?}, but no target is selected",
                        owner.id, reference.required_factory_name, reference.runtime_name
                    )));
                }
                continue;
            };
            let target = by_id.get(target_id).ok_or_else(|| {
                stage_export_error(format!(
                    "object '{}' runtime lookup {:?} references missing object '{}'",
                    owner.id, reference.runtime_name, target_id
                ))
            })?;
            if target.factory_name != reference.required_factory_name {
                return Err(stage_export_error(format!(
                    "object '{}' runtime lookup {:?} requires factory '{}', but target '{}' is '{}'",
                    owner.id,
                    reference.runtime_name,
                    reference.required_factory_name,
                    target.id,
                    target.factory_name
                )));
            }
            if let Some(existing_name) =
                by_target.insert(target.id.clone(), reference.runtime_name.clone())
            {
                if existing_name != reference.runtime_name {
                    return Err(stage_export_error(format!(
                        "target object '{}' cannot satisfy both runtime names {:?} and {:?}",
                        target.id, existing_name, reference.runtime_name
                    )));
                }
            }
            if let Some(existing_target) =
                by_runtime_name.insert(reference.runtime_name.clone(), target.id.clone())
            {
                if existing_target != target.id {
                    return Err(stage_export_error(format!(
                        "runtime name {:?} is assigned to both '{}' and '{}'",
                        reference.runtime_name, existing_target, target.id
                    )));
                }
            }
        }
    }
    Ok(by_target)
}

#[cfg(test)]
fn reconcile_scene_objects(
    archive: &mut SourceFreeStageArchive,
    objects: &[SceneObject],
    inserted_placement_roots: &[PlacementAddress],
    registry: Option<&ObjectRegistry>,
) -> Result<()> {
    reconcile_scene_objects_with_owned_dialogue_names(
        archive,
        objects,
        inserted_placement_roots,
        registry,
        &BTreeSet::new(),
    )
}

fn reconcile_scene_objects_with_owned_dialogue_names(
    archive: &mut SourceFreeStageArchive,
    objects: &[SceneObject],
    inserted_placement_roots: &[PlacementAddress],
    registry: Option<&ObjectRegistry>,
    dialogue_owned_runtime_names: &BTreeSet<String>,
) -> Result<()> {
    let runtime_actor_names = resolve_runtime_actor_names(objects)?;
    let baseline = archive
        .object_placements()
        .into_iter()
        .filter(|placement| is_editor_placement_resource(&placement.raw_resource_path))
        .filter(|placement| {
            !inserted_placement_roots.iter().any(|root| {
                root.raw_resource_path == placement.raw_resource_path
                    && placement.record_path.starts_with(&root.record_path)
            })
        })
        .map(|placement| {
            (
                PlacementAddress {
                    raw_resource_path: placement.raw_resource_path.clone(),
                    record_path: placement.record_path.clone(),
                },
                placement,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let has_source_bound_object = objects.iter().any(|object| {
        matches!(
            object.placement,
            Some(PlacementBinding::Existing(_) | PlacementBinding::CloneOf(_))
        )
    });
    if baseline.is_empty() && has_source_bound_object {
        return Err(stage_export_error(
            "the archive has no canonical map/scene.bin actor records",
        ));
    }

    let mut existing = BTreeMap::<PlacementAddress, (&SceneObject, JDramaRecord)>::new();
    let mut clones = Vec::<(PlacementAddress, &SceneObject, JDramaRecord)>::new();
    let mut authored = Vec::<(&crate::AuthoredPlacement, &SceneObject, JDramaRecord)>::new();
    for object in objects {
        let Some(binding) = object.placement.as_ref() else {
            return Err(stage_export_error(format!(
                "object '{}' has no typed JDrama placement constructor",
                object.id
            )));
        };
        let dialogue_owns_name = dialogue_owned_runtime_names.contains(&object.id);
        if dialogue_owns_name && runtime_actor_names.contains_key(&object.id) {
            return Err(stage_export_error(format!(
                "object '{}' cannot use both an editor-generated dialogue identity and another runtime TNameRef binding",
                object.id
            )));
        }
        match binding {
            PlacementBinding::Existing(address) | PlacementBinding::CloneOf(address) => {
                let Some(placement) = baseline.get(address) else {
                    return Err(stage_export_error(format!(
                        "object '{}' references missing placement {}:{}",
                        object.id,
                        display_raw_path(&address.raw_resource_path),
                        display_record_path(&address.record_path)
                    )));
                };
                validate_object_identity(object, placement)?;
                let mut record = placement_record(archive, address)?.clone();
                crate::validate_object_parameter_links_with_owned_name(
                    &record,
                    object,
                    binding,
                    dialogue_owns_name,
                )?;
                crate::apply_object_parameter_edits(
                    &mut record,
                    object,
                    crate::ParameterApplyMode::AllCanonical,
                )?;
                if let Some(runtime_name) = runtime_actor_names.get(&object.id) {
                    record.name.clone_from(runtime_name);
                }
                match binding {
                    PlacementBinding::Existing(_) => {
                        if existing.insert(address.clone(), (object, record)).is_some() {
                            return Err(stage_export_error(format!(
                                "multiple existing objects reference {}:{}",
                                display_raw_path(&address.raw_resource_path),
                                display_record_path(&address.record_path)
                            )));
                        }
                    }
                    PlacementBinding::CloneOf(_) => {
                        clones.push((address.clone(), object, record));
                    }
                    PlacementBinding::Authored(_) => unreachable!(),
                }
            }
            PlacementBinding::Authored(placement) => {
                validate_authored_object_identity(object, placement)?;
                let mut record = placement.prototype.clone();
                crate::validate_object_parameter_links_with_owned_name(
                    &record,
                    object,
                    binding,
                    dialogue_owns_name,
                )?;
                crate::apply_object_parameter_edits(
                    &mut record,
                    object,
                    crate::ParameterApplyMode::AllCanonical,
                )?;
                if let Some(runtime_name) = runtime_actor_names.get(&object.id) {
                    record.name.clone_from(runtime_name);
                }
                authored.push((placement, object, record));
            }
        }
    }

    // Runtime managers and other support records must exist before their
    // actors are appended. Identity is the exact record name plus semantic
    // (namespace-insensitive) type, matching the runtime lookup contract.
    for (placement, object, _) in &authored {
        for dependency in &placement.dependencies {
            ensure_authored_dependency(
                archive,
                &placement.raw_resource_path,
                dependency,
                &object.id,
            )?;
        }
    }

    let mut manager_references = BTreeMap::<(Vec<u8>, String), usize>::new();
    let mut required_manager_references = BTreeSet::<(Vec<u8>, String)>::new();
    for (address, (object, record)) in &existing {
        count_manager_reference(
            &mut manager_references,
            &mut required_manager_references,
            &address.raw_resource_path,
            object,
            record,
            registry,
        )?;
    }
    for (address, object, record) in &clones {
        count_manager_reference(
            &mut manager_references,
            &mut required_manager_references,
            &address.raw_resource_path,
            object,
            record,
            registry,
        )?;
    }
    for (placement, object, record) in &authored {
        let (mut manager_names, mut required_names) =
            record_manager_names(record, object, registry)?;
        for dependency in &placement.dependencies {
            if dependency_has_typed_capacity(dependency) {
                manager_names.insert(dependency.record.name.clone());
                required_names.insert(dependency.record.name.clone());
            }
        }
        required_manager_references.extend(
            required_names
                .into_iter()
                .map(|name| (placement.raw_resource_path.clone(), name)),
        );
        for manager_name in manager_names {
            increment_manager_reference(
                &mut manager_references,
                &placement.raw_resource_path,
                manager_name,
            )?;
        }
    }
    for ((raw_resource_path, manager_name), count) in manager_references {
        let required_capacity = u32::try_from(count).map_err(|_| {
            stage_export_error(format!(
                "manager {manager_name:?} has too many authored object references"
            ))
        })?;
        let missing_is_error = required_manager_references
            .contains(&(raw_resource_path.clone(), manager_name.clone()));
        raise_manager_capacity_floor(
            archive,
            &raw_resource_path,
            &manager_name,
            required_capacity,
            missing_is_error,
        )?;
    }

    for (address, (object, record)) in &existing {
        *placement_record_mut(archive, address)? = record.clone();
        archive.set_object_transform(
            &address.raw_resource_path,
            &address.record_path,
            to_jdrama_transform(object),
        )?;
    }

    // Appending clones and authored records does not shift imported child
    // indices, so original addresses remain valid for later deletions.
    for (address, object, record) in clones {
        let (_, parent_path) = address.record_path.split_last().ok_or_else(|| {
            stage_export_error(format!("object '{}' references the root record", object.id))
        })?;
        let record_path =
            archive.insert_placement_record(&address.raw_resource_path, parent_path, record)?;
        archive.set_object_transform(
            &address.raw_resource_path,
            &record_path,
            to_jdrama_transform(object),
        )?;
    }
    for (placement, object, record) in authored {
        let parent_path = authored_target_group_path(
            archive,
            &placement.raw_resource_path,
            placement.target_group_index,
            &object.id,
        )?;
        let record_path =
            archive.insert_placement_record(&placement.raw_resource_path, &parent_path, record)?;
        archive.set_object_transform(
            &placement.raw_resource_path,
            &record_path,
            to_jdrama_transform(object),
        )?;
    }

    let mut deletions = baseline
        .keys()
        .filter(|address| !existing.contains_key(*address))
        .cloned()
        .collect::<Vec<_>>();
    deletions.sort_by(|left, right| {
        right
            .record_path
            .len()
            .cmp(&left.record_path.len())
            .then_with(|| right.raw_resource_path.cmp(&left.raw_resource_path))
            .then_with(|| right.record_path.cmp(&left.record_path))
    });
    for address in deletions {
        archive.remove_placement_record(&address.raw_resource_path, &address.record_path)?;
    }
    Ok(())
}

fn validate_object_identity(object: &SceneObject, placement: &StageObjectPlacement) -> Result<()> {
    if object.factory_name != placement.type_name {
        return Err(stage_export_error(format!(
            "object '{}' changed factory from '{}' to '{}' without a typed field mapping",
            object.id, placement.type_name, object.factory_name
        )));
    }
    Ok(())
}

fn validate_authored_object_identity(
    object: &SceneObject,
    placement: &crate::AuthoredPlacement,
) -> Result<()> {
    if semantic_record_type_name(&object.factory_name)
        != semantic_record_type_name(&placement.prototype.type_name)
    {
        return Err(stage_export_error(format!(
            "object '{}' changed authored factory from '{}' to '{}' without a typed field mapping",
            object.id, placement.prototype.type_name, object.factory_name
        )));
    }
    Ok(())
}

fn authored_target_group_path(
    archive: &SourceFreeStageArchive,
    raw_resource_path: &[u8],
    target_group_index: u32,
    object_id: &str,
) -> Result<Vec<usize>> {
    if !is_editor_placement_resource(raw_resource_path) {
        return Err(stage_export_error(format!(
            "object '{object_id}' targets non-canonical placement resource {}",
            display_raw_path(raw_resource_path)
        )));
    }

    archive
        .find_group_record_path(raw_resource_path, "IdxGroup", Some(target_group_index))?
        .ok_or_else(|| {
            stage_export_error(format!(
                "object '{object_id}' targets missing IdxGroup {target_group_index} in {}",
                display_raw_path(raw_resource_path)
            ))
        })
}

fn authored_dependency_target_path(
    archive: &SourceFreeStageArchive,
    raw_resource_path: &[u8],
    dependency: &crate::AuthoredPlacementDependency,
    object_id: &str,
) -> Result<Vec<usize>> {
    match dependency.target.as_ref() {
        None => authored_target_group_path(
            archive,
            raw_resource_path,
            dependency.target_group_index,
            object_id,
        ),
        Some(crate::AuthoredPlacementDependencyTarget::IndexedGroup { group_index }) => {
            authored_target_group_path(archive, raw_resource_path, *group_index, object_id)
        }
        Some(crate::AuthoredPlacementDependencyTarget::NamedGroup { type_name, name }) => {
            let matches = named_record_paths(archive, raw_resource_path, type_name, name)?;
            let [target_path] = matches.as_slice() else {
                return Err(stage_export_error(if matches.is_empty() {
                    format!(
                        "object '{object_id}' targets missing dependency container type '{}' named {:?} in {}",
                        semantic_record_type_name(type_name),
                        name,
                        display_raw_path(raw_resource_path)
                    )
                } else {
                    format!(
                        "object '{object_id}' targets ambiguous dependency container type '{}' named {:?} in {} ({} matches)",
                        semantic_record_type_name(type_name),
                        name,
                        display_raw_path(raw_resource_path),
                        matches.len()
                    )
                }));
            };
            let target = placement_record(
                archive,
                &PlacementAddress {
                    raw_resource_path: raw_resource_path.to_vec(),
                    record_path: target_path.clone(),
                },
            )?;
            if !matches!(target.payload, JDramaRecordPayload::Group { .. }) {
                return Err(stage_export_error(format!(
                    "object '{object_id}' dependency container type '{}' named {:?} is not a group in {}",
                    semantic_record_type_name(type_name),
                    name,
                    display_raw_path(raw_resource_path)
                )));
            }
            Ok(target_path.clone())
        }
    }
}

fn authored_dependency_target_description(
    dependency: &crate::AuthoredPlacementDependency,
) -> String {
    match dependency.target.as_ref() {
        None => format!("IdxGroup {}", dependency.target_group_index),
        Some(crate::AuthoredPlacementDependencyTarget::IndexedGroup { group_index }) => {
            format!("IdxGroup {group_index}")
        }
        Some(crate::AuthoredPlacementDependencyTarget::NamedGroup { type_name, name }) => format!(
            "container type '{}' named {:?}",
            semantic_record_type_name(type_name),
            name
        ),
    }
}

fn ensure_authored_dependency(
    archive: &mut SourceFreeStageArchive,
    raw_resource_path: &[u8],
    dependency: &crate::AuthoredPlacementDependency,
    object_id: &str,
) -> Result<()> {
    let target_path =
        authored_dependency_target_path(archive, raw_resource_path, dependency, object_id)?;
    let conflicts = conflicting_named_records(
        archive,
        raw_resource_path,
        &dependency.record.type_name,
        &dependency.record.name,
    )?;
    if let Some((_, conflicting_type)) = conflicts.first() {
        return Err(stage_export_error(format!(
            "dependency name {:?} for object '{}' is already used by semantic type '{}' in {} ({} conflicting record(s))",
            dependency.record.name,
            object_id,
            semantic_record_type_name(conflicting_type),
            display_raw_path(raw_resource_path),
            conflicts.len()
        )));
    }
    let matches = named_record_paths(
        archive,
        raw_resource_path,
        &dependency.record.type_name,
        &dependency.record.name,
    )?;
    match matches.as_slice() {
        [] => {
            archive.insert_placement_record(
                raw_resource_path,
                &target_path,
                dependency.record.clone(),
            )?;
        }
        [record_path] => {
            let (_, parent_path) = record_path.split_last().ok_or_else(|| {
                stage_export_error(format!(
                    "dependency type '{}' named {:?} unexpectedly resolves to the placement root",
                    semantic_record_type_name(&dependency.record.type_name),
                    dependency.record.name
                ))
            })?;
            if parent_path != target_path {
                return Err(stage_export_error(format!(
                    "dependency type '{}' named {:?} for object '{}' exists outside {}",
                    semantic_record_type_name(&dependency.record.type_name),
                    dependency.record.name,
                    object_id,
                    authored_dependency_target_description(dependency)
                )));
            }
            let address = PlacementAddress {
                raw_resource_path: raw_resource_path.to_vec(),
                record_path: record_path.clone(),
            };
            let compatibility = {
                let existing = placement_record(archive, &address)?;
                dependency_record_compatibility(existing, &dependency.record)
            };
            match compatibility {
                DependencyRecordCompatibility::Exact => {}
                DependencyRecordCompatibility::Capacity(merged_capacity) => {
                    let existing = placement_record_mut(archive, &address)?;
                    let capacity = unique_dependency_capacity_mut(existing).ok_or_else(|| {
                        stage_export_error(format!(
                            "dependency type '{}' named {:?} for object '{}' lost its unique typed capacity while merging",
                            semantic_record_type_name(&dependency.record.type_name),
                            dependency.record.name,
                            object_id
                        ))
                    })?;
                    *capacity = merged_capacity;
                }
                DependencyRecordCompatibility::Incompatible => {
                    return Err(stage_export_error(format!(
                        "dependency type '{}' named {:?} for object '{}' has an incompatible payload in {}",
                        semantic_record_type_name(&dependency.record.type_name),
                        dependency.record.name,
                        object_id,
                        display_raw_path(raw_resource_path)
                    )));
                }
            }
        }
        _ => {
            return Err(stage_export_error(format!(
                "dependency type '{}' named {:?} for object '{}' is ambiguous in {} ({} matches)",
                semantic_record_type_name(&dependency.record.type_name),
                dependency.record.name,
                object_id,
                display_raw_path(raw_resource_path),
                matches.len()
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependencyRecordCompatibility {
    Exact,
    Capacity(u32),
    Incompatible,
}

fn dependency_record_compatibility(
    existing: &JDramaRecord,
    requested: &JDramaRecord,
) -> DependencyRecordCompatibility {
    if existing.payload == requested.payload {
        return DependencyRecordCompatibility::Exact;
    }

    let mut normalized_existing = existing.clone();
    let mut normalized_requested = requested.clone();
    let Some(existing_capacity) = normalize_unique_dependency_capacity(&mut normalized_existing)
    else {
        return DependencyRecordCompatibility::Incompatible;
    };
    let Some(requested_capacity) = normalize_unique_dependency_capacity(&mut normalized_requested)
    else {
        return DependencyRecordCompatibility::Incompatible;
    };
    if normalized_existing.payload != normalized_requested.payload {
        return DependencyRecordCompatibility::Incompatible;
    }

    DependencyRecordCompatibility::Capacity(existing_capacity.max(requested_capacity))
}

fn normalize_unique_dependency_capacity(record: &mut JDramaRecord) -> Option<u32> {
    let capacity = unique_dependency_capacity_mut(record)?;
    let value = *capacity;
    *capacity = 0;
    Some(value)
}

fn unique_dependency_capacity_mut(record: &mut JDramaRecord) -> Option<&mut u32> {
    let fields = jdrama_record_fields_mut(record)?;
    let capacity_indices = fields
        .iter()
        .enumerate()
        .filter_map(|(index, field)| (field.name == "capacity").then_some(index))
        .collect::<Vec<_>>();
    let [capacity_index] = capacity_indices.as_slice() else {
        return None;
    };
    match &mut fields[*capacity_index].value {
        JDramaFieldValue::U32(capacity) => Some(capacity),
        _ => None,
    }
}

fn named_record_paths(
    archive: &SourceFreeStageArchive,
    raw_resource_path: &[u8],
    type_name: &str,
    name: &str,
) -> Result<Vec<Vec<usize>>> {
    let Some(StageResourceDocument::Placement(document)) = archive.resource(raw_resource_path)
    else {
        return Err(stage_export_error(format!(
            "placement resource {} was not found",
            display_raw_path(raw_resource_path)
        )));
    };
    let mut matches = Vec::new();
    collect_named_record_paths(
        &document.root,
        type_name,
        name,
        &mut Vec::new(),
        &mut matches,
    );
    Ok(matches)
}

fn collect_named_record_paths(
    record: &JDramaRecord,
    type_name: &str,
    name: &str,
    path: &mut Vec<usize>,
    matches: &mut Vec<Vec<usize>>,
) {
    if semantic_record_type_name(&record.type_name) == semantic_record_type_name(type_name)
        && record.name == name
    {
        matches.push(path.clone());
    }
    if let JDramaRecordPayload::Group { children, .. } = &record.payload {
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            collect_named_record_paths(child, type_name, name, path, matches);
            path.pop();
        }
    }
}

fn count_manager_reference(
    references: &mut BTreeMap<(Vec<u8>, String), usize>,
    required_references: &mut BTreeSet<(Vec<u8>, String)>,
    raw_resource_path: &[u8],
    object: &SceneObject,
    record: &JDramaRecord,
    registry: Option<&ObjectRegistry>,
) -> Result<()> {
    let (manager_names, required_names) = record_manager_names(record, object, registry)?;
    required_references.extend(
        required_names
            .into_iter()
            .map(|name| (raw_resource_path.to_vec(), name)),
    );
    for manager_name in manager_names {
        increment_manager_reference(references, raw_resource_path, manager_name)?;
    }
    Ok(())
}

fn conflicting_named_records(
    archive: &SourceFreeStageArchive,
    raw_resource_path: &[u8],
    expected_type_name: &str,
    name: &str,
) -> Result<Vec<(Vec<usize>, String)>> {
    let Some(StageResourceDocument::Placement(document)) = archive.resource(raw_resource_path)
    else {
        return Err(stage_export_error(format!(
            "placement resource {} was not found",
            display_raw_path(raw_resource_path)
        )));
    };
    let mut conflicts = Vec::new();
    collect_conflicting_named_records(
        &document.root,
        expected_type_name,
        name,
        &mut Vec::new(),
        &mut conflicts,
    );
    Ok(conflicts)
}

fn collect_conflicting_named_records(
    record: &JDramaRecord,
    expected_type_name: &str,
    name: &str,
    path: &mut Vec<usize>,
    conflicts: &mut Vec<(Vec<usize>, String)>,
) {
    if record.name == name
        && semantic_record_type_name(&record.type_name)
            != semantic_record_type_name(expected_type_name)
    {
        conflicts.push((path.clone(), record.type_name.clone()));
    }
    if let JDramaRecordPayload::Group { children, .. } = &record.payload {
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            collect_conflicting_named_records(child, expected_type_name, name, path, conflicts);
            path.pop();
        }
    }
}

fn increment_manager_reference(
    references: &mut BTreeMap<(Vec<u8>, String), usize>,
    raw_resource_path: &[u8],
    manager_name: String,
) -> Result<()> {
    let count = references
        .entry((raw_resource_path.to_vec(), manager_name))
        .or_default();
    *count = count
        .checked_add(1)
        .ok_or_else(|| stage_export_error("manager reference count overflowed usize"))?;
    Ok(())
}

fn dependency_has_typed_capacity(dependency: &crate::AuthoredPlacementDependency) -> bool {
    jdrama_record_fields(&dependency.record).is_some_and(|fields| {
        let mut capacities = fields.iter().filter(|field| field.name == "capacity");
        matches!(
            (capacities.next(), capacities.next()),
            (
                Some(JDramaField {
                    value: JDramaFieldValue::U32(_),
                    ..
                }),
                None
            )
        )
    })
}

fn record_manager_names(
    record: &JDramaRecord,
    object: &SceneObject,
    registry: Option<&ObjectRegistry>,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut required_names = crate::resolved_object_manager_capacity_dependencies(object, registry);
    let mut manager_names = required_names.clone();
    if let Some(manager_name) = record_string_field(record, &object.id, "manager_name")? {
        manager_names.insert(manager_name);
    }
    if let Some(manager_name) = record_string_field(record, &object.id, "launched_enemy_name")? {
        manager_names.insert(manager_name.clone());
        required_names.insert(manager_name);
    }

    // TMapObjData resolves the runtime manager through the exact resource
    // identity, so use the decomp-derived registry relation instead of a
    // per-factory exception. This also covers CloneOf records, which do not
    // own authored dependency records.
    if let (Some(registry), Some(resource_name)) = (
        registry,
        record_string_field(record, &object.id, "resource_name")?,
    ) {
        if let Some(manager_name) = registry
            .find_map_obj_resource(&resource_name)
            .map(|resource| resource.required_manager_name.as_str())
            .filter(|name| !name.is_empty())
        {
            manager_names.insert(manager_name.to_string());
            required_names.insert(manager_name.to_string());
        }
    }

    Ok((manager_names, required_names))
}

fn record_string_field(
    record: &JDramaRecord,
    object_id: &str,
    field_name: &str,
) -> Result<Option<String>> {
    let Some(fields) = jdrama_record_fields(record) else {
        return Ok(None);
    };
    let matching_fields = fields
        .iter()
        .filter(|field| field.name == field_name)
        .collect::<Vec<_>>();
    match matching_fields.as_slice() {
        [] => Ok(None),
        [field] => match &field.value {
            JDramaFieldValue::String(value) if value.is_empty() => Ok(None),
            JDramaFieldValue::String(value) => Ok(Some(value.clone())),
            _ => Err(stage_export_error(format!(
                "object '{object_id}' has a {field_name} field that is not a typed string"
            ))),
        },
        _ => Err(stage_export_error(format!(
            "object '{object_id}' has multiple {field_name} fields"
        ))),
    }
}

fn raise_manager_capacity_floor(
    archive: &mut SourceFreeStageArchive,
    raw_resource_path: &[u8],
    manager_name: &str,
    required_capacity: u32,
    missing_is_error: bool,
) -> Result<()> {
    let matches = {
        let Some(StageResourceDocument::Placement(document)) = archive.resource(raw_resource_path)
        else {
            return Err(stage_export_error(format!(
                "placement resource {} was not found",
                display_raw_path(raw_resource_path)
            )));
        };
        let mut matches = Vec::new();
        collect_manager_capacity_paths(&document.root, manager_name, &mut Vec::new(), &mut matches);
        matches
    };
    let record_path = match matches.as_slice() {
        [] if missing_is_error => {
            return Err(stage_export_error(format!(
                "manager named {manager_name:?} with exactly one typed capacity field was not found in {}",
                display_raw_path(raw_resource_path)
            )));
        }
        [] => return Ok(()),
        [record_path] => record_path.clone(),
        _ => {
            return Err(stage_export_error(format!(
                "manager named {manager_name:?} with a capacity field is ambiguous in {} ({} matches)",
                display_raw_path(raw_resource_path),
                matches.len()
            )));
        }
    };
    let address = PlacementAddress {
        raw_resource_path: raw_resource_path.to_vec(),
        record_path,
    };
    let record = placement_record_mut(archive, &address)?;
    let fields = jdrama_record_fields_mut(record).ok_or_else(|| {
        stage_export_error(format!(
            "manager named {manager_name:?} has no typed field payload"
        ))
    })?;
    let capacity_fields = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.name == "capacity")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [capacity_index] = capacity_fields.as_slice() else {
        return Err(stage_export_error(format!(
            "manager named {manager_name:?} does not have exactly one capacity field"
        )));
    };
    let JDramaFieldValue::U32(capacity) = &mut fields[*capacity_index].value else {
        return Err(stage_export_error(format!(
            "manager named {manager_name:?} has a capacity field that is not typed u32"
        )));
    };
    if *capacity < required_capacity {
        *capacity = required_capacity;
    }
    Ok(())
}

fn collect_manager_capacity_paths(
    record: &JDramaRecord,
    manager_name: &str,
    path: &mut Vec<usize>,
    matches: &mut Vec<Vec<usize>>,
) {
    if record.name == manager_name
        && jdrama_record_fields(record)
            .is_some_and(|fields| fields.iter().any(|field| field.name == "capacity"))
    {
        matches.push(path.clone());
    }
    if let JDramaRecordPayload::Group { children, .. } = &record.payload {
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            collect_manager_capacity_paths(child, manager_name, path, matches);
            path.pop();
        }
    }
}

fn jdrama_record_fields(record: &JDramaRecord) -> Option<&[JDramaField]> {
    match &record.payload {
        JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Group { fields, .. } => Some(fields),
        JDramaRecordPayload::Empty => None,
    }
}

fn jdrama_record_fields_mut(record: &mut JDramaRecord) -> Option<&mut Vec<JDramaField>> {
    match &mut record.payload {
        JDramaRecordPayload::Fields { fields }
        | JDramaRecordPayload::Actor { fields, .. }
        | JDramaRecordPayload::Group { fields, .. } => Some(fields),
        JDramaRecordPayload::Empty => None,
    }
}

fn placement_record_mut<'a>(
    archive: &'a mut SourceFreeStageArchive,
    address: &PlacementAddress,
) -> Result<&'a mut JDramaRecord> {
    let Some(StageResourceDocument::Placement(document)) =
        archive.resource_mut(&address.raw_resource_path)
    else {
        return Err(stage_export_error(format!(
            "placement resource {} was not found",
            display_raw_path(&address.raw_resource_path)
        )));
    };
    let mut record = &mut document.root;
    for index in &address.record_path {
        let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return Err(stage_export_error(format!(
                "placement path {} crosses a non-group",
                display_record_path(&address.record_path)
            )));
        };
        record = children.get_mut(*index).ok_or_else(|| {
            stage_export_error(format!(
                "placement path {} is outside {}",
                display_record_path(&address.record_path),
                display_raw_path(&address.raw_resource_path)
            ))
        })?;
    }
    Ok(record)
}
fn placement_record<'a>(
    archive: &'a SourceFreeStageArchive,
    address: &PlacementAddress,
) -> Result<&'a JDramaRecord> {
    let Some(StageResourceDocument::Placement(document)) =
        archive.resource(&address.raw_resource_path)
    else {
        return Err(stage_export_error(format!(
            "placement resource {} was not found",
            display_raw_path(&address.raw_resource_path)
        )));
    };
    let mut record = &document.root;
    for index in &address.record_path {
        let JDramaRecordPayload::Group { children, .. } = &record.payload else {
            return Err(stage_export_error(format!(
                "placement path {} crosses a non-group",
                display_record_path(&address.record_path)
            )));
        };
        record = children.get(*index).ok_or_else(|| {
            stage_export_error(format!(
                "placement path {} is outside {}",
                display_record_path(&address.record_path),
                display_raw_path(&address.raw_resource_path)
            ))
        })?;
    }
    Ok(record)
}

fn to_jdrama_transform(object: &SceneObject) -> JDramaTransform {
    JDramaTransform {
        translation: object.transform.translation,
        rotation: object.transform.rotation_degrees,
        scale: object.transform.scale,
    }
}

fn is_editor_placement_resource(raw_path: &[u8]) -> bool {
    let lower = raw_path
        .iter()
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    lower == b"scene.bin" || lower == b"map/scene.bin" || lower.ends_with(b"/map/scene.bin")
}

pub(crate) fn exact_stage_archive_path(base_root: &Path, stage_id: &str) -> Result<PathBuf> {
    let matches = discover_scene_archives(base_root)?
        .into_iter()
        .filter(|archive| archive.stage_id.eq_ignore_ascii_case(stage_id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [archive] => Ok(archive.path.clone()),
        [] => Err(stage_export_error(format!(
            "no scene archive exactly matches stage '{stage_id}'"
        ))),
        _ => Err(stage_export_error(format!(
            "multiple scene archives exactly match stage '{stage_id}'"
        ))),
    }
}

/// Imports the complete strict semantic archive once. The source bytes are
/// used only for this import proof and are dropped before the document enters
/// editor state or project persistence.
pub(crate) fn import_exact_stage_archive(
    base_root: &Path,
    stage_id: &str,
) -> Result<(PathBuf, SourceFreeStageArchive)> {
    let source_path = exact_stage_archive_path(base_root, stage_id)?;
    let source = fs::read(&source_path)?;
    let archive = SourceFreeStageArchive::parse(&source)?;
    let rebuilt = archive.encode()?;
    if rebuilt != source {
        return Err(stage_export_error(format!(
            "the unedited semantic rebuild of '{stage_id}' was not byte-identical"
        )));
    }
    Ok((source_path, archive))
}

fn checked_external_output(base_root: &Path, output_path: &Path) -> Result<PathBuf> {
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| stage_export_error("output path must include an existing parent"))?;
    if !parent.is_dir() {
        return Err(stage_export_error(format!(
            "output parent does not exist: {}",
            parent.display()
        )));
    }
    let file_name = output_path
        .file_name()
        .ok_or_else(|| stage_export_error("output path has no archive filename"))?;
    let canonical_base = fs::canonicalize(base_root)?;
    let canonical_output = fs::canonicalize(parent)?.join(file_name);
    if path_is_same_or_child(&canonical_output, &canonical_base) {
        return Err(SceneError::StageArchiveOutputOverlapsBase(canonical_output));
    }
    Ok(canonical_output)
}

fn path_is_same_or_child(path: &Path, parent: &Path) -> bool {
    let normalize = |value: &Path| {
        value
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    let path = normalize(path);
    let parent = normalize(parent);
    path == parent
        || path
            .strip_prefix(&parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn display_raw_path(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn display_record_path(path: &[usize]) -> String {
    if path.is_empty() {
        return "root".to_string();
    }
    path.iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join("/")
}

fn stage_export_error(message: impl Into<String>) -> SceneError {
    SceneError::StageExport(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sms_formats::{
        BmgMessage, ColGroup, ColTriangle, ColVertex, JDramaAmbient, JDramaDocument, JDramaField,
        JDramaFieldValue, JDramaLight, JDramaLightMap, PrmEntry, PrmFile, PrmValue, RarcDocument,
        RarcEntryRecord, RarcLayout, RarcNodeRecord,
    };

    use crate::{
        AuthoredPlacement, AuthoredPlacementDependency, AuthoredPlacementDependencyTarget,
        PlacementBinding, SceneRuntimeReferenceBinding, Transform,
    };

    #[test]
    fn runtime_texture_binding_without_goop_needs_no_texture_resource() {
        let mut archive = SourceFreeStageArchive::new_for_blank("custom0", 1).unwrap();
        let objects = vec![SceneObject::new("mario", "Mario")];
        let registry = ObjectRegistry {
            runtime_texture_replacements: vec![sms_schema::RuntimeTextureReplacementDefinition {
                factory_name: "Mario".to_string(),
                dummy_texture_name: "H_ma_rak_dummy".to_string(),
                resource_path: "/scene/map/pollution/H_ma_rak.bti".to_string(),
                source_file: "src/Player/MarioDraw.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };

        apply_runtime_texture_replacements(&mut archive, &objects, Some(&registry), false)
            .expect("a stage without goop does not need an unused global texture");
        assert!(archive.resources().is_empty());
    }

    #[test]
    fn global_actor_runtime_texture_is_required_when_custom_stage_has_goop() {
        let mut archive = SourceFreeStageArchive::new_for_blank("custom0", 1).unwrap();
        let objects = vec![SceneObject::new("mario", "Mario")];
        let registry = ObjectRegistry {
            runtime_texture_replacements: vec![sms_schema::RuntimeTextureReplacementDefinition {
                factory_name: "Mario".to_string(),
                dummy_texture_name: "H_ma_rak_dummy".to_string(),
                resource_path: "/scene/map/pollution/H_ma_rak.bti".to_string(),
                source_file: "src/Player/MarioDraw.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };

        let error =
            apply_runtime_texture_replacements(&mut archive, &objects, Some(&registry), true)
                .expect_err("a custom stage with goop must carry Mario's declared runtime texture");
        assert!(error.to_string().contains("H_ma_rak.bti"));
    }

    #[test]
    fn runtime_actor_link_binds_the_selected_arbitrary_shine_instance() {
        let mut archive = authoring_strategy_archive(Vec::new());

        let mut owner_record = actor_record("reward trigger", [0.0; 3]);
        owner_record.type_name = "FixtureTrigger".to_string();
        let mut owner = authored_test_object("owner", owner_record, 7, Vec::new());
        owner.runtime_references = vec![SceneRuntimeReferenceBinding {
            required_factory_name: "Shine".to_string(),
            runtime_name: "runtime reward name".to_string(),
            required: true,
            target_object_id: Some("shine-selected".to_string()),
        }];

        let mut first_record = actor_record("first authored shine", [10.0, 0.0, 0.0]);
        first_record.type_name = "Shine".to_string();
        let first = authored_test_object("shine-unselected", first_record, 7, Vec::new());

        let mut selected_record = actor_record("second authored shine", [20.0, 0.0, 0.0]);
        selected_record.type_name = "Shine".to_string();
        let selected = authored_test_object("shine-selected", selected_record, 7, Vec::new());

        reconcile_scene_objects(&mut archive, &[owner, first, selected], &[], None).unwrap();

        let mut shine_names = archive
            .object_placements()
            .into_iter()
            .filter(|placement| placement.type_name == "Shine")
            .map(|placement| placement.name)
            .collect::<Vec<_>>();
        shine_names.sort();
        assert_eq!(shine_names, ["first authored shine", "runtime reward name"]);
    }

    #[test]
    fn runtime_actor_link_rejects_an_unassigned_target_before_export() {
        let mut owner = SceneObject::new("owner", "FixtureTrigger");
        owner.runtime_references = vec![SceneRuntimeReferenceBinding {
            required_factory_name: "Shine".to_string(),
            runtime_name: "runtime reward name".to_string(),
            required: true,
            target_object_id: None,
        }];

        let error = resolve_runtime_actor_names(&[owner])
            .unwrap_err()
            .to_string();
        assert!(error.contains("no target is selected"), "{error}");
    }

    #[test]
    fn optional_runtime_actor_link_may_remain_unassigned() {
        let mut owner = SceneObject::new("owner", "FixtureTrigger");
        owner.runtime_references = vec![SceneRuntimeReferenceBinding {
            required_factory_name: "Shine".to_string(),
            runtime_name: "optional reward name".to_string(),
            required: false,
            target_object_id: None,
        }];

        assert!(resolve_runtime_actor_names(&[owner]).unwrap().is_empty());
    }

    #[test]
    fn authored_stage_lighting_rewrites_typed_records_by_stable_order_and_name() {
        let light = JDramaRecord::new(
            "Light",
            "Object key",
            JDramaRecordPayload::Fields {
                fields: vec![
                    JDramaField {
                        name: "position".to_string(),
                        value: JDramaFieldValue::Vec3F32([1.0, 2.0, 3.0]),
                    },
                    JDramaField {
                        name: "color".to_string(),
                        value: JDramaFieldValue::ColorRgba8([4, 5, 6, 7]),
                    },
                    JDramaField {
                        name: "range".to_string(),
                        value: JDramaFieldValue::F32(50.0),
                    },
                ],
            },
        )
        .unwrap();
        let ambient = JDramaRecord::new(
            "AmbColor",
            "Object ambient",
            JDramaRecordPayload::Fields {
                fields: vec![JDramaField {
                    name: "color".to_string(),
                    value: JDramaFieldValue::ColorRgba8([8, 9, 10, 11]),
                }],
            },
        )
        .unwrap();
        let root = JDramaRecord::new(
            "GroupObj",
            "scene",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![light, ambient],
            },
        )
        .unwrap();
        let mut archive = SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(
                WORLD_SCENE_PATH.to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            )
            .unwrap();
        let lighting = crate::StageLighting {
            lights: vec![JDramaLight {
                name: Some("Object key".to_string()),
                position: [100.0, 200.0, 300.0],
                color: [20, 30, 40, 50],
            }],
            ambients: vec![JDramaAmbient {
                name: Some("Object ambient".to_string()),
                color: [60, 70, 80, 90],
            }],
        };

        reconcile_scene_lighting(&mut archive, &lighting).unwrap();
        let StageResourceDocument::Placement(scene) = archive.resource(WORLD_SCENE_PATH).unwrap()
        else {
            panic!("scene resource was not typed placement");
        };
        let JDramaRecordPayload::Group { children, .. } = &scene.root.payload else {
            panic!("scene root was not a group");
        };
        let JDramaRecordPayload::Fields { fields } = &children[0].payload else {
            panic!("light was not typed fields");
        };
        assert_eq!(
            fields[0].value,
            JDramaFieldValue::Vec3F32([100.0, 200.0, 300.0])
        );
        assert_eq!(
            fields[1].value,
            JDramaFieldValue::ColorRgba8([20, 30, 40, 50])
        );
        assert_eq!(fields[2].value, JDramaFieldValue::F32(50.0));
        let JDramaRecordPayload::Fields { fields } = &children[1].payload else {
            panic!("ambient was not typed fields");
        };
        assert_eq!(
            fields[0].value,
            JDramaFieldValue::ColorRgba8([60, 70, 80, 90])
        );
    }

    #[test]
    fn object_transform_delete_and_typed_clone_survive_reimport() {
        let fixture = StageFixture::new("object-edits");
        let mut document = fixture.document();
        document.objects[0].transform.translation = [10.0, 20.0, 30.0];
        document.objects.remove(1);
        let mut clone = document.objects[0].clone();
        clone.id = "clone".to_string();
        clone.placement = Some(PlacementBinding::CloneOf(address(&[0])));
        clone.transform.translation = [40.0, 50.0, 60.0];
        document.objects.push(clone);

        let rebuilt = document.build_stage_archive().unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(reopened.encode().unwrap(), rebuilt);
        let placements = reopened.object_placements();
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].name, "first");
        assert_eq!(placements[0].transform.translation, [10.0, 20.0, 30.0]);
        assert_eq!(placements[1].name, "first");
        assert_eq!(placements[1].transform.translation, [40.0, 50.0, 60.0]);
    }

    #[test]
    fn typed_field_placement_transform_and_clone_survive_reimport() {
        let fixture = StageFixture::new("typed-field-placement");
        let mut document = fixture.document();
        let archive = document.stage_archive.as_mut().unwrap();
        let path = archive
            .insert_placement_record(b"scene.bin", &[], area_cylinder_record("area"))
            .unwrap();
        assert_eq!(path, [2]);

        let mut area = SceneObject::new("retail-area", "AreaCylinder");
        area.placement = Some(PlacementBinding::Existing(address(&path)));
        area.insert_source_raw_param("name", "area");
        area.transform = Transform {
            translation: [100.0, 200.0, 300.0],
            rotation_degrees: [10.0, 20.0, 30.0],
            scale: [40.0, 50.0, 60.0],
        };
        document.objects.push(area.clone());

        let mut clone = area;
        clone.id = "area-clone".to_string();
        clone.placement = Some(PlacementBinding::CloneOf(address(&path)));
        clone.transform.translation = [400.0, 500.0, 600.0];
        document.objects.push(clone);

        let rebuilt = document.build_stage_archive().unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(reopened.encode().unwrap(), rebuilt);
        let areas = reopened
            .object_placements()
            .into_iter()
            .filter(|placement| placement.type_name == "AreaCylinder")
            .collect::<Vec<_>>();
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].transform.translation, [100.0, 200.0, 300.0]);
        assert_eq!(areas[0].transform.rotation, [10.0, 20.0, 30.0]);
        assert_eq!(areas[0].transform.scale, [40.0, 50.0, 60.0]);
        assert_eq!(areas[1].transform.translation, [400.0, 500.0, 600.0]);
    }

    #[test]
    fn authored_record_insert_and_transform_survive_reparse() {
        let mut archive = authoring_strategy_archive(Vec::new());
        let dependency = fixture_manager_dependency(1);
        let mut object = authored_test_object(
            "authored-enemy",
            live_actor_record("authored enemy", "fixture manager"),
            7,
            vec![dependency],
        );
        object.transform = Transform {
            translation: [100.0, 200.0, 300.0],
            rotation_degrees: [10.0, 20.0, 30.0],
            scale: [2.0, 3.0, 4.0],
        };

        reconcile_scene_objects(&mut archive, &[object], &[], None).unwrap();
        let rebuilt = archive.encode().unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(reopened.encode().unwrap(), rebuilt);

        let paths =
            named_record_paths(&reopened, WORLD_SCENE_PATH, "LiveActor", "authored enemy").unwrap();
        assert_eq!(paths.len(), 1);
        let record = placement_record(
            &reopened,
            &PlacementAddress {
                raw_resource_path: WORLD_SCENE_PATH.to_vec(),
                record_path: paths[0].clone(),
            },
        )
        .unwrap();
        let JDramaRecordPayload::Actor {
            transform, fields, ..
        } = &record.payload
        else {
            panic!("authored enemy was not an actor");
        };
        assert_eq!(transform.translation, [100.0, 200.0, 300.0]);
        assert_eq!(transform.rotation, [10.0, 20.0, 30.0]);
        assert_eq!(transform.scale, [2.0, 3.0, 4.0]);
        assert_eq!(
            typed_field_value(fields, "manager_name"),
            &JDramaFieldValue::String("fixture manager".to_string())
        );
    }

    #[test]
    fn authored_dependency_is_inserted_into_retail_conductor_container() {
        let mut archive = authoring_conductor_archive(Vec::new(), Vec::new());
        let mut dependency = fixture_manager_dependency(1);
        dependency.target = Some(AuthoredPlacementDependencyTarget::NamedGroup {
            type_name: "GroupObj".to_string(),
            name: "conductor initialization".to_string(),
        });
        let object = authored_test_object(
            "authored-enemy",
            live_actor_record("authored enemy", "fixture manager"),
            7,
            vec![dependency],
        );

        reconcile_scene_objects(&mut archive, &[object], &[], None).unwrap();

        let managers = named_record_paths(
            &archive,
            WORLD_SCENE_PATH,
            "FixtureManager",
            "fixture manager",
        )
        .unwrap();
        assert_eq!(managers, vec![vec![0, 0]]);
    }

    #[test]
    fn authored_dependency_rejects_match_outside_retail_conductor_container() {
        let mut dependency = fixture_manager_dependency(1);
        let misplaced = dependency.record.clone();
        dependency.target = Some(AuthoredPlacementDependencyTarget::NamedGroup {
            type_name: "GroupObj".to_string(),
            name: "conductor initialization".to_string(),
        });
        let mut archive = authoring_conductor_archive(Vec::new(), vec![misplaced]);
        let object = authored_test_object(
            "authored-enemy",
            live_actor_record("authored enemy", "fixture manager"),
            7,
            vec![dependency],
        );

        let error = reconcile_scene_objects(&mut archive, &[object], &[], None).unwrap_err();
        assert!(
            error.to_string().contains(
                "exists outside container type 'GroupObj' named \"conductor initialization\""
            ),
            "{error}"
        );
    }

    #[test]
    fn authored_dependencies_are_deduplicated_across_actors() {
        let mut archive = authoring_strategy_archive(Vec::new());
        let dependency = fixture_manager_dependency(1);
        let first = authored_test_object(
            "first-enemy",
            live_actor_record("first enemy", "fixture manager"),
            7,
            vec![dependency.clone()],
        );
        let second = authored_test_object(
            "second-enemy",
            live_actor_record("second enemy", "fixture manager"),
            7,
            vec![dependency],
        );

        reconcile_scene_objects(&mut archive, &[first, second], &[], None).unwrap();

        let managers = named_record_paths(
            &archive,
            WORLD_SCENE_PATH,
            "FixtureManager",
            "fixture manager",
        )
        .unwrap();
        assert_eq!(managers.len(), 1);
        let actors = archive
            .object_placements()
            .into_iter()
            .filter(|placement| placement.type_name == "LiveActor")
            .count();
        assert_eq!(actors, 2);
    }

    #[test]
    fn authored_dependency_rejects_same_type_and_name_with_incompatible_payload() {
        let mut archive = authoring_strategy_archive(Vec::new());
        let first_dependency = fixture_manager_dependency(1);
        let mut incompatible_dependency = fixture_manager_dependency(1);
        let JDramaRecordPayload::Fields { fields } = &mut incompatible_dependency.record.payload
        else {
            unreachable!()
        };
        let JDramaFieldValue::U32(load_value) = &mut fields[2].value else {
            unreachable!()
        };
        *load_value = 99;
        let first = authored_test_object(
            "first-enemy",
            live_actor_record("first enemy", "fixture manager"),
            7,
            vec![first_dependency],
        );
        let second = authored_test_object(
            "second-enemy",
            live_actor_record("second enemy", "fixture manager"),
            7,
            vec![incompatible_dependency],
        );

        let error = reconcile_scene_objects(&mut archive, &[first, second], &[], None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("incompatible payload"), "{error}");
        assert!(error.contains("FixtureManager"), "{error}");
    }

    #[test]
    fn authored_dependency_rejects_different_type_conflict_alongside_exact_match() {
        let mut archive = authoring_strategy_archive(Vec::new());
        let dependency = fixture_manager_dependency(1);
        archive
            .insert_placement_record(WORLD_SCENE_PATH, &[0], dependency.record.clone())
            .unwrap();
        archive
            .insert_placement_record(
                WORLD_SCENE_PATH,
                &[0],
                JDramaRecord::new(
                    "OtherManager",
                    "fixture manager",
                    JDramaRecordPayload::Fields { fields: Vec::new() },
                )
                .unwrap(),
            )
            .unwrap();
        let object = authored_test_object(
            "authored-enemy",
            live_actor_record("authored enemy", "fixture manager"),
            7,
            vec![dependency],
        );

        let error = reconcile_scene_objects(&mut archive, &[object], &[], None)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("already used by semantic type 'OtherManager'"),
            "{error}"
        );
        assert!(error.contains("1 conflicting record(s)"), "{error}");
    }

    #[test]
    fn authored_dependency_capacity_merges_to_max_before_reference_floor() {
        let mut archive = authoring_strategy_archive(Vec::new());
        let objects = [
            ("first-enemy", "first enemy", 3),
            ("second-enemy", "second enemy", 7),
            ("third-enemy", "third enemy", 5),
        ]
        .into_iter()
        .map(|(id, name, capacity)| {
            authored_test_object(
                id,
                live_actor_record(name, "fixture manager"),
                7,
                vec![fixture_manager_dependency(capacity)],
            )
        })
        .collect::<Vec<_>>();

        reconcile_scene_objects(&mut archive, &objects, &[], None).unwrap();

        let manager_path = named_record_paths(
            &archive,
            WORLD_SCENE_PATH,
            "FixtureManager",
            "fixture manager",
        )
        .unwrap()
        .pop()
        .unwrap();
        let manager = placement_record(
            &archive,
            &PlacementAddress {
                raw_resource_path: WORLD_SCENE_PATH.to_vec(),
                record_path: manager_path,
            },
        )
        .unwrap();
        let JDramaRecordPayload::Fields { fields } = &manager.payload else {
            panic!("fixture manager did not retain typed fields");
        };
        assert_eq!(
            typed_field_value(fields, "capacity"),
            &JDramaFieldValue::U32(7)
        );
    }

    #[test]
    fn dependency_manager_capacity_is_floored_for_authored_map_objects() {
        let mut archive = authoring_strategy_archive(Vec::new());
        let dependency = fixture_manager_dependency(1);
        let first = authored_test_object(
            "first-map-object",
            map_static_record("first map object", "fixture resource"),
            3,
            vec![dependency.clone()],
        );
        let second = authored_test_object(
            "second-map-object",
            map_static_record("second map object", "fixture resource"),
            3,
            vec![dependency],
        );

        reconcile_scene_objects(&mut archive, &[first, second], &[], None).unwrap();

        let manager_path = named_record_paths(
            &archive,
            WORLD_SCENE_PATH,
            "FixtureManager",
            "fixture manager",
        )
        .unwrap()
        .pop()
        .unwrap();
        let manager = placement_record(
            &archive,
            &PlacementAddress {
                raw_resource_path: WORLD_SCENE_PATH.to_vec(),
                record_path: manager_path,
            },
        )
        .unwrap();
        let JDramaRecordPayload::Fields { fields } = &manager.payload else {
            panic!("fixture manager did not retain typed fields");
        };
        assert_eq!(
            typed_field_value(fields, "capacity"),
            &JDramaFieldValue::U32(2)
        );
    }

    #[test]
    fn cloned_map_object_uses_registry_manager_relation_for_capacity_floor() {
        let prototype = map_static_record("retail map object", "fixture resource");
        let mut archive = authoring_strategy_archive(vec![prototype.clone()]);
        archive
            .insert_placement_record(WORLD_SCENE_PATH, &[0], fixture_manager_dependency(1).record)
            .unwrap();
        let mut objects = existing_and_clone_objects(&prototype);
        let mut registry = ObjectRegistry::default();
        registry
            .map_obj_resources
            .push(sms_schema::MapObjResourceDefinition {
                resource_name: "fixture resource".to_string(),
                actor_type: 0,
                object_flags: 0,
                required_manager_name: "fixture manager".to_string(),
                has_hold_dependency: false,
                has_move_dependency: false,
                uses_resource_name_model_fallback: false,
                primary_model: None,
                animation_resources: Vec::new(),
                hold_model_path: None,
                move_bck_path: None,
                load_flags: 0x1022_0000,
                collision_resources: Vec::new(),
                source_file: "fixture".to_string(),
            });

        crate::refresh_object_manager_capacity_dependencies(&mut objects, &registry);
        assert_eq!(
            objects[0].manager_capacity_dependencies,
            ["fixture manager"]
        );
        let persisted: Vec<SceneObject> =
            serde_json::from_slice(&serde_json::to_vec(&objects).unwrap()).unwrap();

        reconcile_scene_objects(&mut archive, &persisted, &[], None).unwrap();

        assert_eq!(fixture_manager_capacity(&archive), 2);
        assert_eq!(
            named_record_paths(
                &archive,
                WORLD_SCENE_PATH,
                "MapStaticObj",
                "retail map object",
            )
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn cloned_launcher_counts_its_implicit_launched_enemy_manager() {
        let prototype = common_launcher_record("retail launcher", "fixture manager");
        let mut archive = authoring_strategy_archive(vec![prototype.clone()]);
        archive
            .insert_placement_record(WORLD_SCENE_PATH, &[0], fixture_manager_dependency(1).record)
            .unwrap();
        let objects = existing_and_clone_objects(&prototype);

        reconcile_scene_objects(&mut archive, &objects, &[], None).unwrap();

        assert_eq!(fixture_manager_capacity(&archive), 2);
        assert_eq!(
            named_record_paths(
                &archive,
                WORLD_SCENE_PATH,
                "CommonLauncher",
                "retail launcher",
            )
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn persisted_implicit_manager_dependency_rejects_missing_manager() {
        let prototype = map_static_record("retail map object", "fixture resource");
        let mut archive = authoring_strategy_archive(vec![prototype.clone()]);
        let mut objects = existing_and_clone_objects(&prototype);
        for object in &mut objects {
            object.manager_capacity_dependencies = vec!["fixture manager".to_string()];
        }

        let error = reconcile_scene_objects(&mut archive, &objects, &[], None)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("manager named \"fixture manager\""),
            "{error}"
        );
        assert!(error.contains("was not found"), "{error}");
    }

    #[test]
    fn persisted_implicit_manager_dependency_rejects_ambiguous_manager() {
        let prototype = map_static_record("retail map object", "fixture resource");
        let mut archive = authoring_strategy_archive(vec![prototype.clone()]);
        let manager = fixture_manager_dependency(1).record;
        archive
            .insert_placement_record(WORLD_SCENE_PATH, &[0], manager.clone())
            .unwrap();
        archive
            .insert_placement_record(WORLD_SCENE_PATH, &[0], manager)
            .unwrap();
        let mut objects = existing_and_clone_objects(&prototype);
        for object in &mut objects {
            object.manager_capacity_dependencies = vec!["fixture manager".to_string()];
        }

        let error = reconcile_scene_objects(&mut archive, &objects, &[], None)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("manager named \"fixture manager\""),
            "{error}"
        );
        assert!(error.contains("is ambiguous"), "{error}");
        assert!(error.contains("2 matches"), "{error}");
    }

    #[test]
    fn persisted_existing_and_clone_typed_field_edits_are_exported() {
        let prototype = map_change_stage_record("base object", "base resource", 1);
        let mut archive = authoring_strategy_archive(vec![prototype.clone()]);
        let source_address = PlacementAddress {
            raw_resource_path: WORLD_SCENE_PATH.to_vec(),
            record_path: vec![1, 0],
        };

        let mut existing = SceneObject::new("existing", "MapObjChangeStage");
        existing.placement = Some(PlacementBinding::Existing(source_address.clone()));
        existing.transform = Transform::default();
        crate::seed_scene_object_parameters(&mut existing, &prototype).unwrap();
        existing.set_raw_param("stage_id", "17");

        let mut clone = existing.clone();
        clone.id = "clone".to_string();
        clone.placement = Some(PlacementBinding::CloneOf(source_address));
        clone.set_raw_param("stage_id", "29");

        let persisted: Vec<SceneObject> =
            serde_json::from_slice(&serde_json::to_vec(&vec![existing, clone]).unwrap()).unwrap();
        assert!(!persisted[0].raw_params["stage_id"].is_dirty());
        assert!(!persisted[1].raw_params["stage_id"].is_dirty());

        reconcile_scene_objects(&mut archive, &persisted, &[], None).unwrap();
        let rebuilt = archive.encode().unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();

        let paths = named_record_paths(
            &reopened,
            WORLD_SCENE_PATH,
            "MapObjChangeStage",
            "base object",
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        let mut stage_ids = paths
            .into_iter()
            .map(|record_path| {
                let record = placement_record(
                    &reopened,
                    &PlacementAddress {
                        raw_resource_path: WORLD_SCENE_PATH.to_vec(),
                        record_path,
                    },
                )
                .unwrap();
                let JDramaRecordPayload::Actor { fields, .. } = &record.payload else {
                    panic!("edited map object was not an actor");
                };
                let JDramaFieldValue::U32(value) = typed_field_value(fields, "stage_id") else {
                    panic!("stage_id field was not a u32");
                };
                *value
            })
            .collect::<Vec<_>>();
        stage_ids.sort_unstable();
        assert_eq!(stage_ids, [17, 29]);
    }

    #[test]
    fn genuinely_missing_placement_address_is_still_rejected() {
        let fixture = StageFixture::new("missing-placement-address");
        let mut document = fixture.document();
        document.objects[0].placement = Some(PlacementBinding::Existing(address(&[99])));

        let error = document.build_stage_archive().unwrap_err().to_string();
        assert!(
            error.contains("references missing placement scene.bin:99"),
            "{error}"
        );
    }

    #[test]
    fn source_less_and_dirty_unknown_objects_are_rejected() {
        let fixture = StageFixture::new("reject-untyped");
        let mut source_less = fixture.document();
        source_less
            .objects
            .push(SceneObject::new("spawned", "MapStaticObj"));
        let error = source_less.build_stage_archive().unwrap_err().to_string();
        assert!(
            error.contains("no typed JDrama placement constructor"),
            "{error}"
        );

        let mut dirty = fixture.document();
        dirty.objects[0].set_raw_param("resource_name", "edited");
        let error = dirty.build_stage_archive().unwrap_err().to_string();
        assert!(
            error.contains("dirty unknown or synthetic parameter"),
            "{error}"
        );
    }

    #[test]
    fn typed_model_and_collision_edits_reach_the_rebuilt_archive() {
        let fixture = StageFixture::new("resource-edits");
        let document = fixture.document();
        let source = fs::read(&fixture.archive_path).unwrap();
        let source_archive = SourceFreeStageArchive::parse(&source).unwrap();
        let mut model = match source_archive.resource(b"map.bmd").unwrap() {
            StageResourceDocument::Model(model) => model.clone(),
            _ => panic!("fixture model has wrong kind"),
        };
        model.reserved_words[0] = 0x1234_5678;
        let mut collision = match source_archive.resource(b"map.col").unwrap() {
            StageResourceDocument::Collision(collision) => collision.clone(),
            _ => panic!("fixture collision has wrong kind"),
        };
        collision.vertices_mut()[0].position[1] = 75.0;
        let mut edits = StageArchiveEdits::default();
        edits.replace_model(b"map.bmd".to_vec(), model);
        edits.replace_collision(b"map.col".to_vec(), collision);

        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        let StageResourceDocument::Model(model) = reopened.resource(b"map.bmd").unwrap() else {
            panic!("rebuilt model has wrong kind");
        };
        assert_eq!(model.reserved_words[0], 0x1234_5678);
        let StageResourceDocument::Collision(collision) = reopened.resource(b"map.col").unwrap()
        else {
            panic!("rebuilt collision has wrong kind");
        };
        assert_eq!(collision.vertices()[0].position[1], 75.0);
    }

    #[test]
    fn resource_removal_is_idempotent_and_superseded_by_upsert() {
        let fixture = StageFixture::new("resource-removal");
        let document = fixture.document();
        let mut edits = StageArchiveEdits::default();
        edits.remove_resource(b"map.bmd".to_vec());
        edits.remove_resource(b"missing.bmd".to_vec());

        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert!(reopened.resource(b"map.bmd").is_none());

        let source = fs::read(&fixture.archive_path).unwrap();
        let source_archive = SourceFreeStageArchive::parse(&source).unwrap();
        let replacement = match source_archive.resource(b"map.bmd").unwrap() {
            StageResourceDocument::Model(model) => model.clone(),
            _ => panic!("fixture model has wrong kind"),
        };
        edits.upsert_model(b"map.bmd".to_vec(), replacement.clone());
        assert!(!edits
            .resource_removals
            .iter()
            .any(|path| path == b"map.bmd"));
        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            reopened.resource(b"map.bmd"),
            Some(&StageResourceDocument::Model(replacement))
        );
    }

    #[test]
    fn resource_and_typed_upserts_insert_missing_archive_children() {
        let fixture = StageFixture::new("resource-upserts");
        let document = fixture.document();
        let source = fs::read(&fixture.archive_path).unwrap();
        let source_archive = SourceFreeStageArchive::parse(&source).unwrap();
        let mut model = match source_archive.resource(b"map.bmd").unwrap() {
            StageResourceDocument::Model(model) => model.clone(),
            _ => panic!("fixture model has wrong kind"),
        };
        model.reserved_words[0] = 0xAABB_CCDD;
        let collision = authored_collision(0x4100, 20.0, 3);
        let parameters = PrmFile {
            entries: vec![PrmEntry {
                name: "mSize".to_string(),
                value: PrmValue::from_f32(2.5),
            }],
        };
        let mut edits = StageArchiveEdits::default();
        edits.insert_resource(
            b"mapobj/authored.prm".to_vec(),
            StageResourceDocument::Parameters(parameters.clone()),
        );
        edits.upsert_model(b"mapobj/authored.bmd".to_vec(), model.clone());
        edits.upsert_collision(b"mapobj/authored.col".to_vec(), collision.clone());

        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            reopened.resource(b"mapobj/authored.prm"),
            Some(&StageResourceDocument::Parameters(parameters))
        );
        assert_eq!(
            reopened.resource(b"mapobj/authored.bmd"),
            Some(&StageResourceDocument::Model(model))
        );
        assert_eq!(
            reopened.resource(b"mapobj/authored.col"),
            Some(&StageResourceDocument::Collision(collision))
        );
        assert_eq!(reopened.encode().unwrap(), rebuilt);
    }

    #[test]
    fn general_insert_rejects_existing_paths_while_explicit_upsert_replaces() {
        let fixture = StageFixture::new("general-resource-modes");
        let document = fixture.document();
        let collision = authored_collision(0x4100, 12.0, 4);
        let mut insert = StageArchiveEdits::default();
        insert.insert_resource(
            b"map.col".to_vec(),
            StageResourceDocument::Collision(collision.clone()),
        );
        let error = document
            .build_stage_archive_with_edits(&insert)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");

        let mut upsert = StageArchiveEdits::default();
        upsert.upsert_resource(
            b"map.col".to_vec(),
            StageResourceDocument::Collision(collision.clone()),
        );
        let rebuilt = document.build_stage_archive_with_edits(&upsert).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            reopened.resource(b"map.col"),
            Some(&StageResourceDocument::Collision(collision))
        );
    }

    #[test]
    fn replace_model_and_collision_remain_replacement_only() {
        let fixture = StageFixture::new("replacement-only");
        let document = fixture.document();
        let source = fs::read(&fixture.archive_path).unwrap();
        let source_archive = SourceFreeStageArchive::parse(&source).unwrap();
        let model = match source_archive.resource(b"map.bmd").unwrap() {
            StageResourceDocument::Model(model) => model.clone(),
            _ => panic!("fixture model has wrong kind"),
        };
        let mut model_edits = StageArchiveEdits::default();
        model_edits.replace_model(b"missing.bmd".to_vec(), model);
        let error = document
            .build_stage_archive_with_edits(&model_edits)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("model resource missing.bmd was not found"),
            "{error}"
        );

        let mut collision_edits = StageArchiveEdits::default();
        collision_edits.replace_collision(b"missing.col".to_vec(), authored_collision(0, 0.0, 0));
        let error = document
            .build_stage_archive_with_edits(&collision_edits)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("collision resource missing.col was not found"),
            "{error}"
        );
    }

    #[test]
    fn collision_appends_preserve_retail_groups_and_remap_each_authored_index() {
        let fixture = StageFixture::new("collision-appends");
        let document = fixture.document();
        let source = fs::read(&fixture.archive_path).unwrap();
        let source_archive = SourceFreeStageArchive::parse(&source).unwrap();
        let original = match source_archive.resource(b"map.col").unwrap() {
            StageResourceDocument::Collision(collision) => collision.clone(),
            _ => panic!("fixture collision has wrong kind"),
        };
        let first = authored_collision(0x4100, 20.0, 3);
        let second = authored_collision(0x4200, 40.0, 7);
        let mut edits = StageArchiveEdits::default();
        edits.append_collision(b"map.col".to_vec(), first.clone());
        edits.append_collision(b"map.col".to_vec(), second.clone());

        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        let collision = match reopened.resource(b"map.col").unwrap() {
            StageResourceDocument::Collision(collision) => collision,
            _ => panic!("rebuilt collision has wrong kind"),
        };
        assert_eq!(
            &collision.vertices()[..original.vertices().len()],
            original.vertices()
        );
        assert_eq!(
            &collision.groups()[..original.groups().len()],
            original.groups()
        );
        assert_eq!(collision.vertices().len(), 9);
        assert_eq!(collision.groups().len(), 3);
        assert_eq!(
            collision.groups()[1].surface_type,
            first.groups()[0].surface_type
        );
        assert_eq!(collision.groups()[1].triangles[0].vertex_indices, [3, 4, 5]);
        assert_eq!(
            collision.groups()[2].surface_type,
            second.groups()[0].surface_type
        );
        assert_eq!(collision.groups()[2].triangles[0].vertex_indices, [6, 7, 8]);
    }

    #[test]
    fn world_collision_append_preserves_duck_like_runtime_headroom() {
        let fixture = StageFixture::new("world-collision-capacities");
        let mut document = fixture.document();
        install_world_collision_resources(&mut document);
        let authored = duck_like_collision();
        let config = MapCollisionRuntimeConfig {
            grid_width: 60,
            grid_height: 60,
            triangle_capacity: 12_000,
            list_capacity: 30_000,
        };
        assert_eq!(authored_collision_triangle_count(&authored).unwrap(), 4_212);
        assert_eq!(
            authored_collision_grid_link_count(&authored, config).unwrap(),
            4_668
        );

        let mut edits = StageArchiveEdits::default();
        edits.append_collision(WORLD_COLLISION_PATH.to_vec(), authored);
        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            world_map_collision_fields(&reopened),
            [60, 60, 16_212, 34_668, 3_000]
        );
        let StageResourceDocument::Collision(collision) =
            reopened.resource(WORLD_COLLISION_PATH).unwrap()
        else {
            panic!("rebuilt world collision has wrong kind");
        };
        assert_eq!(
            collision
                .groups()
                .iter()
                .map(|group| group.triangles.len())
                .sum::<usize>(),
            4_213
        );
        assert_eq!(reopened.encode().unwrap(), rebuilt);
    }

    #[test]
    fn collision_grid_links_apply_wall_padding_to_bounds_and_cells() {
        let authored = collision_with_triangle(
            0,
            [
                [1_100.0, 0.0, 100.0],
                [1_100.0, 100.0, 100.0],
                [1_100.0, 0.0, 200.0],
            ],
        );
        let config = MapCollisionRuntimeConfig {
            grid_width: 4,
            grid_height: 4,
            triangle_capacity: 0,
            list_capacity: 0,
        };
        let points = collision_triangle_points(&authored, [0, 1, 2], 0, 0).unwrap();

        assert_eq!(
            collision_plane_type(authored.groups()[0].surface_type, points.normal_y),
            CollisionPlaneType::Wall
        );
        assert_eq!(
            collision_grid_bounds(
                points.points,
                CollisionPlaneType::Wall,
                2_048.0,
                2_048.0,
                4,
                4,
                0,
                0,
            )
            .unwrap(),
            Some([2, 2, 3, 2])
        );
        assert!(!polygon_is_in_grid(0.0, 0.0, 1_024.0, 1_024.0, points));
        assert!(polygon_is_in_grid(
            -COLLISION_WALL_PADDING,
            -COLLISION_WALL_PADDING,
            1_024.0 + COLLISION_WALL_PADDING,
            1_024.0 + COLLISION_WALL_PADDING,
            points,
        ));
        assert_eq!(
            authored_collision_grid_link_count(&authored, config).unwrap(),
            2
        );
    }

    #[test]
    fn collision_grid_links_negative_normal_roofs_across_the_full_bbox() {
        let authored = collision_with_triangle(
            0,
            [
                [-500.0, 0.0, -500.0],
                [500.0, 0.0, -500.0],
                [-500.0, 0.0, 500.0],
            ],
        );
        let config = MapCollisionRuntimeConfig {
            grid_width: 4,
            grid_height: 4,
            triangle_capacity: 0,
            list_capacity: 0,
        };
        let points = collision_triangle_points(&authored, [0, 1, 2], 0, 0).unwrap();

        assert!(points.normal_y < 0.0);
        assert_eq!(
            collision_plane_type(authored.groups()[0].surface_type, points.normal_y),
            CollisionPlaneType::Roof
        );
        assert_eq!(
            collision_grid_bounds(
                points.points,
                CollisionPlaneType::Roof,
                2_048.0,
                2_048.0,
                4,
                4,
                0,
                0,
            )
            .unwrap(),
            Some([1, 1, 2, 2])
        );
        assert!(polygon_is_in_grid(0.0, 0.0, 1_024.0, 1_024.0, points));
        assert_eq!(
            authored_collision_grid_link_count(&authored, config).unwrap(),
            4
        );
    }

    #[test]
    fn collision_surface_0801_overrides_wall_classification_to_ground() {
        let points = [
            [1_100.0, 0.0, 100.0],
            [1_100.0, 100.0, 100.0],
            [1_100.0, 0.0, 200.0],
        ];
        let wall = collision_with_triangle(0, points);
        let forced_ground = collision_with_triangle(0x0801, points);
        let config = MapCollisionRuntimeConfig {
            grid_width: 4,
            grid_height: 4,
            triangle_capacity: 0,
            list_capacity: 0,
        };
        let triangle = collision_triangle_points(&forced_ground, [0, 1, 2], 0, 0).unwrap();

        assert_eq!(triangle.normal_y, 0.0);
        assert_eq!(
            collision_plane_type(0x0801, triangle.normal_y),
            CollisionPlaneType::Ground
        );
        assert_eq!(
            authored_collision_grid_link_count(&wall, config).unwrap(),
            2
        );
        assert_eq!(
            authored_collision_grid_link_count(&forced_ground, config).unwrap(),
            1
        );
    }

    #[test]
    fn collision_grid_bounds_clip_partial_triangles_and_reject_outside_ones() {
        let partial = collision_with_triangle(
            0,
            [
                [1_900.0, 0.0, 100.0],
                [1_900.0, 0.0, 300.0],
                [2_300.0, 0.0, 100.0],
            ],
        );
        let outside = collision_with_triangle(
            0,
            [
                [2_050.0, 0.0, 100.0],
                [2_050.0, 0.0, 300.0],
                [2_300.0, 0.0, 100.0],
            ],
        );
        let config = MapCollisionRuntimeConfig {
            grid_width: 4,
            grid_height: 4,
            triangle_capacity: 0,
            list_capacity: 0,
        };
        let partial_points = collision_triangle_points(&partial, [0, 1, 2], 0, 0).unwrap();
        let outside_points = collision_triangle_points(&outside, [0, 1, 2], 0, 0).unwrap();

        assert_eq!(
            collision_grid_bounds(
                partial_points.points,
                CollisionPlaneType::Ground,
                2_048.0,
                2_048.0,
                4,
                4,
                0,
                0,
            )
            .unwrap(),
            Some([3, 2, 3, 2])
        );
        assert_eq!(
            collision_grid_bounds(
                outside_points.points,
                CollisionPlaneType::Ground,
                2_048.0,
                2_048.0,
                4,
                4,
                0,
                0,
            )
            .unwrap(),
            None
        );
        assert_eq!(
            authored_collision_grid_link_count(&partial, config).unwrap(),
            1
        );
        assert_eq!(
            authored_collision_grid_link_count(&outside, config).unwrap(),
            0
        );
    }

    #[test]
    fn collision_grid_links_match_odd_runtime_extents_and_truncation() {
        let boundary = collision_with_triangle(
            0,
            [
                [2_048.0, 0.0, 0.0],
                [2_048.0, 0.0, 10.0],
                [2_050.0, 0.0, 0.0],
            ],
        );
        let outside = collision_with_triangle(
            0,
            [
                [2_048.25, 0.0, 0.0],
                [2_048.25, 0.0, 10.0],
                [2_050.0, 0.0, 0.0],
            ],
        );
        let config = MapCollisionRuntimeConfig {
            grid_width: 5,
            grid_height: 3,
            triangle_capacity: 0,
            list_capacity: 0,
        };
        let boundary_points = collision_triangle_points(&boundary, [0, 1, 2], 0, 0).unwrap();

        assert_eq!(
            collision_grid_bounds(
                boundary_points.points,
                CollisionPlaneType::Ground,
                2_048.0,
                1_024.0,
                5,
                3,
                0,
                0,
            )
            .unwrap(),
            Some([4, 1, 4, 1])
        );
        assert_eq!(checked_trunc_grid_index(-0.75, 0, 0).unwrap(), 0);
        assert_eq!(checked_trunc_grid_index(-1.75, 0, 0).unwrap(), -1);
        assert_eq!(
            authored_collision_grid_link_count(&boundary, config).unwrap(),
            1
        );
        assert_eq!(
            authored_collision_grid_link_count(&outside, config).unwrap(),
            0
        );
    }

    #[test]
    fn stale_and_non_world_collision_appends_do_not_change_map_capacities() {
        let fixture = StageFixture::new("non-world-collision-capacities");
        let mut document = fixture.document();
        install_world_collision_resources(&mut document);
        document
            .stage_archive
            .as_mut()
            .unwrap()
            .insert_resource(
                b"mapobj/authored.col".to_vec(),
                StageResourceDocument::Collision(authored_collision(0, 200.0, 1)),
            )
            .unwrap();

        let mut edits = StageArchiveEdits::default();
        edits.append_collision(b"map.col".to_vec(), authored_collision(0, 300.0, 2));
        edits.append_collision(
            b"mapobj/authored.col".to_vec(),
            authored_collision(0, 400.0, 3),
        );
        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            world_map_collision_fields(&reopened),
            [60, 60, 12_000, 30_000, 3_000]
        );
    }

    #[test]
    fn world_collision_capacity_target_rejects_missing_ambiguous_and_wrong_fields() {
        let authored = authored_collision(0, 100.0, 0);

        let mut missing = world_collision_archive("missing-map-field");
        world_map_fields_mut(&mut missing)
            .retain(|field| field.name != COLLISION_LIST_CAPACITY_FIELD);
        let error = preserve_world_collision_runtime_headroom(&mut missing, &authored)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("missing field 'collision_list_capacity'"),
            "{error}"
        );

        let mut ambiguous = world_collision_archive("ambiguous-map-record");
        let placement = match ambiguous.resource_mut(WORLD_SCENE_PATH).unwrap() {
            StageResourceDocument::Placement(document) => document,
            _ => panic!("world scene has wrong kind"),
        };
        let JDramaRecordPayload::Group { children, .. } = &mut placement.root.payload else {
            panic!("world scene root is not a group");
        };
        children.push(children[0].clone());
        let error = preserve_world_collision_runtime_headroom(&mut ambiguous, &authored)
            .unwrap_err()
            .to_string();
        assert!(error.contains("capacity target is ambiguous"), "{error}");

        let mut wrong_type = world_collision_archive("wrong-map-field-type");
        let fields = world_map_fields_mut(&mut wrong_type);
        let field = fields
            .iter_mut()
            .find(|field| field.name == COLLISION_TRIANGLE_CAPACITY_FIELD)
            .unwrap();
        field.value = JDramaFieldValue::U32(12_000);
        let error = preserve_world_collision_runtime_headroom(&mut wrong_type, &authored)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("collision_triangle_capacity") && error.contains("not typed i32"),
            "{error}"
        );
    }

    #[test]
    fn multiple_world_collision_appends_accumulate_capacity_deltas() {
        let fixture = StageFixture::new("multiple-world-collision-appends");
        let mut document = fixture.document();
        install_world_collision_resources(&mut document);
        let mut edits = StageArchiveEdits::default();
        edits.append_collision(
            WORLD_COLLISION_PATH.to_vec(),
            authored_collision(0, 100.0, 1),
        );
        edits.append_collision(
            WORLD_COLLISION_PATH.to_vec(),
            authored_collision(0, 200.0, 2),
        );

        let rebuilt = document.build_stage_archive_with_edits(&edits).unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            world_map_collision_fields(&reopened),
            [60, 60, 12_002, 30_002, 3_000]
        );
    }

    #[test]
    fn world_collision_capacity_overflow_is_rejected_atomically() {
        let authored = authored_collision(0, 100.0, 0);

        let mut triangle_overflow = world_collision_archive("triangle-capacity-overflow");
        set_unique_i32_field(
            world_map_fields_mut(&mut triangle_overflow),
            COLLISION_TRIANGLE_CAPACITY_FIELD,
            i32::MAX,
        )
        .unwrap();
        let before = world_map_collision_fields(&triangle_overflow);
        let error = preserve_world_collision_runtime_headroom(&mut triangle_overflow, &authored)
            .unwrap_err()
            .to_string();
        assert!(error.contains("collision_triangle_capacity"), "{error}");
        assert_eq!(world_map_collision_fields(&triangle_overflow), before);

        let mut list_overflow = world_collision_archive("list-capacity-overflow");
        set_unique_i32_field(
            world_map_fields_mut(&mut list_overflow),
            COLLISION_LIST_CAPACITY_FIELD,
            i32::MAX,
        )
        .unwrap();
        let before = world_map_collision_fields(&list_overflow);
        let error = preserve_world_collision_runtime_headroom(&mut list_overflow, &authored)
            .unwrap_err()
            .to_string();
        assert!(error.contains("collision_list_capacity"), "{error}");
        assert_eq!(world_map_collision_fields(&list_overflow), before);
    }

    #[test]
    fn collision_append_rejects_retail_signed_index_overflow() {
        let existing = ColFile::new(
            vec![ColVertex::new(0.0, 0.0, 0.0); i16::MAX as usize],
            Vec::new(),
        );
        let authored = ColFile::new(
            vec![ColVertex::new(1.0, 0.0, 0.0), ColVertex::new(2.0, 0.0, 0.0)],
            vec![ColGroup {
                surface_type: 0,
                has_per_triangle_data: false,
                triangles: vec![ColTriangle {
                    vertex_indices: [0, 1, 1],
                    attribute_0: 0,
                    attribute_1: 0,
                    data: None,
                }],
            }],
        );
        let error = append_collision_document(&existing, &authored, b"map.col")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("index 32768 exceeds the retail COL signed-index limit 32767"),
            "{error}"
        );
    }

    #[test]
    fn collision_append_accepts_retail_signed_index_maximum() {
        let existing = ColFile::new(
            vec![ColVertex::new(0.0, 0.0, 0.0); i16::MAX as usize],
            Vec::new(),
        );
        let authored = ColFile::new(
            vec![ColVertex::new(1.0, 0.0, 0.0)],
            vec![ColGroup {
                surface_type: 0,
                has_per_triangle_data: false,
                triangles: vec![ColTriangle {
                    vertex_indices: [0, 0, 0],
                    attribute_0: 0,
                    attribute_1: 0,
                    data: None,
                }],
            }],
        );

        let merged = append_collision_document(&existing, &authored, b"map.col").unwrap();
        assert_eq!(
            merged.groups()[0].triangles[0].vertex_indices,
            [i16::MAX as u16; 3]
        );
    }

    #[test]
    fn fully_typed_placement_insert_survives_fresh_reimport() {
        let fixture = StageFixture::new("typed-placement-insert");
        let mut document = fixture.document();
        document.archive_edits.insert_placement(
            b"scene.bin".to_vec(),
            Vec::new(),
            actor_record("inserted", [70.0, 80.0, 90.0]),
        );

        let rebuilt = document.build_stage_archive().unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(reopened.encode().unwrap(), rebuilt);
        let placements = reopened.object_placements();
        assert_eq!(placements.len(), 3);
        assert_eq!(placements[2].type_name, "MapStaticObj");
        assert_eq!(placements[2].name, "inserted");
        assert_eq!(placements[2].transform.translation, [70.0, 80.0, 90.0]);
    }

    #[test]
    fn external_export_is_create_new_and_rejects_the_base_tree() {
        let fixture = StageFixture::new("external-output");
        let document = fixture.document();
        let output_root = unique_root("external-output-target");
        fs::create_dir_all(&output_root).unwrap();
        let output = output_root.join("test.arc");
        let outcome = document.export_stage_archive_new(&output).unwrap();
        assert_eq!(
            outcome.output_path,
            fs::canonicalize(&output_root).unwrap().join("test.arc")
        );
        assert!(!outcome.changed);
        assert_eq!(
            fs::read(&output).unwrap(),
            fs::read(&fixture.archive_path).unwrap()
        );
        assert!(document.export_stage_archive_new(&output).is_err());

        let inside_base = fixture.root.join("inside.arc");
        let error = document.export_stage_archive_new(&inside_base).unwrap_err();
        assert!(matches!(
            error,
            SceneError::StageArchiveOutputOverlapsBase(_)
        ));
        fs::remove_dir_all(output_root).unwrap();
    }

    #[test]
    fn opened_stage_exports_after_the_source_archive_is_overwritten() {
        let fixture = StageFixture::new("detached-after-open");
        let mut document = StageDocument::open(&fixture.root, "test").unwrap();
        assert!(document.stage_archive.is_some());
        assert_eq!(
            document.stage_archive_source_path.as_deref(),
            Some(fixture.archive_path.as_path())
        );
        document.objects = vec![
            fixture_object("first", &[0]),
            fixture_object("second", &[1]),
        ];
        let expected = document.stage_archive.as_ref().unwrap().encode().unwrap();

        fs::write(&fixture.archive_path, b"destroyed source archive").unwrap();
        let output_root = unique_root("detached-after-open-output");
        fs::create_dir_all(&output_root).unwrap();
        let output = output_root.join("test.arc");
        let outcome = document.export_stage_archive_new(&output).unwrap();

        assert!(!outcome.changed);
        assert_eq!(fs::read(output).unwrap(), expected);
        fs::remove_dir_all(output_root).unwrap();
    }

    #[test]
    fn project_round_trip_keeps_the_freshly_imported_semantic_archive() {
        let fixture = StageFixture::new("semantic-project-round-trip");
        let project_root = unique_root("semantic-project");
        let mut saved = fixture.document();
        saved.objects[0].transform.translation = [101.0, 202.0, 303.0];
        let expected_rebuild = saved.build_stage_archive().unwrap();
        saved.save_project_folder(&project_root).unwrap();

        let mut reopened = fixture.document();
        let fresh_archive = reopened.stage_archive.clone();
        assert!(reopened.load_project_folder(&project_root).unwrap());
        assert_eq!(reopened.stage_archive, fresh_archive);
        assert_eq!(
            reopened.objects[0].transform.translation,
            [101.0, 202.0, 303.0]
        );
        assert_eq!(reopened.build_stage_archive().unwrap(), expected_rebuild);

        fs::remove_dir_all(project_root).unwrap();
    }

    #[test]
    fn stage_only_export_rejects_uncompiled_dialogue_authoring() {
        let fixture = StageFixture::new("uncompiled-dialogue-export");
        let mut document = fixture.document();
        document
            .dialogue_library
            .common_overrides
            .push(crate::ProjectDialogueOverride {
                message: crate::DialogueMessageRef {
                    domain: crate::DialogueDomain::System,
                    raw_resource_path: crate::SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
                    full_message_id: 1,
                    entry_index: 1,
                },
                content: crate::DialogueContent {
                    message: BmgMessage::default(),
                    authored_tokens: None,
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            });

        let error = document.build_stage_archive().unwrap_err().to_string();
        assert!(error.contains("stage-only export"), "{error}");
        assert!(error.contains("common.szs"), "{error}");

        document
            .build_stage_archive_with_compiled_dialogue_edits(&document.archive_edits)
            .expect("managed caller can rebuild after compiling external dialogue outputs");
    }

    #[test]
    fn stage_only_export_rejects_allocation_only_dialogue_tombstones() {
        let fixture = StageFixture::new("uncompiled-dialogue-allocation-export");
        let mut document = fixture.document();
        let object_id = document.objects[0].id.clone();
        let key = crate::DialogueVariantKey::generated_for_object(&object_id);
        document.dialogue_authoring = Some(crate::DialogueAuthoringDocument::default());
        document
            .dialogue_authoring
            .as_mut()
            .unwrap()
            .objects
            .insert(
                object_id,
                crate::DialogueObjectAuthoring {
                    inherited_from_object_id: None,
                    prior_runtime_name: None,
                    overrides: Vec::new(),
                    stable_allocations: vec![crate::DialogueStableAllocation {
                        key,
                        message_index: 7,
                    }],
                },
            );

        let error = document.build_stage_archive().unwrap_err().to_string();
        assert!(error.contains("stage-only export"), "{error}");
        assert!(error.contains("DOL outputs remain atomic"), "{error}");
    }

    #[test]
    fn managed_export_accepts_only_an_owned_generated_dialogue_runtime_name() {
        let fixture = StageFixture::new("generated-dialogue-runtime-name");
        let mut document = fixture.document();
        let object_id = document.objects[0].id.clone();
        document
            .initialize_dialogue_for_new_object(&object_id)
            .unwrap();
        let generated_name = document.objects[0].raw_param("name").unwrap().to_string();
        assert!(document.owns_generated_dialogue_runtime_name(&object_id));

        let rebuilt = document
            .build_stage_archive_with_compiled_dialogue_edits(&document.archive_edits)
            .expect("dialogue-owned deterministic name may cross the managed export boundary");
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert!(reopened.object_placements().iter().any(|placement| {
            placement.raw_resource_path == b"scene.bin"
                && placement.record_path == [0]
                && placement.name == generated_name
        }));

        let mut spoofed = fixture.document();
        spoofed.objects[0].set_raw_param("name", "GraffitoDlg_0000000000000000");
        assert!(!spoofed.owns_generated_dialogue_runtime_name(&spoofed.objects[0].id));
        let error = spoofed
            .build_stage_archive_with_compiled_dialogue_edits(&spoofed.archive_edits)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("cannot change linked parameter 'name'"),
            "{error}"
        );
    }

    struct StageFixture {
        root: PathBuf,
        archive_path: PathBuf,
    }

    impl StageFixture {
        fn new(label: &str) -> Self {
            let root = unique_root(label);
            let scene_root = root.join("files/data/scene");
            fs::create_dir_all(&scene_root).unwrap();
            let archive_path = scene_root.join("test.arc");
            fs::write(&archive_path, fixture_archive()).unwrap();
            Self { root, archive_path }
        }

        fn document(&self) -> StageDocument {
            let source = fs::read(&self.archive_path).expect("read stage fixture archive");
            let stage_archive = SourceFreeStageArchive::parse(&source)
                .expect("import stage fixture archive semantically");
            StageDocument {
                stage_id: "test".to_string(),
                base_root: self.root.clone(),
                assets: Vec::new(),
                objects: vec![
                    fixture_object("first", &[0]),
                    fixture_object("second", &[1]),
                ],
                changed_files: BTreeMap::new(),
                stage_archive: Some(stage_archive),
                stage_archive_source_path: Some(self.archive_path.clone()),
                archive_edits: StageArchiveEdits::default(),
                registry: None,
                route_authoring: None,
                goop_authoring: None,
                dialogue_authoring: None,
                dialogue_library: Default::default(),
                load_issues: Vec::new(),
                lighting: crate::StageLighting::default(),
                actor_previews: BTreeMap::new(),
                loaded_project: None,
            }
        }
    }

    impl Drop for StageFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture_object(name: &str, path: &[usize]) -> SceneObject {
        let mut object = SceneObject::new(format!("retail-{name}"), "MapStaticObj");
        object.placement = Some(PlacementBinding::Existing(address(path)));
        object.transform = if path == [0] {
            Transform {
                translation: [1.0, 2.0, 3.0],
                rotation_degrees: [0.0, 90.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            }
        } else {
            Transform {
                translation: [4.0, 5.0, 6.0],
                rotation_degrees: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            }
        };
        object.insert_source_raw_param("name", name);
        object
    }

    fn address(path: &[usize]) -> PlacementAddress {
        PlacementAddress {
            raw_resource_path: b"scene.bin".to_vec(),
            record_path: path.to_vec(),
        }
    }

    fn install_world_collision_resources(document: &mut StageDocument) {
        let archive = document.stage_archive.as_mut().unwrap();
        archive
            .insert_resource(
                WORLD_SCENE_PATH.to_vec(),
                StageResourceDocument::Placement(world_map_scene_document()),
            )
            .unwrap();
        archive
            .insert_resource(
                WORLD_COLLISION_PATH.to_vec(),
                StageResourceDocument::Collision(authored_collision(0, 500.0, 0)),
            )
            .unwrap();
    }

    fn world_collision_archive(label: &str) -> SourceFreeStageArchive {
        let fixture = StageFixture::new(label);
        let mut document = fixture.document();
        install_world_collision_resources(&mut document);
        document.stage_archive.take().unwrap()
    }

    fn world_map_scene_document() -> JDramaDocument {
        JDramaDocument {
            root: JDramaRecord::new(
                "NameRefGrp",
                "root",
                JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: vec![JDramaRecord::new(
                        "Map",
                        "map",
                        JDramaRecordPayload::Fields {
                            fields: vec![
                                JDramaField {
                                    name: "translucent_group_count".to_string(),
                                    value: JDramaFieldValue::U32(0),
                                },
                                JDramaField {
                                    name: COLLISION_GRID_WIDTH_FIELD.to_string(),
                                    value: JDramaFieldValue::I32(60),
                                },
                                JDramaField {
                                    name: COLLISION_GRID_HEIGHT_FIELD.to_string(),
                                    value: JDramaFieldValue::I32(60),
                                },
                                JDramaField {
                                    name: COLLISION_TRIANGLE_CAPACITY_FIELD.to_string(),
                                    value: JDramaFieldValue::I32(12_000),
                                },
                                JDramaField {
                                    name: COLLISION_LIST_CAPACITY_FIELD.to_string(),
                                    value: JDramaFieldValue::I32(30_000),
                                },
                                JDramaField {
                                    name: COLLISION_WARP_CAPACITY_FIELD.to_string(),
                                    value: JDramaFieldValue::I32(3_000),
                                },
                                JDramaField {
                                    name: "warp_pair_count".to_string(),
                                    value: JDramaFieldValue::U32(0),
                                },
                            ],
                        },
                    )
                    .unwrap()],
                },
            )
            .unwrap(),
        }
    }

    fn world_map_fields_mut(archive: &mut SourceFreeStageArchive) -> &mut Vec<JDramaField> {
        let placement = match archive.resource_mut(WORLD_SCENE_PATH).unwrap() {
            StageResourceDocument::Placement(document) => document,
            _ => panic!("world scene has wrong kind"),
        };
        let JDramaRecordPayload::Group { children, .. } = &mut placement.root.payload else {
            panic!("world scene root is not a group");
        };
        let JDramaRecordPayload::Fields { fields } = &mut children[0].payload else {
            panic!("Map record does not have fields");
        };
        fields
    }

    fn world_map_collision_fields(archive: &SourceFreeStageArchive) -> [i32; 5] {
        let placement = match archive.resource(WORLD_SCENE_PATH).unwrap() {
            StageResourceDocument::Placement(document) => document,
            _ => panic!("world scene has wrong kind"),
        };
        let JDramaRecordPayload::Group { children, .. } = &placement.root.payload else {
            panic!("world scene root is not a group");
        };
        let JDramaRecordPayload::Fields { fields } = &children[0].payload else {
            panic!("Map record does not have fields");
        };
        [
            unique_i32_field(fields, COLLISION_GRID_WIDTH_FIELD).unwrap(),
            unique_i32_field(fields, COLLISION_GRID_HEIGHT_FIELD).unwrap(),
            unique_i32_field(fields, COLLISION_TRIANGLE_CAPACITY_FIELD).unwrap(),
            unique_i32_field(fields, COLLISION_LIST_CAPACITY_FIELD).unwrap(),
            unique_i32_field(fields, COLLISION_WARP_CAPACITY_FIELD).unwrap(),
        ]
    }

    fn duck_like_collision() -> ColFile {
        let mut triangles = Vec::with_capacity(4_212);
        triangles.extend((0..3_756).map(|_| ColTriangle {
            vertex_indices: [0, 1, 2],
            attribute_0: 0,
            attribute_1: 0,
            data: None,
        }));
        triangles.extend((0..456).map(|_| ColTriangle {
            vertex_indices: [3, 4, 5],
            attribute_0: 0,
            attribute_1: 0,
            data: None,
        }));
        ColFile::new(
            vec![
                ColVertex::new(100.0, 0.0, 100.0),
                ColVertex::new(100.0, 0.0, 110.0),
                ColVertex::new(110.0, 0.0, 100.0),
                ColVertex::new(-10.0, 0.0, 100.0),
                ColVertex::new(-10.0, 0.0, 110.0),
                ColVertex::new(10.0, 0.0, 100.0),
            ],
            vec![ColGroup {
                surface_type: 0,
                has_per_triangle_data: false,
                triangles,
            }],
        )
    }

    fn collision_with_triangle(surface_type: u16, points: [[f32; 3]; 3]) -> ColFile {
        ColFile::new(
            points
                .into_iter()
                .map(|[x, y, z]| ColVertex::new(x, y, z))
                .collect(),
            vec![ColGroup {
                surface_type,
                has_per_triangle_data: false,
                triangles: vec![ColTriangle {
                    vertex_indices: [0, 1, 2],
                    attribute_0: 0,
                    attribute_1: 0,
                    data: None,
                }],
            }],
        )
    }

    fn authored_collision(surface_type: u16, x: f32, attribute: u8) -> ColFile {
        ColFile::new(
            vec![
                ColVertex::new(x, 0.0, 0.0),
                ColVertex::new(x + 1.0, 0.0, 0.0),
                ColVertex::new(x, 0.0, 1.0),
            ],
            vec![ColGroup {
                surface_type,
                has_per_triangle_data: false,
                triangles: vec![ColTriangle {
                    vertex_indices: [0, 1, 2],
                    attribute_0: attribute,
                    attribute_1: attribute.wrapping_add(1),
                    data: None,
                }],
            }],
        )
    }

    fn fixture_archive() -> Vec<u8> {
        let placement = JDramaDocument {
            root: JDramaRecord::new(
                "NameRefGrp",
                "root",
                JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: vec![
                        actor_record("first", [1.0, 2.0, 3.0]),
                        actor_record("second", [4.0, 5.0, 6.0]),
                    ],
                },
            )
            .unwrap(),
        }
        .to_bytes()
        .unwrap();
        let collision = ColFile::new(
            vec![
                ColVertex::new(0.0, 0.0, 0.0),
                ColVertex::new(1.0, 0.0, 0.0),
                ColVertex::new(0.0, 0.0, 1.0),
            ],
            vec![ColGroup {
                surface_type: 0,
                has_per_triangle_data: false,
                triangles: vec![ColTriangle {
                    vertex_indices: [0, 1, 2],
                    attribute_0: 0,
                    attribute_1: 0,
                    data: None,
                }],
            }],
        )
        .encode()
        .unwrap();
        let model = J3dRebuildDocument {
            file_type: *b"bmd3",
            version_tag: *b"SVR3",
            reserved_words: [u32::MAX; 3],
            declared_section_count: 0,
            sections: Vec::new(),
        }
        .to_bytes()
        .unwrap();
        let resources = [
            (b"scene.bin".as_slice(), placement),
            (b"map.col".as_slice(), collision),
            (b"map.bmd".as_slice(), model),
        ];
        let mut entries = resources
            .into_iter()
            .enumerate()
            .map(|(index, (name, data))| RarcEntryRecord {
                file_id: index as u16,
                name_hash: 0,
                flags: 0x11,
                name_offset: 0,
                raw_name: name.to_vec(),
                data_offset: 0,
                size: data.len() as u32,
                reserved: 0,
                data: Some(data),
            })
            .collect::<Vec<_>>();
        entries.extend(root_directory_entries());
        let mut archive = RarcDocument {
            layout: RarcLayout {
                file_size: 0,
                header_size: 0x20,
                data_offset: 0,
                data_size: 0,
                mram_data_size: 0,
                aram_data_size: 0,
                dvd_data_size: 0,
                metadata_present: true,
                node_offset: 0,
                entry_offset: 0,
                string_table_offset: 0,
                string_table_size: 0,
                next_free_file_id: entries.len() as u16,
                sync_file_ids: 1,
                info_reserved: [0; 5],
                alignment: 0x20,
                padding_byte: 0,
            },
            nodes: vec![RarcNodeRecord {
                node_type: *b"ROOT",
                name_offset: 0,
                name_hash: 0,
                raw_name: b"root".to_vec(),
                entry_count: entries.len() as u16,
                first_entry_index: 0,
            }],
            entries,
        };
        archive.canonicalize_layout().unwrap();
        archive.to_bytes().unwrap()
    }

    fn root_directory_entries() -> [RarcEntryRecord; 2] {
        [
            RarcEntryRecord {
                file_id: u16::MAX,
                name_hash: 0,
                flags: 0x02,
                name_offset: 0,
                raw_name: b".".to_vec(),
                data_offset: 0,
                size: 0,
                reserved: 0,
                data: None,
            },
            RarcEntryRecord {
                file_id: u16::MAX,
                name_hash: 0,
                flags: 0x02,
                name_offset: 0,
                raw_name: b"..".to_vec(),
                data_offset: u32::MAX,
                size: 0,
                reserved: 0,
                data: None,
            },
        ]
    }

    fn actor_record(name: &str, translation: [f32; 3]) -> JDramaRecord {
        JDramaRecord::new(
            "MapStaticObj",
            name,
            JDramaRecordPayload::Actor {
                transform: JDramaTransform {
                    translation,
                    rotation: if name == "first" {
                        [0.0, 90.0, 0.0]
                    } else {
                        [0.0; 3]
                    },
                    scale: [1.0; 3],
                },
                character_name: name.to_string(),
                light_map: JDramaLightMap::default(),
                fields: Vec::new(),
            },
        )
        .unwrap()
    }

    fn area_cylinder_record(name: &str) -> JDramaRecord {
        JDramaRecord::new(
            "AreaCylinder",
            name,
            JDramaRecordPayload::Fields {
                fields: vec![
                    JDramaField {
                        name: "center".to_string(),
                        value: JDramaFieldValue::Vec3F32([1.0, 2.0, 3.0]),
                    },
                    JDramaField {
                        name: "authoring_vector".to_string(),
                        value: JDramaFieldValue::Vec3F32([4.0, 5.0, 6.0]),
                    },
                    JDramaField {
                        name: "cylinder_parameters".to_string(),
                        value: JDramaFieldValue::Vec3F32([7.0, 8.0, 9.0]),
                    },
                    JDramaField {
                        name: "authoring_character_name".to_string(),
                        value: JDramaFieldValue::String("area character".to_string()),
                    },
                    JDramaField {
                        name: "indexed_name_count".to_string(),
                        value: JDramaFieldValue::U32(0),
                    },
                    JDramaField {
                        name: "manager_group_name".to_string(),
                        value: JDramaFieldValue::String("area manager".to_string()),
                    },
                    JDramaField {
                        name: "raw_angle_hundredths".to_string(),
                        value: JDramaFieldValue::I32(0),
                    },
                ],
            },
        )
        .unwrap()
    }

    fn authoring_strategy_archive(
        existing_map_objects: Vec<JDramaRecord>,
    ) -> SourceFreeStageArchive {
        let root = JDramaRecord::new(
            "Strategy",
            "strategy",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![
                    indexed_group_record(2, "managers", Vec::new()),
                    indexed_group_record(3, "objects", existing_map_objects),
                    indexed_group_record(7, "enemies", Vec::new()),
                ],
            },
        )
        .unwrap();
        let mut archive = SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(
                WORLD_SCENE_PATH.to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            )
            .unwrap();
        archive
    }

    fn authoring_conductor_archive(
        conductor_children: Vec<JDramaRecord>,
        indexed_manager_children: Vec<JDramaRecord>,
    ) -> SourceFreeStageArchive {
        let conductor = JDramaRecord::new(
            "GroupObj",
            "conductor initialization",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: conductor_children,
            },
        )
        .unwrap();
        let strategy = JDramaRecord::new(
            "Strategy",
            "strategy",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![
                    indexed_group_record(2, "managers", indexed_manager_children),
                    indexed_group_record(7, "enemies", Vec::new()),
                ],
            },
        )
        .unwrap();
        let root = JDramaRecord::new(
            "GroupObj",
            "whole scene",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![conductor, strategy],
            },
        )
        .unwrap();
        let mut archive = SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(
                WORLD_SCENE_PATH.to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            )
            .unwrap();
        archive
    }

    fn indexed_group_record(
        group_index: u32,
        name: &str,
        children: Vec<JDramaRecord>,
    ) -> JDramaRecord {
        JDramaRecord::new(
            "IdxGroup",
            name,
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "group_index".to_string(),
                    value: JDramaFieldValue::U32(group_index),
                }],
                children,
            },
        )
        .unwrap()
    }

    fn authored_test_object(
        id: &str,
        prototype: JDramaRecord,
        target_group_index: u32,
        dependencies: Vec<AuthoredPlacementDependency>,
    ) -> SceneObject {
        let mut object = SceneObject::new(id, prototype.type_name.clone());
        crate::seed_scene_object_parameters(&mut object, &prototype).unwrap();
        object.placement = Some(PlacementBinding::Authored(AuthoredPlacement {
            raw_resource_path: WORLD_SCENE_PATH.to_vec(),
            target_group_index,
            prototype,
            dependencies,
        }));
        object
    }

    fn fixture_manager_dependency(capacity: u32) -> AuthoredPlacementDependency {
        AuthoredPlacementDependency {
            target: None,
            target_group_index: 2,
            record: JDramaRecord::new(
                "FixtureManager",
                "fixture manager",
                JDramaRecordPayload::Fields {
                    fields: vec![
                        JDramaField {
                            name: "character_name".to_string(),
                            value: JDramaFieldValue::String(
                                "fixture manager character".to_string(),
                            ),
                        },
                        JDramaField {
                            name: "capacity".to_string(),
                            value: JDramaFieldValue::U32(capacity),
                        },
                        JDramaField {
                            name: "manager_load_value".to_string(),
                            value: JDramaFieldValue::U32(0),
                        },
                    ],
                },
            )
            .unwrap(),
        }
    }

    fn existing_and_clone_objects(prototype: &JDramaRecord) -> Vec<SceneObject> {
        let source_address = PlacementAddress {
            raw_resource_path: WORLD_SCENE_PATH.to_vec(),
            record_path: vec![1, 0],
        };
        let mut existing = SceneObject::new("existing", prototype.type_name.clone());
        crate::seed_scene_object_parameters(&mut existing, prototype).unwrap();
        existing.placement = Some(PlacementBinding::Existing(source_address.clone()));
        let mut clone = existing.clone();
        clone.id = "clone".to_string();
        clone.placement = Some(PlacementBinding::CloneOf(source_address));
        vec![existing, clone]
    }

    fn fixture_manager_capacity(archive: &SourceFreeStageArchive) -> u32 {
        let manager_path = named_record_paths(
            archive,
            WORLD_SCENE_PATH,
            "FixtureManager",
            "fixture manager",
        )
        .unwrap()
        .pop()
        .unwrap();
        let record = placement_record(
            archive,
            &PlacementAddress {
                raw_resource_path: WORLD_SCENE_PATH.to_vec(),
                record_path: manager_path,
            },
        )
        .unwrap();
        let fields = jdrama_record_fields(record).unwrap();
        let JDramaFieldValue::U32(capacity) = typed_field_value(fields, "capacity") else {
            panic!("fixture manager capacity was not a u32");
        };
        *capacity
    }

    fn common_launcher_record(name: &str, launched_enemy_name: &str) -> JDramaRecord {
        let mut record = map_static_record(name, "launcher resource");
        record.type_name = "CommonLauncher".to_string();
        let JDramaRecordPayload::Actor { fields, .. } = &mut record.payload else {
            unreachable!()
        };
        fields.push(JDramaField {
            name: "launched_enemy_name".to_string(),
            value: JDramaFieldValue::String(launched_enemy_name.to_string()),
        });
        record
    }

    fn live_actor_record(name: &str, manager_name: &str) -> JDramaRecord {
        JDramaRecord::new(
            "LiveActor",
            name,
            JDramaRecordPayload::Actor {
                transform: JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: format!("{name} character"),
                light_map: JDramaLightMap::default(),
                fields: vec![JDramaField {
                    name: "manager_name".to_string(),
                    value: JDramaFieldValue::String(manager_name.to_string()),
                }],
            },
        )
        .unwrap()
    }

    fn map_static_record(name: &str, resource_name: &str) -> JDramaRecord {
        JDramaRecord::new(
            "MapStaticObj",
            name,
            JDramaRecordPayload::Actor {
                transform: JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: format!("{name} character"),
                light_map: JDramaLightMap::default(),
                fields: vec![JDramaField {
                    name: "resource_name".to_string(),
                    value: JDramaFieldValue::String(resource_name.to_string()),
                }],
            },
        )
        .unwrap()
    }

    fn map_change_stage_record(name: &str, resource_name: &str, stage_id: u32) -> JDramaRecord {
        JDramaRecord::new(
            "MapObjChangeStage",
            name,
            JDramaRecordPayload::Actor {
                transform: JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: format!("{name} character"),
                light_map: JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "resource_name".to_string(),
                        value: JDramaFieldValue::String(resource_name.to_string()),
                    },
                    JDramaField {
                        name: "stage_id".to_string(),
                        value: JDramaFieldValue::U32(stage_id),
                    },
                ],
            },
        )
        .unwrap()
    }

    fn typed_field_value<'a>(fields: &'a [JDramaField], name: &str) -> &'a JDramaFieldValue {
        let matches = fields
            .iter()
            .filter(|field| field.name == name)
            .collect::<Vec<_>>();
        let [field] = matches.as_slice() else {
            panic!("expected one typed field named {name:?}");
        };
        &field.value
    }

    fn unique_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sms-stage-export-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
