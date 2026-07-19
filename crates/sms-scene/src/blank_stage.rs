//! Typed, source-free construction of a minimal playable stage archive.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sms_formats::{
    compile_static_bmd3, ColFile, FormatError, GxMaterial, J3dRebuildDocument,
    J3dRebuildSectionData, JDramaDocument, JDramaField, JDramaFieldValue, JDramaLightMap,
    JDramaRecord, JDramaRecordPayload, JDramaTransform, JpaxDocument, StaticModel, StaticModelMesh,
    StaticModelVertex,
};

use crate::stage_archive::parse_resource;
use crate::{
    Result, SceneError, SourceFreeStageArchive, StageCompression, StageResource,
    StageResourceDocument,
};

pub const BLANK_STAGE_PRESET_VERSION: u32 = 6;
pub const DEFAULT_BLANK_STAGE_TARGET_SLOT: &str = "test11";
pub const BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH: &[u8] = b"map/camera/sms_runtime_directory.bmd";
pub const BLANK_STAGE_COIN_PARTICLE_PATH: &[u8] = b"mapobj/ms_watcoin_kira.jpa";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlankStageBootstrapKind {
    Model,
    Collision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlankStageBootstrapRequirement {
    pub raw_path: &'static [u8],
    pub kind: BlankStageBootstrapKind,
}

pub const BLANK_STAGE_BOOTSTRAP_REQUIREMENTS: [BlankStageBootstrapRequirement; 5] = [
    BlankStageBootstrapRequirement {
        raw_path: b"mapobj/coin.bmd",
        kind: BlankStageBootstrapKind::Model,
    },
    BlankStageBootstrapRequirement {
        raw_path: b"mapobj/bottle_large.bmd",
        kind: BlankStageBootstrapKind::Model,
    },
    BlankStageBootstrapRequirement {
        raw_path: b"mapobj/normalblock.bmd",
        kind: BlankStageBootstrapKind::Model,
    },
    BlankStageBootstrapRequirement {
        raw_path: b"mapobj/normalblock.col",
        kind: BlankStageBootstrapKind::Collision,
    },
    BlankStageBootstrapRequirement {
        raw_path: b"mapobj/juiceblock.bmd",
        kind: BlankStageBootstrapKind::Model,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlankStageBootstrapResource {
    pub raw_path: Vec<u8>,
    pub bytes: Vec<u8>,
}

/// Parsed bootstrap dependency closure. Input bytes are consumed into typed
/// resource documents immediately and are never retained as archive fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub struct BlankStageBootstrapManifest {
    resources: Vec<StageResource>,
}

impl BlankStageBootstrapManifest {
    pub fn from_authored_bytes(
        resources: impl IntoIterator<Item = BlankStageBootstrapResource>,
    ) -> Result<Self> {
        let mut seen = BTreeSet::new();
        let mut parsed = Vec::new();
        for resource in resources {
            let raw_path = normalize_bootstrap_path(resource.raw_path)?;
            if !seen.insert(raw_path.clone()) {
                return Err(blank_stage_error(format!(
                    "bootstrap resource {} is duplicated",
                    display_raw_path(&raw_path)
                )));
            }
            let document = parse_resource(&raw_path, &resource.bytes).map_err(|source| {
                SceneError::StageResource {
                    path: display_raw_path(&raw_path),
                    source,
                }
            })?;
            parsed.push(StageResource { raw_path, document });
        }
        parsed.sort_by(|left, right| left.raw_path.cmp(&right.raw_path));

        for requirement in BLANK_STAGE_BOOTSTRAP_REQUIREMENTS {
            let Some(resource) = parsed
                .iter()
                .find(|resource| resource.raw_path == requirement.raw_path)
            else {
                return Err(blank_stage_error(format!(
                    "required authored bootstrap resource {} is missing",
                    display_raw_path(requirement.raw_path)
                )));
            };
            let kind_matches = matches!(
                (requirement.kind, &resource.document),
                (
                    BlankStageBootstrapKind::Model,
                    StageResourceDocument::Model(_)
                ) | (
                    BlankStageBootstrapKind::Collision,
                    StageResourceDocument::Collision(_)
                )
            );
            if !kind_matches {
                return Err(blank_stage_error(format!(
                    "bootstrap resource {} has the wrong semantic kind",
                    display_raw_path(requirement.raw_path)
                )));
            }
        }
        Ok(Self { resources: parsed })
    }

    pub fn resources(&self) -> &[StageResource] {
        &self.resources
    }
}

/// A source-free skybox selection copied from a complete typed stage archive.
///
/// Sunshine's `TSky` actor always opens `map/map/sky.bmd`, while water mirrors
/// draw the separate `map/map/reflectsky.bmd` helper. Keeping both models,
/// their same-stem animations, and the shared `sky.bmt` together prevents a
/// visually plausible sky selection from retaining the previous stage's
/// reflection in a managed build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlankStageSkyboxPreset {
    sky_actor: JDramaRecord,
    resources: Vec<StageResource>,
}

impl BlankStageSkyboxPreset {
    /// Clones the canonical `Sky` actor and its complete typed runtime bundle.
    /// Retail reflection helpers are retained when present. Stages without a
    /// dedicated helper use the main sky as the reflection fallback, matching
    /// the selected sky instead of silently retaining an unrelated proxy.
    pub fn from_archive(archive: &SourceFreeStageArchive) -> Result<Self> {
        let placement = canonical_placement_document(archive)?;
        let sky_actor = unique_record(placement, "Sky", "sky actor")?.clone();
        let mut resources = archive
            .resources()
            .iter()
            .filter(|resource| is_skybox_resource_path(&resource.raw_path))
            .cloned()
            .collect::<Vec<_>>();

        if !resources
            .iter()
            .any(|resource| raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/map/sky.bmt"))
        {
            let sky_model = resources
                .iter()
                .find_map(|resource| {
                    raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/map/sky.bmd")
                        .then_some(&resource.document)
                })
                .and_then(|document| match document {
                    StageResourceDocument::Model(model) => Some(model),
                    _ => None,
                })
                .ok_or_else(|| {
                    blank_stage_error(
                        "skybox preset cannot derive sky.bmt without a typed map/map/sky.bmd",
                    )
                })?;
            resources.push(StageResource {
                raw_path: b"map/map/sky.bmt".to_vec(),
                document: StageResourceDocument::Model(runtime_sky_material_table(sky_model)?),
            });
        }

        if !resources.iter().any(|resource| {
            raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/map/reflectsky.bmd")
        }) {
            let fallbacks = resources
                .iter()
                .filter_map(reflection_fallback_resource)
                .collect::<Vec<_>>();
            resources.extend(fallbacks);
        }
        resources.sort_by(|left, right| left.raw_path.cmp(&right.raw_path));

        let preset = Self {
            sky_actor,
            resources,
        };
        preset.validate()?;
        Ok(preset)
    }

    pub fn sky_actor(&self) -> &JDramaRecord {
        &self.sky_actor
    }

    pub fn resources(&self) -> &[StageResource] {
        &self.resources
    }

    fn validate(&self) -> Result<()> {
        if semantic_type_name(&self.sky_actor.type_name) != "Sky"
            || !matches!(&self.sky_actor.payload, JDramaRecordPayload::Actor { .. })
        {
            return Err(blank_stage_error(
                "skybox preset does not contain a typed Sky actor",
            ));
        }

        let mut paths = BTreeSet::new();
        let mut model_count = 0_usize;
        let mut material_table_count = 0_usize;
        let mut reflection_model_count = 0_usize;
        for resource in &self.resources {
            if !paths.insert(resource.raw_path.clone()) {
                return Err(blank_stage_error(format!(
                    "skybox resource {} is duplicated",
                    display_raw_path(&resource.raw_path)
                )));
            }
            if !is_skybox_resource_path(&resource.raw_path)
                || !matches!(
                    &resource.document,
                    StageResourceDocument::Model(_)
                        | StageResourceDocument::Animation(_)
                        | StageResourceDocument::AnimationSound(_)
                )
            {
                return Err(blank_stage_error(format!(
                    "skybox resource {} is not a typed sky model or animation",
                    display_raw_path(&resource.raw_path)
                )));
            }
            if raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/map/sky.bmd") {
                if !matches!(&resource.document, StageResourceDocument::Model(_)) {
                    return Err(blank_stage_error(
                        "map/map/sky.bmd is not a typed J3D model",
                    ));
                }
                model_count += 1;
            } else if raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/map/sky.bmt") {
                if !matches!(&resource.document, StageResourceDocument::Model(_)) {
                    return Err(blank_stage_error(
                        "map/map/sky.bmt is not a typed J3D material table",
                    ));
                }
                material_table_count += 1;
            } else if raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/map/reflectsky.bmd") {
                if !matches!(&resource.document, StageResourceDocument::Model(_)) {
                    return Err(blank_stage_error(
                        "map/map/reflectsky.bmd is not a typed J3D model",
                    ));
                }
                reflection_model_count += 1;
            }
        }
        if model_count != 1 {
            return Err(blank_stage_error(format!(
                "skybox preset requires exactly one typed map/map/sky.bmd resource, found {model_count}"
            )));
        }
        if material_table_count != 1 {
            return Err(blank_stage_error(format!(
                "skybox preset requires exactly one typed map/map/sky.bmt resource, found {material_table_count}"
            )));
        }
        if reflection_model_count != 1 {
            return Err(blank_stage_error(format!(
                "skybox preset requires exactly one typed map/map/reflectsky.bmd resource, found {reflection_model_count}"
            )));
        }
        Ok(())
    }
}

/// Builds the material table copied by Sunshine onto both `TSky` and
/// `ReflectSky`. The result is detached typed J3D data, not cached source
/// bytes, so it remains safe for project overlays and semantic rebuilds.
pub fn runtime_sky_material_table(model: &J3dRebuildDocument) -> Result<J3dRebuildDocument> {
    let sections = model
        .sections
        .iter()
        .filter(|section| {
            matches!(
                section.data,
                J3dRebuildSectionData::Materials(_) | J3dRebuildSectionData::Textures(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if !sections
        .iter()
        .any(|section| matches!(section.data, J3dRebuildSectionData::Materials(_)))
    {
        return Err(blank_stage_error(
            "sky model has no MAT3 section for the shared runtime material table",
        ));
    }
    Ok(J3dRebuildDocument {
        file_type: *b"bmt3",
        version_tag: model.version_tag,
        reserved_words: model.reserved_words,
        declared_section_count: sections.len() as u32,
        sections,
    })
}

fn reflection_fallback_resource(resource: &StageResource) -> Option<StageResource> {
    let reflected_path = reflection_resource_path(&resource.raw_path)?;
    Some(StageResource {
        raw_path: reflected_path,
        document: resource.document.clone(),
    })
}

fn reflection_resource_path(raw_path: &[u8]) -> Option<Vec<u8>> {
    let separator = raw_path.iter().rposition(|byte| *byte == b'/')?;
    if !raw_path_eq_ignore_ascii_case(&raw_path[..separator], b"map/map") {
        return None;
    }
    let file_name = &raw_path[separator + 1..];
    let extension = file_name.iter().rposition(|byte| *byte == b'.')?;
    if !raw_path_eq_ignore_ascii_case(&file_name[..extension], b"sky") {
        return None;
    }
    let extension = &file_name[extension + 1..];
    if !matches_ignore_ascii_case(
        extension,
        &[b"bmd".as_slice(), b"bck", b"bpk", b"btp", b"brk", b"btk"],
    ) {
        return None;
    }
    let mut path = b"map/map/reflectsky.".to_vec();
    path.extend_from_slice(extension);
    Some(path)
}

fn matches_ignore_ascii_case(value: &[u8], candidates: &[&[u8]]) -> bool {
    candidates
        .iter()
        .any(|candidate| raw_path_eq_ignore_ascii_case(value, candidate))
}

/// The complete stage-global lighting records used by Sunshine's runtime.
///
/// Record ordering is significant: the runtime finds each role's primary
/// light/ambient by name and then indexes consecutive entries. Cloning the
/// complete arrays also retains fields such as light range that the editor's
/// flattened viewport lighting summary intentionally omits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlankStageLightingPreset {
    ambient_group: JDramaRecord,
    light_group: JDramaRecord,
    mar_scene_light_map: JDramaLightMap,
    sun_manager: JDramaRecord,
}

impl BlankStageLightingPreset {
    /// Clones the ordered `AmbAry` and `LightAry` records, the `MarScene`
    /// light map, and the complete `SunMgr` record from a typed stage archive.
    pub fn from_archive(archive: &SourceFreeStageArchive) -> Result<Self> {
        let placement = canonical_placement_document(archive)?;
        let mar_scene = unique_record(placement, "MarScene", "MarScene record")?;
        let mar_scene_light_map = record_light_map(mar_scene)?.clone();
        let preset = Self {
            ambient_group: unique_record(placement, "AmbAry", "ambient group")?.clone(),
            light_group: unique_record(placement, "LightAry", "light group")?.clone(),
            mar_scene_light_map,
            sun_manager: unique_record(placement, "SunMgr", "sun manager")?.clone(),
        };
        preset.validate()?;
        Ok(preset)
    }

    pub fn ambient_group(&self) -> &JDramaRecord {
        &self.ambient_group
    }

    pub fn light_group(&self) -> &JDramaRecord {
        &self.light_group
    }

    pub fn mar_scene_light_map(&self) -> &JDramaLightMap {
        &self.mar_scene_light_map
    }

    pub fn sun_manager(&self) -> &JDramaRecord {
        &self.sun_manager
    }

    fn validate(&self) -> Result<()> {
        validate_lighting_group(&self.ambient_group, "AmbAry", "Ambient Group", "AmbColor")?;
        validate_lighting_group(&self.light_group, "LightAry", "Light Group", "Light")?;
        if semantic_type_name(&self.sun_manager.type_name) != "SunMgr"
            || !matches!(
                &self.sun_manager.payload,
                JDramaRecordPayload::Fields { .. }
            )
        {
            return Err(blank_stage_error(
                "lighting preset does not contain a typed SunMgr record",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlankStageTargetMetadata {
    pub target_slot: String,
    pub output_archive_name: String,
    pub replaces_existing_stage_mapping: bool,
    pub stage_table_entry_required: bool,
    pub runtime_patch_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlankStagePreset {
    pub target_slot: String,
    pub spawn: Option<JDramaTransform>,
    pub compression: Option<StageCompression>,
    #[serde(default)]
    pub skybox: Option<BlankStageSkyboxPreset>,
    #[serde(default)]
    pub lighting: Option<BlankStageLightingPreset>,
}

impl Default for BlankStagePreset {
    fn default() -> Self {
        Self {
            target_slot: DEFAULT_BLANK_STAGE_TARGET_SLOT.to_string(),
            spawn: None,
            compression: Some(StageCompression::Yaz0 { reserved: [0; 8] }),
            skybox: None,
            lighting: None,
        }
    }
}

impl BlankStagePreset {
    pub fn target_metadata(&self) -> Result<BlankStageTargetMetadata> {
        validate_target_slot(&self.target_slot)?;
        Ok(BlankStageTargetMetadata {
            target_slot: self.target_slot.clone(),
            output_archive_name: format!("{}.szs", self.target_slot),
            replaces_existing_stage_mapping: false,
            stage_table_entry_required: true,
            runtime_patch_required: false,
        })
    }

    /// Builds an empty but structurally complete authored stage shell.
    ///
    /// The internal map model is a canonical degenerate triangle and the
    /// collision document is empty. They exist because Sunshine's
    /// `TMapModelManager` unconditionally opens both resources; neither is
    /// user-authored world content. A later map-terrain placement can replace
    /// the model and append its collision through the normal semantic edits.
    /// Mario and the sky actor are absent unless explicitly supplied by this
    /// preset, while neutral editable lighting records remain because the
    /// retail runtime dereferences `AmbAry`, `LightAry`, and `SunMgr` during
    /// setup without null checks.
    pub fn build(&self, bootstrap: BlankStageBootstrapManifest) -> Result<SourceFreeStageArchive> {
        self.build_with_world(
            blank_world_model()?,
            ColFile::new(Vec::new(), Vec::new()),
            bootstrap,
        )
    }

    /// Builds the same authored shell with an explicit map model/collision
    /// baseline. This is retained for automation that intentionally starts
    /// from authored terrain; interactive new-stage creation should use
    /// [`Self::build`] and place terrain in the scene afterward.
    pub fn build_with_world(
        &self,
        world_model: J3dRebuildDocument,
        world_collision: ColFile,
        bootstrap: BlankStageBootstrapManifest,
    ) -> Result<SourceFreeStageArchive> {
        validate_target_slot(&self.target_slot)?;
        if let Some(skybox) = &self.skybox {
            skybox.validate()?;
        }
        if let Some(lighting) = &self.lighting {
            lighting.validate()?;
        }
        if let Some(spawn) = self.spawn {
            validate_transform(spawn)?;
        }
        let placement =
            blank_scene_document(self.spawn, self.skybox.as_ref(), self.lighting.as_ref())?;

        let mut archive = SourceFreeStageArchive::new_for_blank(
            self.target_slot.clone(),
            BLANK_STAGE_PRESET_VERSION,
        )?;
        archive.set_compression(self.compression);
        archive.insert_resource(
            b"map/scene.bin".to_vec(),
            StageResourceDocument::Placement(placement),
        )?;
        archive.insert_resource(
            b"map/map/map.bmd".to_vec(),
            StageResourceDocument::Model(world_model),
        )?;
        archive.insert_resource(
            b"map/map.col".to_vec(),
            StageResourceDocument::Collision(world_collision),
        )?;
        // CPolarSubCamera constructs TCameraBck during scene loading, and
        // MActorAnmData::init unconditionally enumerates /scene/map/camera
        // before it adds the shared camera animations from common.arc. RARC
        // cannot retain an empty folder in the detached resource model, so a
        // tiny typed source-free model keeps that required directory present.
        // The runtime animation scanner ignores .bmd files here.
        archive.insert_resource(
            BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH.to_vec(),
            StageResourceDocument::Model(blank_world_model()?),
        )?;
        // TItemManager creates its pooled coins during loadAfter, and every
        // TCoin::initMapObj unconditionally registers this stage-local effect.
        // Authored stages do not inherit a retail mapObj directory, so retain
        // the required effect ID with a source-free zero-spawn JPA document.
        archive.insert_resource(
            BLANK_STAGE_COIN_PARTICLE_PATH.to_vec(),
            StageResourceDocument::Particle(JpaxDocument::authored_noop()),
        )?;
        if let Some(skybox) = &self.skybox {
            for resource in &skybox.resources {
                archive.insert_resource(resource.raw_path.clone(), resource.document.clone())?;
            }
        }
        for resource in bootstrap.resources {
            archive.insert_resource(resource.raw_path, resource.document)?;
        }
        Ok(archive)
    }
}

/// Upgrades an existing source-free blank-stage baseline with runtime shell
/// resources added by newer presets.
pub fn ensure_blank_stage_runtime_resources(archive: &mut SourceFreeStageArchive) -> Result<bool> {
    let crate::StageOrigin::Blank {
        target_slot,
        preset_version,
    } = archive.origin().clone()
    else {
        return Ok(false);
    };

    let mut changed = false;
    let placement = match archive.resource_mut(b"map/scene.bin") {
        Some(StageResourceDocument::Placement(placement)) => placement,
        Some(_) => {
            return Err(blank_stage_error(
                "blank stage map/scene.bin is not typed placement data",
            ));
        }
        None => {
            return Err(blank_stage_error(
                "blank stage is missing the required map/scene.bin hierarchy",
            ));
        }
    };
    if ensure_camera_map_tool_table(placement)? {
        changed = true;
    }
    if ensure_runtime_cube_managers(placement)? {
        changed = true;
    }
    if archive
        .resource(BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH)
        .is_none()
    {
        archive.insert_resource(
            BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH.to_vec(),
            StageResourceDocument::Model(blank_world_model()?),
        )?;
        changed = true;
    }
    if archive.resource(BLANK_STAGE_COIN_PARTICLE_PATH).is_none() {
        archive.insert_resource(
            BLANK_STAGE_COIN_PARTICLE_PATH.to_vec(),
            StageResourceDocument::Particle(JpaxDocument::authored_noop()),
        )?;
        changed = true;
    }
    if preset_version < BLANK_STAGE_PRESET_VERSION {
        archive.set_origin(crate::StageOrigin::Blank {
            target_slot,
            preset_version: BLANK_STAGE_PRESET_VERSION,
        });
        changed = true;
    }
    Ok(changed)
}

fn blank_scene_document(
    spawn: Option<JDramaTransform>,
    skybox: Option<&BlankStageSkyboxPreset>,
    lighting: Option<&BlankStageLightingPreset>,
) -> Result<JDramaDocument> {
    let conductor = group(
        "GroupObj",
        "\u{30B3}\u{30F3}\u{30C0}\u{30AF}\u{30BF}\u{30FC}\u{521D}\u{671F}\u{5316}\u{7528}",
        Vec::new(),
        vec![
            fields_record(
                "ItemManager",
                "\u{30A2}\u{30A4}\u{30C6}\u{30E0}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC}",
                vec![
                    field(
                        "character_name",
                        JDramaFieldValue::String(
                            "\u{30A2}\u{30A4}\u{30C6}\u{30E0}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC} \u{30AD}\u{30E3}\u{30E9}"
                                .to_string(),
                        ),
                    ),
                    field("capacity", JDramaFieldValue::U32(300)),
                    field("clip_distance", JDramaFieldValue::F32(12_000.0)),
                    field("clip_radius", JDramaFieldValue::F32(500.0)),
                ],
            )?,
            fields_record(
                "MapObjManager",
                "\u{5730}\u{5F62}\u{30AA}\u{30D6}\u{30B8}\u{30A7}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC}",
                vec![
                    field(
                        "character_name",
                        JDramaFieldValue::String(
                            "\u{30A2}\u{30A4}\u{30C6}\u{30E0}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC} \u{30AD}\u{30E3}\u{30E9}"
                                .to_string(),
                        ),
                    ),
                    field("capacity", JDramaFieldValue::U32(300)),
                    field("clip_distance", JDramaFieldValue::F32(5_000.0)),
                    field("clip_radius", JDramaFieldValue::F32(500.0)),
                ],
            )?,
        ],
    )?;
    let mirror_scene = group(
        "GroupObj",
        "\u{93E1}\u{30B7}\u{30FC}\u{30F3}",
        Vec::new(),
        vec![actor(
            "MirrorCamera",
            "\u{93E1}\u{30AB}\u{30E1}\u{30E9}",
            identity_transform(),
            "\u{93E1}\u{30AB}\u{30E1}\u{30E9} \u{30AD}\u{30E3}\u{30E9}",
            Vec::new(),
        )?],
    )?;
    let mirror_manager = fields_record(
        "MirrorModelManager",
        "\u{93E1}\u{8868}\u{793A}\u{30E2}\u{30C7}\u{30EB}\u{7BA1}\u{7406}",
        vec![
            field("opaque_model_count", JDramaFieldValue::I32(0)),
            field("translucent_model_count", JDramaFieldValue::I32(0)),
            field("paired_model_count", JDramaFieldValue::I32(0)),
        ],
    )?;
    let ambient = match lighting {
        Some(preset) => preset.ambient_group.clone(),
        None => ambient_group()?,
    };
    let lights = match lighting {
        Some(preset) => preset.light_group.clone(),
        None => light_group()?,
    };

    let normal_scene = group(
        "MarScene",
        "\u{901A}\u{5E38}\u{30B7}\u{30FC}\u{30F3}",
        vec![field(
            "light_map",
            JDramaFieldValue::LightMap(
                lighting
                    .map(|preset| preset.mar_scene_light_map.clone())
                    .unwrap_or_default(),
            ),
        )],
        vec![
            ambient,
            lights,
            strategy(
                spawn,
                lighting.map(|preset| preset.sun_manager.clone()),
                skybox.map(|preset| preset.sky_actor.clone()),
            )?,
            camera_group()?,
        ],
    )?;
    Ok(JDramaDocument {
        root: group(
            "GroupObj",
            "\u{5168}\u{4F53}\u{30B7}\u{30FC}\u{30F3}",
            Vec::new(),
            vec![
                conductor,
                mirror_scene,
                mirror_manager,
                group(
                    "GroupObj",
                    "\u{30B9}\u{30DA}\u{30AD}\u{30E5}\u{30E9}\u{30B7}\u{30FC}\u{30F3}",
                    Vec::new(),
                    Vec::new(),
                )?,
                group(
                    "GroupObj",
                    "\u{30A4}\u{30F3}\u{30C0}\u{30A4}\u{30EC}\u{30AF}\u{30C8}\u{30B7}\u{30FC}\u{30F3}",
                    Vec::new(),
                    Vec::new(),
                )?,
                normal_scene,
            ],
        )?,
    })
}

fn ambient_group() -> Result<JDramaRecord> {
    let roles = [
        "\u{30D7}\u{30EC}\u{30A4}\u{30E4}\u{30FC}",
        "\u{30AA}\u{30D6}\u{30B8}\u{30A7}\u{30AF}\u{30C8}",
        "\u{6575}",
    ];
    let mut children = Vec::with_capacity(6);
    for role in roles {
        children.push(fields_record(
            "AmbColor",
            &format!("\u{592A}\u{967D}\u{30A2}\u{30F3}\u{30D3}\u{30A8}\u{30F3}\u{30C8}\u{FF08}{role}\u{FF09}"),
            vec![field(
                "color",
                JDramaFieldValue::ColorRgba8([100, 100, 100, 255]),
            )],
        )?);
        children.push(fields_record(
            "AmbColor",
            &format!(
                "\u{5F71}\u{30A2}\u{30F3}\u{30D3}\u{30A8}\u{30F3}\u{30C8}\u{FF08}{role}\u{FF09}"
            ),
            vec![field(
                "color",
                JDramaFieldValue::ColorRgba8([40, 40, 40, 255]),
            )],
        )?);
    }
    group("AmbAry", "Ambient Group", Vec::new(), children)
}

fn light_group() -> Result<JDramaRecord> {
    let roles = [
        "\u{30D7}\u{30EC}\u{30A4}\u{30E4}\u{30FC}",
        "\u{30AA}\u{30D6}\u{30B8}\u{30A7}\u{30AF}\u{30C8}",
        "\u{6575}",
    ];
    let mut children = Vec::with_capacity(15);
    for role in roles {
        children.push(light(
            &format!("\u{592A}\u{967D}\u{FF08}{role}\u{FF09}"),
            [-100_000.0, 300_000.0, 400_000.0],
            [200, 200, 200, 255],
        )?);
        children.push(light(
            &format!("\u{592A}\u{967D}\u{30B5}\u{30D6}\u{FF08}{role}\u{FF09}"),
            [100_000.0, -300_000.0, -400_000.0],
            [50, 50, 50, 255],
        )?);
        children.push(light(
            &format!("\u{5F71}\u{FF08}{role}\u{FF09}"),
            [-100_000.0, 300_000.0, 400_000.0],
            [80, 80, 80, 255],
        )?);
        children.push(light(
            &format!("\u{5F71}\u{30B5}\u{30D6}\u{FF08}{role}\u{FF09}"),
            [100_000.0, -300_000.0, -400_000.0],
            [0, 0, 0, 255],
        )?);
        children.push(light(
            &format!(
                "\u{592A}\u{967D}\u{30B9}\u{30DA}\u{30AD}\u{30E5}\u{30E9}\u{FF08}{role}\u{FF09}"
            ),
            [-100_000.0, 300_000.0, 400_000.0],
            [200, 200, 200, 255],
        )?);
    }
    group("LightAry", "Light Group", Vec::new(), children)
}

fn light(name: &str, position: [f32; 3], color: [u8; 4]) -> Result<JDramaRecord> {
    fields_record(
        "Light",
        name,
        vec![
            field("position", JDramaFieldValue::Vec3F32(position)),
            field("color", JDramaFieldValue::ColorRgba8(color)),
            field("range", JDramaFieldValue::F32(50.0)),
        ],
    )
}

fn strategy(
    spawn: Option<JDramaTransform>,
    sun_manager: Option<JDramaRecord>,
    sky_actor: Option<JDramaRecord>,
) -> Result<JDramaRecord> {
    let map = fields_record(
        "Map",
        "\u{30DE}\u{30C3}\u{30D7}",
        vec![
            field("translucent_group_count", JDramaFieldValue::U32(0)),
            field("collision_grid_width", JDramaFieldValue::I32(80)),
            field("collision_grid_height", JDramaFieldValue::I32(80)),
            field("collision_triangle_capacity", JDramaFieldValue::I32(12_000)),
            field("collision_list_capacity", JDramaFieldValue::I32(25_000)),
            field("collision_warp_capacity", JDramaFieldValue::I32(8_000)),
            field("warp_pair_count", JDramaFieldValue::U32(0)),
        ],
    )?;
    let sun_manager = match sun_manager {
        Some(sun_manager) => sun_manager,
        None => fields_record(
            "SunMgr",
            "\u{592A}\u{967D}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC}",
            vec![
                field("sun_color_r", JDramaFieldValue::U32(10_103)),
                field("sun_color_g", JDramaFieldValue::U32(255)),
                field("sun_color_b", JDramaFieldValue::U32(120)),
                field("sun_color_a", JDramaFieldValue::U32(195)),
                field("sun_size", JDramaFieldValue::F32(0.0)),
            ],
        )?,
    };
    let mut manager_children = vec![sun_manager];
    manager_children.extend(runtime_cube_manager_records()?);
    manager_children.push(fields_record(
            "MapWireManager",
            "\u{30EF}\u{30A4}\u{30E4}\u{30FC}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC}",
            vec![
                field(
                    "character_name",
                    JDramaFieldValue::String(
                        "\u{30EF}\u{30A4}\u{30E4}\u{30FC}\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC} \u{30AD}\u{30E3}\u{30E9}"
                            .to_string(),
                    ),
                ),
                field("wire_capacity", JDramaFieldValue::U32(200)),
                field("actor_capacity", JDramaFieldValue::U32(10)),
                field("draw_width", JDramaFieldValue::F32(10.0)),
                field("draw_height", JDramaFieldValue::F32(20.0)),
                field("upper_red", JDramaFieldValue::U32(200)),
                field("upper_green", JDramaFieldValue::U32(200)),
                field("upper_blue", JDramaFieldValue::U32(200)),
                field("lower_red", JDramaFieldValue::U32(128)),
                field("lower_green", JDramaFieldValue::U32(128)),
                field("lower_blue", JDramaFieldValue::U32(128)),
            ],
        )?);
    let mario = spawn.map(blank_stage_mario_record).transpose()?;
    let pollution = actor(
        "Pollution",
        "\u{843D}\u{66F8}\u{304D}\u{7BA1}\u{7406}",
        identity_transform(),
        "\u{843D}\u{66F8}\u{304D}\u{7BA1}\u{7406} \u{30AD}\u{30E3}\u{30E9}",
        Vec::new(),
    )?;

    group(
        "Strategy",
        "\u{30B9}\u{30C8}\u{30E9}\u{30C6}\u{30B8}",
        Vec::new(),
        vec![
            indexed_group(
                0,
                "\u{30DE}\u{30C3}\u{30D7}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                vec![map],
            )?,
            indexed_group(
                1,
                "\u{7A7A}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                sky_actor.into_iter().collect(),
            )?,
            indexed_group(
                2,
                "\u{30DE}\u{30CD}\u{30FC}\u{30B8}\u{30E3}\u{30FC}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                manager_children,
            )?,
            indexed_group(
                3,
                "\u{30AA}\u{30D6}\u{30B8}\u{30A7}\u{30AF}\u{30C8}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
            indexed_group(
                4,
                "\u{843D}\u{66F8}\u{304D}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                vec![pollution],
            )?,
            indexed_group(
                5,
                "\u{30A2}\u{30A4}\u{30C6}\u{30E0}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
            indexed_group(
                6,
                "\u{30D7}\u{30EC}\u{30FC}\u{30E4}\u{30FC}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                mario.into_iter().collect(),
            )?,
            indexed_group(
                7,
                "\u{6575}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
            indexed_group(
                8,
                "\u{30DC}\u{30B9}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
            indexed_group(
                9,
                "\u{FF2E}\u{FF30}\u{FF23}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
            indexed_group(
                10,
                "\u{6C34}\u{30D1}\u{30FC}\u{30C6}\u{30A3}\u{30AF}\u{30EB}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
            indexed_group(
                11,
                "\u{521D}\u{671F}\u{5316}\u{7528}\u{30B0}\u{30EB}\u{30FC}\u{30D7}",
                Vec::new(),
            )?,
        ],
    )
}

fn camera_group() -> Result<JDramaRecord> {
    group(
        "GroupObj",
        "Cameras",
        Vec::new(),
        vec![
            camera_map_tool_table()?,
            empty_record("PolarSubCamera", "camera 1")?,
        ],
    )
}

fn camera_map_tool_table() -> Result<JDramaRecord> {
    group(
        "CameraMapToolTable",
        "\u{30AB}\u{30E1}\u{30E9}\u{30DE}\u{30C3}\u{30D7}\u{30C4}\u{30FC}\u{30EB}\u{30C6}\u{30FC}\u{30D6}\u{30EB}",
        Vec::new(),
        Vec::new(),
    )
}

fn ensure_camera_map_tool_table(document: &mut JDramaDocument) -> Result<bool> {
    let mut existing = Vec::new();
    collect_records_by_type(&document.root, "CameraMapToolTable", &mut existing);
    match existing.len() {
        0 => {}
        1 => return Ok(false),
        count => {
            return Err(blank_stage_error(format!(
                "blank stage camera hierarchy contains {count} CameraMapToolTable records"
            )));
        }
    }

    if insert_camera_map_tool_table_before_polar(&mut document.root)? {
        Ok(true)
    } else {
        // Source-free authored archives are allowed to provide a different
        // camera implementation (or none at all). The table is mandatory only
        // for scenes that instantiate CPolarSubCamera, whose loadAfter path
        // dereferences the global table unconditionally.
        Ok(false)
    }
}

const RUNTIME_CUBE_MANAGER_TYPES: [&str; 11] = [
    "CubeCamera",
    "CubeMirror",
    "CubeWire",
    "CubeStream",
    "CubeShadow",
    "CubeArea",
    "CubeFastA",
    "CubeFastB",
    "CubeFastC",
    "CubeSoundChange",
    "CubeSoundEffect",
];

fn runtime_cube_manager_records() -> Result<Vec<JDramaRecord>> {
    RUNTIME_CUBE_MANAGER_TYPES
        .iter()
        .map(|type_name| {
            let name = match *type_name {
                "CubeCamera" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{30AB}\u{30E1}\u{30E9}\u{FF09}",
                "CubeMirror" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{93E1}\u{FF09}",
                "CubeWire" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{30EF}\u{30A4}\u{30E4}\u{30FC}\u{FF09}",
                "CubeStream" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{6D41}\u{308C}\u{FF09}",
                "CubeShadow" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{5F71}\u{FF09}",
                "CubeArea" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{30A8}\u{30EA}\u{30A2}\u{FF09}",
                "CubeFastA" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{9AD8}\u{901F}\u{FF21}\u{FF09}",
                "CubeFastB" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{9AD8}\u{901F}\u{FF22}\u{FF09}",
                "CubeFastC" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{9AD8}\u{901F}\u{FF23}\u{FF09}",
                "CubeSoundChange" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{30B5}\u{30A6}\u{30F3}\u{30C9}\u{5207}\u{308A}\u{66FF}\u{3048}\u{FF09}",
                "CubeSoundEffect" => "\u{30AD}\u{30E5}\u{30FC}\u{30D6}\u{FF08}\u{30B5}\u{30A6}\u{30F3}\u{30C9}\u{30A8}\u{30D5}\u{30A7}\u{30AF}\u{30C8}\u{FF09}",
                _ => unreachable!("runtime cube manager table is exhaustive"),
            };
            empty_record(type_name, name)
        })
        .collect()
}

fn ensure_runtime_cube_managers(document: &mut JDramaDocument) -> Result<bool> {
    let mut missing = Vec::new();
    for type_name in RUNTIME_CUBE_MANAGER_TYPES {
        let mut existing = Vec::new();
        collect_records_by_type(&document.root, type_name, &mut existing);
        match existing.len() {
            0 => missing.push(type_name),
            1 => {}
            count => {
                return Err(blank_stage_error(format!(
                    "blank stage hierarchy contains {count} {type_name} records"
                )));
            }
        }
    }
    if missing.is_empty() {
        return Ok(false);
    }

    if !has_standard_manager_group(&document.root) {
        // As with alternate camera implementations, a custom authored archive
        // may intentionally omit Sunshine's standard Strategy manager group.
        return Ok(false);
    }
    let records = runtime_cube_manager_records()?
        .into_iter()
        .filter(|record| {
            missing
                .iter()
                .any(|type_name| *type_name == semantic_type_name(&record.type_name))
        })
        .collect();
    let JDramaRecordPayload::Group { children, .. } = &mut document.root.payload else {
        return Err(blank_stage_error(
            "blank stage scene root is not a group hierarchy",
        ));
    };
    // Append instead of inserting into manager group 2. Existing authored
    // overlays use semantic record byte offsets as stable identities, and an
    // append leaves every prior address unchanged. JDrama constructs the new
    // services during the load pass before any object's loadAfter callback.
    children.push(group(
        "GroupObj",
        "SMS Runtime Cube Managers",
        Vec::new(),
        records,
    )?);
    Ok(true)
}

fn has_standard_manager_group(record: &JDramaRecord) -> bool {
    let is_manager_group = matches!(
        &record.payload,
        JDramaRecordPayload::Group { fields, .. }
            if semantic_type_name(&record.type_name) == "IdxGroup"
                && fields.iter().any(|field| {
                    field.name == "group_index"
                        && field.value == JDramaFieldValue::U32(2)
                })
    );
    if is_manager_group {
        return true;
    }
    let JDramaRecordPayload::Group { children, .. } = &record.payload else {
        return false;
    };
    children.iter().any(has_standard_manager_group)
}

fn insert_camera_map_tool_table_before_polar(record: &mut JDramaRecord) -> Result<bool> {
    let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
        return Ok(false);
    };
    if let Some(index) = children
        .iter()
        .position(|child| semantic_type_name(&child.type_name) == "PolarSubCamera")
    {
        children.insert(index, camera_map_tool_table()?);
        return Ok(true);
    }
    for child in children {
        if insert_camera_map_tool_table_before_polar(child)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn indexed_group(index: u32, name: &str, children: Vec<JDramaRecord>) -> Result<JDramaRecord> {
    group(
        "IdxGroup",
        name,
        vec![field("group_index", JDramaFieldValue::U32(index))],
        children,
    )
}

fn group(
    type_name: &str,
    name: &str,
    fields: Vec<JDramaField>,
    children: Vec<JDramaRecord>,
) -> Result<JDramaRecord> {
    Ok(JDramaRecord::new(
        type_name,
        name,
        JDramaRecordPayload::Group { fields, children },
    )?)
}

fn fields_record(type_name: &str, name: &str, fields: Vec<JDramaField>) -> Result<JDramaRecord> {
    Ok(JDramaRecord::new(
        type_name,
        name,
        JDramaRecordPayload::Fields { fields },
    )?)
}

fn empty_record(type_name: &str, name: &str) -> Result<JDramaRecord> {
    Ok(JDramaRecord::new(
        type_name,
        name,
        JDramaRecordPayload::Empty,
    )?)
}

fn actor(
    type_name: &str,
    name: &str,
    transform: JDramaTransform,
    character_name: &str,
    fields: Vec<JDramaField>,
) -> Result<JDramaRecord> {
    Ok(JDramaRecord::new(
        type_name,
        name,
        JDramaRecordPayload::Actor {
            transform,
            character_name: character_name.to_string(),
            light_map: JDramaLightMap::default(),
            fields,
        },
    )?)
}

/// Creates the typed player actor used by Sunshine's player runtime group.
///
/// New authored stages intentionally do not contain this actor. The editor can
/// use this constructor when the user drops the `Mario` class into IdxGroup 6
/// so the record fields never drift from the blank-stage runtime contract.
pub fn blank_stage_mario_record(transform: JDramaTransform) -> Result<JDramaRecord> {
    validate_transform(transform)?;
    actor(
        "Mario",
        "\u{30DE}\u{30EA}\u{30AA}",
        transform,
        "\u{30DE}\u{30EA}\u{30AA} \u{30AD}\u{30E3}\u{30E9}",
        vec![
            field("starting_water", JDramaFieldValue::U32(100)),
            field("equipment_flags", JDramaFieldValue::U32(0)),
        ],
    )
}

/// Creates the typed sky actor for insertion into Sunshine's sky runtime
/// group. Supplying the matching `map/map/sky.*` resource bundle remains a
/// separate semantic resource edit.
pub fn blank_stage_sky_record(transform: JDramaTransform) -> Result<JDramaRecord> {
    validate_transform(transform)?;
    actor(
        "Sky",
        "\u{7A7A}",
        transform,
        "\u{7A7A} \u{30AD}\u{30E3}\u{30E9}",
        Vec::new(),
    )
}

fn field(name: &str, value: JDramaFieldValue) -> JDramaField {
    JDramaField {
        name: name.to_string(),
        value,
    }
}

fn canonical_placement_document(archive: &SourceFreeStageArchive) -> Result<&JDramaDocument> {
    archive
        .resources()
        .iter()
        .find(|resource| raw_path_eq_ignore_ascii_case(&resource.raw_path, b"map/scene.bin"))
        .and_then(|resource| match &resource.document {
            StageResourceDocument::Placement(document) => Some(document),
            _ => None,
        })
        .ok_or_else(|| {
            blank_stage_error("source stage has no typed canonical map/scene.bin placement")
        })
}

fn unique_record<'a>(
    document: &'a JDramaDocument,
    type_name: &str,
    label: &str,
) -> Result<&'a JDramaRecord> {
    let mut matches = Vec::new();
    collect_records_by_type(&document.root, type_name, &mut matches);
    match matches.as_slice() {
        [record] => Ok(*record),
        _ => Err(blank_stage_error(format!(
            "source stage requires exactly one typed {label}, found {}",
            matches.len()
        ))),
    }
}

fn collect_records_by_type<'a>(
    record: &'a JDramaRecord,
    type_name: &str,
    output: &mut Vec<&'a JDramaRecord>,
) {
    if semantic_type_name(&record.type_name) == type_name {
        output.push(record);
    }
    if let JDramaRecordPayload::Group { children, .. } = &record.payload {
        for child in children {
            collect_records_by_type(child, type_name, output);
        }
    }
}

fn semantic_type_name(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

fn record_light_map(record: &JDramaRecord) -> Result<&JDramaLightMap> {
    let JDramaRecordPayload::Group { fields, .. } = &record.payload else {
        return Err(blank_stage_error(
            "typed MarScene lighting source is not a group record",
        ));
    };
    let mut maps = fields.iter().filter_map(|field| match &field.value {
        JDramaFieldValue::LightMap(map) if field.name == "light_map" => Some(map),
        _ => None,
    });
    let Some(map) = maps.next() else {
        return Err(blank_stage_error(
            "typed MarScene lighting source has no light_map field",
        ));
    };
    if maps.next().is_some() {
        return Err(blank_stage_error(
            "typed MarScene lighting source has duplicate light_map fields",
        ));
    }
    Ok(map)
}

fn validate_lighting_group(
    record: &JDramaRecord,
    type_name: &str,
    runtime_name: &str,
    child_type: &str,
) -> Result<()> {
    if semantic_type_name(&record.type_name) != type_name || record.name != runtime_name {
        return Err(blank_stage_error(format!(
            "lighting preset is missing the runtime {runtime_name} {type_name} record"
        )));
    }
    let JDramaRecordPayload::Group { children, .. } = &record.payload else {
        return Err(blank_stage_error(format!(
            "lighting preset {runtime_name} is not a typed group"
        )));
    };
    if children.is_empty() {
        return Err(blank_stage_error(format!(
            "lighting preset {runtime_name} has no entries"
        )));
    }
    for child in children {
        let actual = semantic_type_name(&child.type_name);
        let type_matches = actual == child_type || (child_type == "Light" && actual == "IdxLight");
        if !type_matches || !matches!(&child.payload, JDramaRecordPayload::Fields { .. }) {
            return Err(blank_stage_error(format!(
                "lighting preset {runtime_name} contains non-{child_type} entry {:?}",
                child.type_name
            )));
        }
    }
    Ok(())
}

fn is_skybox_resource_path(raw_path: &[u8]) -> bool {
    let Some(separator) = raw_path.iter().rposition(|byte| *byte == b'/') else {
        return false;
    };
    if !raw_path_eq_ignore_ascii_case(&raw_path[..separator], b"map/map") {
        return false;
    }
    let file_name = &raw_path[separator + 1..];
    let Some(extension) = file_name.iter().rposition(|byte| *byte == b'.') else {
        return false;
    };
    extension > 0
        && extension + 1 < file_name.len()
        && matches_ignore_ascii_case(&file_name[..extension], &[b"sky".as_slice(), b"reflectsky"])
}

fn raw_path_eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn identity_transform() -> JDramaTransform {
    JDramaTransform {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0; 3],
    }
}

fn blank_world_model() -> Result<J3dRebuildDocument> {
    let vertices = [
        StaticModelVertex::new([-1.0, -1_000_000.0, -1.0], [0.0, 1.0, 0.0]),
        StaticModelVertex::new([1.0, -1_000_000.0, -1.0], [0.0, 1.0, 0.0]),
        StaticModelVertex::new([0.0, -1_000_000.0, 1.0], [0.0, 1.0, 0.0]),
    ];
    Ok(compile_static_bmd3(&StaticModel {
        root_joint_name: "map".to_string(),
        meshes: vec![StaticModelMesh {
            name: "empty_stage_placeholder".to_string(),
            material_index: 0,
            // Sunshine's map model loader requires a real canonical BMD, and
            // the source-free compiler rejects zero-area geometry. This tiny
            // valid triangle is placed far below the editable world instead.
            vertices: vertices.to_vec(),
            triangles: vec![[0, 1, 2]],
        }],
        materials: vec![GxMaterial::default()],
        textures: Vec::new(),
    })?)
}

fn validate_transform(transform: JDramaTransform) -> Result<()> {
    if transform
        .translation
        .into_iter()
        .chain(transform.rotation)
        .chain(transform.scale)
        .all(f32::is_finite)
    {
        Ok(())
    } else {
        Err(blank_stage_error(
            "spawn transform contains non-finite values",
        ))
    }
}

fn validate_target_slot(target_slot: &str) -> Result<()> {
    if target_slot.is_empty()
        || !target_slot
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Err(blank_stage_error(format!(
            "invalid authored stage id {target_slot:?}"
        )))
    } else {
        Ok(())
    }
}

fn normalize_bootstrap_path(mut raw_path: Vec<u8>) -> Result<Vec<u8>> {
    if raw_path.first() == Some(&b'/') {
        raw_path.remove(0);
    }
    if raw_path.is_empty()
        || raw_path.contains(&0)
        || raw_path
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || matches!(component, b"." | b".."))
    {
        Err(blank_stage_error(format!(
            "invalid bootstrap archive path {raw_path:02X?}"
        )))
    } else {
        Ok(raw_path)
    }
}

fn display_raw_path(path: &[u8]) -> String {
    path.iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn blank_stage_error(message: impl Into<String>) -> SceneError {
    SceneError::Format(FormatError::Unsupported {
        format: "blank stage preset",
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use sms_formats::{
        ColGroup, ColTriangle, ColVertex, J3dRebuildDocument, JDramaLightMapEntry,
        JDramaRecordPayload,
    };

    use super::*;

    fn empty_model() -> J3dRebuildDocument {
        J3dRebuildDocument {
            file_type: *b"bmd3",
            version_tag: *b"SVR3",
            reserved_words: [u32::MAX; 3],
            declared_section_count: 0,
            sections: Vec::new(),
        }
    }

    fn empty_material_table() -> J3dRebuildDocument {
        J3dRebuildDocument {
            file_type: *b"bmt3",
            ..empty_model()
        }
    }

    fn floor_collision() -> ColFile {
        ColFile::new(
            vec![
                ColVertex::new(-100.0, 0.0, -100.0),
                ColVertex::new(0.0, 0.0, 100.0),
                ColVertex::new(100.0, 0.0, -100.0),
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
    }

    fn bootstrap() -> BlankStageBootstrapManifest {
        let model = empty_model().to_bytes().unwrap();
        let collision = floor_collision().encode().unwrap();
        BlankStageBootstrapManifest::from_authored_bytes(BLANK_STAGE_BOOTSTRAP_REQUIREMENTS.map(
            |requirement| BlankStageBootstrapResource {
                raw_path: requirement.raw_path.to_vec(),
                bytes: match requirement.kind {
                    BlankStageBootstrapKind::Model => model.clone(),
                    BlankStageBootstrapKind::Collision => collision.clone(),
                },
            },
        ))
        .unwrap()
    }

    fn collect_type_names<'a>(record: &'a JDramaRecord, output: &mut Vec<&'a str>) {
        output.push(&record.type_name);
        if let JDramaRecordPayload::Group { children, .. } = &record.payload {
            for child in children {
                collect_type_names(child, output);
            }
        }
    }

    fn first_record_mut<'a>(
        record: &'a mut JDramaRecord,
        type_name: &str,
    ) -> Option<&'a mut JDramaRecord> {
        if semantic_type_name(&record.type_name) == type_name {
            return Some(record);
        }
        let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return None;
        };
        children
            .iter_mut()
            .find_map(|child| first_record_mut(child, type_name))
    }

    fn parent_children<'a>(
        record: &'a JDramaRecord,
        child_type_name: &str,
    ) -> Option<&'a [JDramaRecord]> {
        let JDramaRecordPayload::Group { children, .. } = &record.payload else {
            return None;
        };
        if children
            .iter()
            .any(|child| semantic_type_name(&child.type_name) == child_type_name)
        {
            return Some(children);
        }
        children
            .iter()
            .find_map(|child| parent_children(child, child_type_name))
    }

    fn remove_records_by_type(record: &mut JDramaRecord, type_name: &str) {
        let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return;
        };
        children.retain(|child| semantic_type_name(&child.type_name) != type_name);
        for child in children {
            remove_records_by_type(child, type_name);
        }
    }

    fn indexed_group_children_mut(
        record: &mut JDramaRecord,
        group_index: u32,
    ) -> Option<&mut Vec<JDramaRecord>> {
        let is_match = matches!(
            &record.payload,
            JDramaRecordPayload::Group { fields, .. }
                if semantic_type_name(&record.type_name) == "IdxGroup"
                    && fields.iter().any(|field| {
                        field.name == "group_index"
                            && field.value == JDramaFieldValue::U32(group_index)
                    })
        );
        if is_match {
            let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
                unreachable!()
            };
            return Some(children);
        }
        let JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return None;
        };
        children
            .iter_mut()
            .find_map(|child| indexed_group_children_mut(child, group_index))
    }

    fn placement_mut(archive: &mut SourceFreeStageArchive) -> &mut JDramaDocument {
        let StageResourceDocument::Placement(document) =
            archive.resource_mut(b"map/scene.bin").unwrap()
        else {
            panic!("scene.bin is not placement data")
        };
        document
    }

    fn add_skybox_fixture(archive: &mut SourceFreeStageArchive) {
        indexed_group_children_mut(&mut placement_mut(archive).root, 1)
            .unwrap()
            .push(
                actor(
                    "Sky",
                    "\u{7A7A}",
                    JDramaTransform {
                        translation: [1_000.0, 2_000.0, 3_000.0],
                        rotation: [0.0, 45.0, 0.0],
                        scale: [1.0; 3],
                    },
                    "\u{7A7A} \u{30AD}\u{30E3}\u{30E9}",
                    Vec::new(),
                )
                .unwrap(),
            );
        archive
            .insert_resource(
                b"map/map/sky.bmd".to_vec(),
                StageResourceDocument::Model(empty_model()),
            )
            .unwrap();
        archive
            .insert_resource(
                b"map/map/sky.bmt".to_vec(),
                StageResourceDocument::Model(empty_material_table()),
            )
            .unwrap();
    }

    fn assert_no_mojibake(record: &JDramaRecord) {
        assert!(!record.type_name.contains('\u{00E3}'));
        assert!(!record.name.contains('\u{00E3}'), "{:?}", record.name);
        let (fields, children): (&[JDramaField], &[JDramaRecord]) = match &record.payload {
            JDramaRecordPayload::Empty => (&[], &[]),
            JDramaRecordPayload::Fields { fields } => (fields, &[]),
            JDramaRecordPayload::Actor {
                character_name,
                fields,
                ..
            } => {
                assert!(!character_name.contains('\u{00E3}'), "{character_name:?}");
                (fields, &[])
            }
            JDramaRecordPayload::Group { fields, children } => (fields, children),
        };
        for field in fields {
            if let JDramaFieldValue::String(value) = &field.value {
                assert!(!value.contains('\u{00E3}'), "{value:?}");
            }
        }
        for child in children {
            assert_no_mojibake(child);
        }
    }

    #[test]
    fn blank_stage_builds_and_reimports_deterministically() {
        let preset = BlankStagePreset::default();
        let archive = preset.build(bootstrap()).unwrap();
        assert_eq!(
            archive.origin(),
            &crate::StageOrigin::Blank {
                target_slot: "test11".to_string(),
                preset_version: BLANK_STAGE_PRESET_VERSION,
            }
        );
        for path in [
            b"map/scene.bin".as_slice(),
            b"map/map/map.bmd",
            b"map/map.col",
        ] {
            assert!(archive.resource(path).is_some(), "missing {path:02X?}");
        }
        for requirement in BLANK_STAGE_BOOTSTRAP_REQUIREMENTS {
            assert!(archive.resource(requirement.raw_path).is_some());
        }
        for optional in [
            b"map/scene.ral".as_slice(),
            b"map/tables.bin",
            b"map/startcamera.bck",
            b"map/ymap.ymp",
        ] {
            assert!(archive.resource(optional).is_none());
        }

        let first = archive.encode().unwrap();
        let second = archive.encode().unwrap();
        assert_eq!(first, second);
        let reopened = SourceFreeStageArchive::parse(&first).unwrap();
        assert_eq!(reopened.encode().unwrap(), first);
    }

    #[test]
    fn skybox_preset_clones_actor_and_complete_typed_resource_bundle() {
        let mut source = BlankStagePreset::default().build(bootstrap()).unwrap();
        add_skybox_fixture(&mut source);
        let skybox = BlankStageSkyboxPreset::from_archive(&source).unwrap();
        assert_eq!(
            skybox
                .resources()
                .iter()
                .map(|resource| resource.raw_path.as_slice())
                .collect::<Vec<_>>(),
            [
                b"map/map/reflectsky.bmd".as_slice(),
                b"map/map/sky.bmd",
                b"map/map/sky.bmt",
            ]
        );

        let target = BlankStagePreset {
            skybox: Some(skybox.clone()),
            ..BlankStagePreset::default()
        }
        .build(bootstrap())
        .unwrap();
        assert_eq!(
            BlankStageSkyboxPreset::from_archive(&target).unwrap(),
            skybox
        );
        let StageResourceDocument::Placement(scene) = target.resource(b"map/scene.bin").unwrap()
        else {
            panic!("scene.bin is not placement data")
        };
        let mut scene = scene.clone();
        let sky_group = indexed_group_children_mut(&mut scene.root, 1).unwrap();
        assert_eq!(sky_group.len(), 1);
        assert_eq!(semantic_type_name(&sky_group[0].type_name), "Sky");

        let encoded = target.encode().unwrap();
        let reopened = SourceFreeStageArchive::parse(&encoded).unwrap();
        assert_eq!(
            BlankStageSkyboxPreset::from_archive(&reopened).unwrap(),
            skybox
        );
        assert_eq!(reopened.encode().unwrap(), encoded);
    }

    #[test]
    fn skybox_preset_rejects_partial_actor_or_resource_sources() {
        let mut resource_only = BlankStagePreset::default().build(bootstrap()).unwrap();
        resource_only
            .insert_resource(
                b"map/map/sky.bmd".to_vec(),
                StageResourceDocument::Model(empty_model()),
            )
            .unwrap();
        let error = BlankStageSkyboxPreset::from_archive(&resource_only)
            .unwrap_err()
            .to_string();
        assert!(error.contains("sky actor"), "{error}");

        let mut actor_only = BlankStagePreset::default().build(bootstrap()).unwrap();
        indexed_group_children_mut(&mut placement_mut(&mut actor_only).root, 1)
            .unwrap()
            .push(
                actor(
                    "Sky",
                    "\u{7A7A}",
                    identity_transform(),
                    "\u{7A7A} \u{30AD}\u{30E3}\u{30E9}",
                    Vec::new(),
                )
                .unwrap(),
            );
        let error = BlankStageSkyboxPreset::from_archive(&actor_only)
            .unwrap_err()
            .to_string();
        assert!(error.contains("map/map/sky.bmd"), "{error}");
    }

    #[test]
    fn lighting_preset_clones_ordered_groups_scene_map_and_sun_manager() {
        let mut source = BlankStagePreset::default().build(bootstrap()).unwrap();
        {
            let placement = placement_mut(&mut source);
            let light_group = first_record_mut(&mut placement.root, "LightAry").unwrap();
            let JDramaRecordPayload::Group { children, .. } = &mut light_group.payload else {
                panic!("LightAry is not a group")
            };
            let JDramaRecordPayload::Fields { fields } = &mut children[0].payload else {
                panic!("Light is not typed fields")
            };
            fields
                .iter_mut()
                .find(|field| field.name == "range")
                .unwrap()
                .value = JDramaFieldValue::F32(321.5);

            let ambient_group = first_record_mut(&mut placement.root, "AmbAry").unwrap();
            let JDramaRecordPayload::Group { children, .. } = &mut ambient_group.payload else {
                panic!("AmbAry is not a group")
            };
            let JDramaRecordPayload::Fields { fields } = &mut children[0].payload else {
                panic!("AmbColor is not typed fields")
            };
            fields[0].value = JDramaFieldValue::ColorRgba8([9, 8, 7, 255]);

            let mar_scene = first_record_mut(&mut placement.root, "MarScene").unwrap();
            let JDramaRecordPayload::Group { fields, .. } = &mut mar_scene.payload else {
                panic!("MarScene is not a group")
            };
            let JDramaFieldValue::LightMap(light_map) = &mut fields
                .iter_mut()
                .find(|field| field.name == "light_map")
                .unwrap()
                .value
            else {
                panic!("MarScene light_map has the wrong type")
            };
            light_map.entries.push(JDramaLightMapEntry {
                channel: 3,
                light_name: "\u{592A}\u{967D}\u{FF08}\u{30AA}\u{30D6}\u{30B8}\u{30A7}\u{30AF}\u{30C8}\u{FF09}".to_string(),
            });

            let sun_manager = first_record_mut(&mut placement.root, "SunMgr").unwrap();
            let JDramaRecordPayload::Fields { fields } = &mut sun_manager.payload else {
                panic!("SunMgr is not typed fields")
            };
            fields
                .iter_mut()
                .find(|field| field.name == "sun_color_r")
                .unwrap()
                .value = JDramaFieldValue::U32(0x1234);
        }

        let lighting = BlankStageLightingPreset::from_archive(&source).unwrap();
        assert_eq!(lighting.mar_scene_light_map().entries.len(), 1);
        let target = BlankStagePreset {
            lighting: Some(lighting.clone()),
            ..BlankStagePreset::default()
        }
        .build(bootstrap())
        .unwrap();
        assert_eq!(
            BlankStageLightingPreset::from_archive(&target).unwrap(),
            lighting
        );

        let encoded = target.encode().unwrap();
        let reopened = SourceFreeStageArchive::parse(&encoded).unwrap();
        assert_eq!(
            BlankStageLightingPreset::from_archive(&reopened).unwrap(),
            lighting
        );
        assert_eq!(reopened.encode().unwrap(), encoded);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn retail_cross_level_skybox_and_lighting_build_one_source_free_stage() {
        let base_root = std::env::var_os("SMS_BASE_ROOT").expect("set SMS_BASE_ROOT");
        let archives = sms_formats::discover_scene_archives(&base_root).unwrap();
        let load = |stage_id: &str| {
            let source = archives
                .iter()
                .find(|archive| archive.stage_id.eq_ignore_ascii_case(stage_id))
                .unwrap_or_else(|| panic!("missing retail stage {stage_id}"));
            SourceFreeStageArchive::parse(&std::fs::read(&source.path).unwrap()).unwrap()
        };

        let skybox = BlankStageSkyboxPreset::from_archive(&load("dolpic0")).unwrap();
        let lighting = BlankStageLightingPreset::from_archive(&load("bianco0")).unwrap();
        let authored = BlankStagePreset {
            skybox: Some(skybox),
            lighting: Some(lighting),
            ..BlankStagePreset::default()
        }
        .build(bootstrap())
        .unwrap();

        let encoded = authored.encode().unwrap();
        let reopened = SourceFreeStageArchive::parse(&encoded).unwrap();
        assert!(BlankStageSkyboxPreset::from_archive(&reopened).is_ok());
        assert!(BlankStageLightingPreset::from_archive(&reopened).is_ok());
        assert_eq!(reopened.encode().unwrap(), encoded);
    }

    #[test]
    fn serialized_legacy_preset_defaults_environment_selections() {
        let preset: BlankStagePreset =
            serde_json::from_str(r#"{"target_slot":"test11","spawn":null,"compression":null}"#)
                .unwrap();
        assert!(preset.skybox.is_none());
        assert!(preset.lighting.is_none());
    }

    #[test]
    fn blank_scene_contains_required_typed_runtime_skeleton() {
        let archive = BlankStagePreset::default().build(bootstrap()).unwrap();
        let StageResourceDocument::Placement(scene) = archive.resource(b"map/scene.bin").unwrap()
        else {
            panic!("scene.bin is not placement data");
        };
        let reparsed = JDramaDocument::parse(&scene.to_bytes().unwrap()).unwrap();
        assert_eq!(reparsed, *scene);
        assert_no_mojibake(&reparsed.root);
        let mut types = Vec::new();
        collect_type_names(&scene.root, &mut types);
        for required in [
            "ItemManager",
            "MapObjManager",
            "MirrorCamera",
            "MirrorModelManager",
            "MarScene",
            "AmbAry",
            "LightAry",
            "Strategy",
            "Map",
            "SunMgr",
            "CubeCamera",
            "CubeMirror",
            "CubeWire",
            "CubeStream",
            "CubeShadow",
            "CubeArea",
            "CubeFastA",
            "CubeFastB",
            "CubeFastC",
            "CubeSoundChange",
            "CubeSoundEffect",
            "MapWireManager",
            "Pollution",
            "CameraMapToolTable",
            "PolarSubCamera",
        ] {
            assert!(types.contains(&required), "missing {required}");
        }
        let camera_children = parent_children(&scene.root, "PolarSubCamera").unwrap();
        let table_index = camera_children
            .iter()
            .position(|record| semantic_type_name(&record.type_name) == "CameraMapToolTable")
            .unwrap();
        let camera_index = camera_children
            .iter()
            .position(|record| semantic_type_name(&record.type_name) == "PolarSubCamera")
            .unwrap();
        assert!(table_index < camera_index);
        assert!(matches!(
            &camera_children[table_index].payload,
            JDramaRecordPayload::Group { fields, children }
                if fields.is_empty() && children.is_empty()
        ));
        assert!(!types.contains(&"Mario"));
        assert!(!types.contains(&"Sky"));
        assert_eq!(types.iter().filter(|name| **name == "IdxGroup").count(), 12);
    }

    #[test]
    fn mario_is_absent_by_default_and_can_be_explicitly_authored() {
        let empty = BlankStagePreset::default().build(bootstrap()).unwrap();
        assert!(empty
            .object_placements()
            .iter()
            .all(|placement| placement.type_name != "Mario"));
        let preset = BlankStagePreset {
            spawn: Some(JDramaTransform {
                translation: [10.0, 20.0, 30.0],
                rotation: [0.0, 90.0, 0.0],
                scale: [1.0; 3],
            }),
            ..BlankStagePreset::default()
        };
        let archive = preset.build(bootstrap()).unwrap();
        let mario = archive
            .object_placements()
            .into_iter()
            .find(|placement| placement.type_name == "Mario")
            .unwrap();
        assert_eq!(mario.transform.translation, [10.0, 20.0, 30.0]);
        assert_eq!(mario.transform.rotation, [0.0, 90.0, 0.0]);
        assert_eq!(mario.transform.scale, [1.0; 3]);
    }

    #[test]
    fn empty_stage_uses_hidden_canonical_map_resources() {
        let archive = BlankStagePreset::default().build(bootstrap()).unwrap();
        let StageResourceDocument::Model(model) = archive.resource(b"map/map/map.bmd").unwrap()
        else {
            panic!("map model is not typed")
        };
        sms_formats::validate_canonical_bmd3(model).unwrap();
        let preview = sms_formats::J3dFile::parse(model.to_bytes().unwrap())
            .unwrap()
            .geometry_preview()
            .unwrap();
        assert_eq!(preview.triangles.len(), 1);
        assert!(preview.triangles[0]
            .vertices
            .iter()
            .all(|position| position[1] == -1_000_000.0));

        let StageResourceDocument::Collision(collision) = archive.resource(b"map/map.col").unwrap()
        else {
            panic!("map collision is not typed")
        };
        assert!(collision.vertices().is_empty());
        assert!(collision.groups().is_empty());

        let StageResourceDocument::Model(camera_directory_marker) = archive
            .resource(BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH)
            .unwrap()
        else {
            panic!("camera directory marker is not typed")
        };
        sms_formats::validate_canonical_bmd3(camera_directory_marker).unwrap();

        let StageResourceDocument::Particle(coin_particle) =
            archive.resource(BLANK_STAGE_COIN_PARTICLE_PATH).unwrap()
        else {
            panic!("coin particle dependency is not typed")
        };
        assert_eq!(coin_particle, &JpaxDocument::authored_noop());

        let encoded = archive.encode().unwrap();
        let reopened = SourceFreeStageArchive::parse(&encoded).unwrap();
        assert!(reopened
            .resource(BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH)
            .is_some());
        assert!(reopened.resource(BLANK_STAGE_COIN_PARTICLE_PATH).is_some());
    }

    #[test]
    fn older_blank_stage_baselines_gain_required_runtime_resources() {
        let mut archive = BlankStagePreset {
            spawn: Some(identity_transform()),
            ..BlankStagePreset::default()
        }
        .build(bootstrap())
        .unwrap();
        archive
            .remove_resource(BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH)
            .unwrap();
        archive
            .remove_resource(BLANK_STAGE_COIN_PARTICLE_PATH)
            .unwrap();
        let StageResourceDocument::Placement(placement) =
            archive.resource_mut(b"map/scene.bin").unwrap()
        else {
            panic!("scene.bin is not placement data")
        };
        remove_records_by_type(&mut placement.root, "CameraMapToolTable");
        for type_name in RUNTIME_CUBE_MANAGER_TYPES {
            remove_records_by_type(&mut placement.root, type_name);
        }
        let before_scene = archive.resource_bytes(b"map/scene.bin").unwrap().unwrap();
        let mario_offset_before = before_scene
            .windows(b"Mario".len())
            .position(|window| window == b"Mario")
            .unwrap();
        archive.set_origin(crate::StageOrigin::Blank {
            target_slot: "test11".to_string(),
            preset_version: 2,
        });

        assert!(ensure_blank_stage_runtime_resources(&mut archive).unwrap());
        assert!(archive
            .resource(BLANK_STAGE_CAMERA_DIRECTORY_MARKER_PATH)
            .is_some());
        assert_eq!(
            archive.resource(BLANK_STAGE_COIN_PARTICLE_PATH),
            Some(&StageResourceDocument::Particle(
                JpaxDocument::authored_noop()
            ))
        );
        assert_eq!(
            archive.origin(),
            &crate::StageOrigin::Blank {
                target_slot: "test11".to_string(),
                preset_version: BLANK_STAGE_PRESET_VERSION,
            }
        );
        let StageResourceDocument::Placement(placement) =
            archive.resource(b"map/scene.bin").unwrap()
        else {
            panic!("scene.bin is not placement data")
        };
        let camera_children = parent_children(&placement.root, "PolarSubCamera").unwrap();
        assert_eq!(
            camera_children
                .iter()
                .filter(|record| { semantic_type_name(&record.type_name) == "CameraMapToolTable" })
                .count(),
            1
        );
        let mut types = Vec::new();
        collect_type_names(&placement.root, &mut types);
        for type_name in RUNTIME_CUBE_MANAGER_TYPES {
            assert_eq!(
                types
                    .iter()
                    .filter(|record_type| semantic_type_name(record_type) == type_name)
                    .count(),
                1,
                "missing or duplicate migrated {type_name}"
            );
        }
        let after_scene = archive.resource_bytes(b"map/scene.bin").unwrap().unwrap();
        let mario_offset_after = after_scene
            .windows(b"Mario".len())
            .position(|window| window == b"Mario")
            .unwrap();
        assert_eq!(mario_offset_after, mario_offset_before);
        assert!(!ensure_blank_stage_runtime_resources(&mut archive).unwrap());
    }

    #[test]
    fn explicit_world_baseline_remains_available_for_automation() {
        let collision = floor_collision();
        let archive = BlankStagePreset::default()
            .build_with_world(empty_model(), collision.clone(), bootstrap())
            .unwrap();
        assert_eq!(
            archive.resource(b"map/map.col"),
            Some(&StageResourceDocument::Collision(collision))
        );
    }

    #[test]
    fn bootstrap_manifest_is_complete_typed_and_source_detached() {
        let model = empty_model().to_bytes().unwrap();
        let collision = floor_collision().encode().unwrap();
        let mut source =
            BLANK_STAGE_BOOTSTRAP_REQUIREMENTS.map(|requirement| BlankStageBootstrapResource {
                raw_path: requirement.raw_path.to_vec(),
                bytes: match requirement.kind {
                    BlankStageBootstrapKind::Model => model.clone(),
                    BlankStageBootstrapKind::Collision => collision.clone(),
                },
            });
        let manifest = BlankStageBootstrapManifest::from_authored_bytes(source.clone()).unwrap();
        for resource in &mut source {
            resource.bytes.fill(0xA5);
        }
        let archive = BlankStagePreset::default().build(manifest).unwrap();
        let encoded = archive.encode().unwrap();
        assert_eq!(
            SourceFreeStageArchive::parse(&encoded)
                .unwrap()
                .encode()
                .unwrap(),
            encoded
        );

        let missing = BLANK_STAGE_BOOTSTRAP_REQUIREMENTS[..4]
            .iter()
            .map(|requirement| BlankStageBootstrapResource {
                raw_path: requirement.raw_path.to_vec(),
                bytes: match requirement.kind {
                    BlankStageBootstrapKind::Model => model.clone(),
                    BlankStageBootstrapKind::Collision => collision.clone(),
                },
            });
        assert!(BlankStageBootstrapManifest::from_authored_bytes(missing).is_err());
    }

    #[test]
    fn authored_stage_metadata_requires_a_new_stage_table_entry() {
        let metadata = BlankStagePreset::default().target_metadata().unwrap();
        assert_eq!(metadata.target_slot, "test11");
        assert_eq!(metadata.output_archive_name, "test11.szs");
        assert!(!metadata.replaces_existing_stage_mapping);
        assert!(metadata.stage_table_entry_required);
        assert!(!metadata.runtime_patch_required);
    }
}
