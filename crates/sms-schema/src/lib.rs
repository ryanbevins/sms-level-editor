//! Decomp-derived object and parameter registry generation.

mod factory_extractor;
mod map_obj_ball_extractor;
mod map_obj_resource_extractor;
mod map_obj_shared_model_extractor;
mod map_obj_stream_tev_extractor;
mod map_obj_string_tev_extractor;
mod source_inventory;
mod stage_name_extractor;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use factory_extractor::{extract_factory_candidates, FactoryEvidence};
use map_obj_ball_extractor::extract_map_obj_ball_transforms;
use map_obj_resource_extractor::{extract_map_obj_resources, has_null_animation_model_fallback};
use map_obj_shared_model_extractor::{
    extract_direct_make_mactors_overrides, extract_map_obj_shared_models,
    extract_shared_model_loaders,
};
use map_obj_stream_tev_extractor::extract_map_obj_stream_tev_colors;
use map_obj_string_tev_extractor::extract_nozzle_box_tev_program;
use source_inventory::SourceInventory;
pub use stage_name_extractor::{extract_stage_name_tables, StageNameTables};

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("schema source is missing: {0}")]
    MissingSource(PathBuf),
    #[error("failed to traverse schema source tree {root}: {source}")]
    SourceTraversal {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("failed to read schema source {path}: {source}")]
    SourceRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("schema source inventory contains no C/C++ files: {0}")]
    EmptySourceInventory(PathBuf),
    #[error("{extractor} extraction produced no {expected} from required source {source_path}")]
    ExtractionDrift {
        extractor: SchemaExtractor,
        source_path: PathBuf,
        expected: &'static str,
    },
    #[error("generated registry violates invariant: {detail}")]
    RegistryInvariant { detail: String },
}

pub type Result<T> = std::result::Result<T, SchemaError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaExtractor {
    FactoryRegistration,
    ModelLoaderFlags,
    EnemyModelData,
    Params,
    AssetHints,
    MapStaticModels,
    MapObjResources,
    MapObjBallTransforms,
    MapObjModelOverrides,
    MapObjStringTevPrograms,
    MapObjStreamTevColors,
    MapObjFactoryTypes,
    MapObjFlag,
    ParticleResources,
    ParticleBindings,
    NpcResources,
    NpcInitData,
    NpcRootColors,
    StageNames,
    CollisionSurfaces,
    CollisionLimits,
}

impl std::fmt::Display for SchemaExtractor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::FactoryRegistration => "factory registration",
            Self::ModelLoaderFlags => "model loader flags",
            Self::EnemyModelData => "enemy model data",
            Self::Params => "parameter metadata",
            Self::AssetHints => "asset hint",
            Self::MapStaticModels => "map-static model",
            Self::MapObjResources => "map-object resource",
            Self::MapObjBallTransforms => "map-object ball transform",
            Self::MapObjModelOverrides => "map-object model override",
            Self::MapObjStringTevPrograms => "map-object string TEV program",
            Self::MapObjStreamTevColors => "map-object stream TEV color",
            Self::MapObjFactoryTypes => "map-object factory type",
            Self::MapObjFlag => "map-object flag",
            Self::ParticleResources => "particle resource",
            Self::ParticleBindings => "particle binding",
            Self::NpcResources => "NPC model resource",
            Self::NpcInitData => "NPC initialization",
            Self::NpcRootColors => "NPC root color",
            Self::StageNames => "stage name",
            Self::CollisionSurfaces => "collision surface",
            Self::CollisionLimits => "collision runtime limit",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObjectRegistry {
    pub objects: Vec<ObjectDefinition>,
    pub params: Vec<ParamDefinition>,
    pub asset_hints: Vec<AssetHint>,
    /// Decomp-derived primary model resources for actor factory names.
    #[serde(default)]
    pub object_resources: Vec<ObjectResourceBinding>,
    #[serde(default)]
    pub map_static_models: Vec<MapStaticModelDefinition>,
    /// Models selected by the exact resource identity stored in `TMapObjData::unk0`.
    #[serde(default)]
    pub map_obj_resources: Vec<MapObjResourceDefinition>,
    /// Runtime shared-model overrides selected by exact resource and actor class.
    #[serde(default)]
    pub map_obj_model_overrides: Vec<MapObjModelOverrideDefinition>,
    /// Runtime string-selected material-color programs for typed map objects.
    #[serde(default)]
    pub map_obj_string_tev_programs: Vec<MapObjStringTevProgramDefinition>,
    /// Runtime material colors read from fixed-width fields at the end of an actor stream.
    #[serde(default)]
    pub map_obj_stream_tev_colors: Vec<MapObjStreamTevColorDefinition>,
    /// Runtime body-radius and model-matrix corrections selected by exact actor type.
    #[serde(default)]
    pub map_obj_ball_transforms: Vec<MapObjBallTransformDefinition>,
    /// Exact registered factories whose decomp classes inherit `TMapObjBase`.
    #[serde(default)]
    pub map_obj_factories: Vec<String>,
    #[serde(default)]
    pub particle_resources: Vec<ParticleResourceDefinition>,
    #[serde(default)]
    pub actor_particle_bindings: Vec<ActorParticleBinding>,
    #[serde(default)]
    pub npc_actors: Vec<NpcActorDefinition>,
    /// Root-model color programs selected by NPC instance body/cloth indices.
    #[serde(default)]
    pub npc_material_colors: Vec<NpcMaterialColorDefinition>,
    #[serde(default)]
    pub enemy_managers: Vec<EnemyManagerDefinition>,
    /// Stage-local animation folders opened directly by enemy manager
    /// createAnmData overrides instead of through registered character data.
    #[serde(default)]
    pub enemy_manager_animation_folders: Vec<EnemyManagerAnimationFolderDefinition>,
    /// Map-object slots instantiated directly by actor and manager runtime methods.
    #[serde(default)]
    pub runtime_map_obj_dependencies: Vec<RuntimeMapObjDependencyDefinition>,
    /// Fixed JDrama name lookups issued by actor runtime methods.
    ///
    /// These are extracted from decomp callsites so authoring can expose actor
    /// links and transplant support records without per-object tables.
    #[serde(default)]
    pub runtime_name_references: Vec<RuntimeNameReferenceDefinition>,
    #[serde(default)]
    pub enemy_actors: Vec<EnemyActorDefinition>,
    #[serde(default)]
    pub enemy_material_colors: Vec<EnemyMaterialTevColorDefinition>,
    #[serde(default)]
    pub map_obj_flags: Vec<MapObjFlagDefinition>,
    /// Collision type and property-flag names extracted from `BGTypeBits`.
    #[serde(default)]
    pub collision_surfaces: Vec<CollisionSurfaceDefinition>,
    /// Stack-backed transform capacity used by moving map collision.
    #[serde(default)]
    pub moving_collision_vertex_limit: Option<u16>,
}

impl ObjectRegistry {
    pub fn find_object(&self, factory_name: &str) -> Option<&ObjectDefinition> {
        self.objects
            .iter()
            .find(|object| object.factory_name == factory_name)
    }

    pub fn find_npc_actor(&self, factory_name: &str) -> Option<&NpcActorDefinition> {
        let actor_key = factory_name.strip_prefix("NPC")?;
        self.npc_actors
            .iter()
            .filter(|definition| actor_key.starts_with(&definition.actor_key))
            .max_by_key(|definition| definition.actor_key.len())
    }

    pub fn find_enemy_manager(&self, factory_name: &str) -> Option<&EnemyManagerDefinition> {
        self.enemy_managers
            .iter()
            .find(|definition| definition.factory_name == factory_name)
    }

    pub fn find_enemy_actor(&self, factory_name: &str) -> Option<&EnemyActorDefinition> {
        self.enemy_actors
            .iter()
            .find(|definition| definition.factory_name == factory_name)
    }

    pub fn object_resources_for<'a>(
        &'a self,
        factory_name: &str,
    ) -> impl Iterator<Item = &'a ObjectResourceBinding> + 'a {
        let factory_name = factory_name.to_string();
        self.object_resources
            .iter()
            .filter(move |definition| definition.factory_name == factory_name)
    }

    pub fn primary_object_resource(&self, factory_name: &str) -> Option<&ObjectResourceBinding> {
        self.object_resources_for(factory_name)
            .find(|definition| definition.role == ObjectResourceRole::Primary)
    }

    /// Looks up a map-object resource using the case-sensitive identity consumed by retail.
    pub fn find_map_obj_resource(&self, resource_name: &str) -> Option<&MapObjResourceDefinition> {
        self.map_obj_resources
            .iter()
            .find(|definition| definition.resource_name == resource_name)
    }

    pub fn is_map_obj_factory(&self, factory_name: &str) -> bool {
        self.map_obj_factories
            .iter()
            .any(|candidate| candidate == factory_name)
    }

    pub fn find_map_obj_model_override(
        &self,
        factory_name: &str,
        resource_name: &str,
    ) -> Option<&MapObjModelOverrideDefinition> {
        let class_name = &self.find_object(factory_name)?.class_name;
        self.map_obj_model_overrides.iter().find(|definition| {
            definition.resource_name == resource_name && definition.class_name == *class_name
        })
    }

    pub fn find_map_obj_string_tev_program(
        &self,
        factory_name: &str,
        resource_name: &str,
    ) -> Option<&MapObjStringTevProgramDefinition> {
        let class_name = &self.find_object(factory_name)?.class_name;
        self.map_obj_string_tev_programs.iter().find(|definition| {
            definition.resource_name == resource_name && definition.class_name == *class_name
        })
    }

    pub fn find_map_obj_stream_tev_color(
        &self,
        factory_name: &str,
    ) -> Option<&MapObjStreamTevColorDefinition> {
        let class_name = &self.find_object(factory_name)?.class_name;
        self.map_obj_stream_tev_colors
            .iter()
            .find(|definition| definition.class_name == *class_name)
    }

    pub fn find_map_obj_ball_transform(
        &self,
        actor_type: u32,
    ) -> Option<&MapObjBallTransformDefinition> {
        self.map_obj_ball_transforms
            .iter()
            .find(|definition| definition.actor_type == actor_type)
    }

    pub fn npc_material_colors_for<'a>(
        &'a self,
        factory_name: &str,
    ) -> impl Iterator<Item = &'a NpcMaterialColorDefinition> + 'a {
        let actor_key = self
            .find_npc_actor(factory_name)
            .map(|definition| definition.actor_key.as_str());
        self.npc_material_colors.iter().filter(move |definition| {
            actor_key.is_some_and(|actor_key| definition.actor_key == actor_key)
        })
    }

    pub fn apply_overlay(&mut self, overlay: SchemaOverlay) {
        let mut by_name: BTreeMap<String, ObjectOverlay> = overlay
            .objects
            .into_iter()
            .map(|object| (object.factory_name.clone(), object))
            .collect();

        for object in &mut self.objects {
            if let Some(overlay) = by_name.remove(&object.factory_name) {
                if let Some(class_name) = overlay.class_name {
                    object.class_name = class_name;
                }
                if let Some(category) = overlay.category {
                    object.category = category;
                }
                if let Some(display_name) = overlay.display_name {
                    object.display_name = Some(display_name);
                }
                if let Some(preview_model) = overlay.preview_model {
                    object.preview_model = Some(preview_model);
                }
                object.hidden |= overlay.hidden.unwrap_or(false);
                object.unsafe_to_edit |= overlay.unsafe_to_edit.unwrap_or(false);
            }
        }

        for (_, overlay) in by_name {
            self.objects.push(ObjectDefinition {
                factory_name: overlay.factory_name,
                class_name: overlay.class_name.unwrap_or_else(|| "Unknown".to_string()),
                category: overlay.category.unwrap_or_else(|| "Overlay".to_string()),
                source: SchemaSource::Overlay,
                display_name: overlay.display_name,
                preview_model: overlay.preview_model,
                hidden: overlay.hidden.unwrap_or(false),
                unsafe_to_edit: overlay.unsafe_to_edit.unwrap_or(false),
            });
        }

        self.objects.sort_by(|a, b| {
            a.category
                .cmp(&b.category)
                .then_with(|| a.factory_name.cmp(&b.factory_name))
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectDefinition {
    pub factory_name: String,
    pub class_name: String,
    pub category: String,
    pub source: SchemaSource,
    pub display_name: Option<String>,
    pub preview_model: Option<String>,
    pub hidden: bool,
    pub unsafe_to_edit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamDefinition {
    pub owner_hint: Option<String>,
    pub member_name: String,
    pub default_value: String,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetHint {
    pub path: String,
    pub source_file: String,
}

/// A model resource selected by a factory/manager pair in the decompilation.
///
/// `resource_base` is present when `createModelDataArrayBase` supplies an
/// explicit archive directory. When absent, consumers should resolve the
/// decomp-authored filename against the stage resource index rather than
/// inventing a factory-name alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectResourceBinding {
    pub factory_name: String,
    pub model_index: usize,
    pub role: ObjectResourceRole,
    pub model_name: String,
    pub resource_base: Option<String>,
    pub load_flags: u32,
    pub source_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectResourceRole {
    /// Root actor model selected by the manager for normal rendering.
    Primary,
    /// Additional manager model data retained for explicit indexed consumers.
    Secondary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapStaticModelDefinition {
    pub actor_name: String,
    /// Primary model passed to `TMapStaticObj::initModel`, if this table row creates one.
    #[serde(default)]
    pub model_path: Option<String>,
    pub load_flags: u32,
    pub source_file: String,
    #[serde(default)]
    pub stage_bootstrap_created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjResourceDefinition {
    /// Exact `TMapObjData::unk0` identity read from the placement stream.
    pub resource_name: String,
    /// Exact runtime actor type stored in `TMapObjData::unk4`.
    #[serde(default)]
    pub actor_type: u32,
    /// Exact `TMapObjData::unk34` behavior flags copied into
    /// `TMapObjBase::unkF8` before model and collision initialization.
    #[serde(default)]
    pub object_flags: u32,
    /// Exact `TMapObjData::unk8` name passed to
    /// `TNameRefGen::search<TLiveManager>` before the actor is registered.
    ///
    /// An empty value is retained only for compatibility with registries
    /// serialized before this dependency was exposed; source-free stock-slot
    /// authoring must reject such entries instead of guessing a manager.
    #[serde(default)]
    pub required_manager_name: String,
    /// True when `TMapObjData::mHold` points at compiled hold-model/joint data.
    /// Replacing only the primary BMD cannot satisfy this dependency.
    #[serde(default)]
    pub has_hold_dependency: bool,
    /// True when `TMapObjData::mMove` points at compiled BCK/joint motion data.
    /// Replacing only the primary BMD cannot satisfy this dependency.
    #[serde(default)]
    pub has_move_dependency: bool,
    /// True only when the compiled table has no animation-info entry and
    /// `TMapObjBase::makeMActors` therefore loads `<resource_name>.bmd`.
    #[serde(default)]
    pub uses_resource_name_model_fallback: bool,
    /// Primary BMD passed to `TMapObjBase::initMActor`, or `None` when actor count is zero.
    pub primary_model: Option<String>,
    /// Exact `TMapObjAnimData` entries consumed by this slot, truncated to
    /// `TMapObjAnimDataInfo::unk0` just as retail does.
    ///
    /// This includes model switches, extensionless animation identities, the
    /// unresolved extra string, and animation-sound BAS paths. An empty list
    /// means the slot uses the `<resource_name>.bmd` fallback or has no actors.
    #[serde(default)]
    pub animation_resources: Vec<MapObjAnimationResourceDefinition>,
    /// Exact BMD path loaded by `TMapObjBase::initHoldData` from
    /// `TMapObjHoldData::unk0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_model_path: Option<String>,
    /// Exact BCK path loaded by `TMapObjBase::initBckMoveData` from
    /// `TMapObjMoveData::unk0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_bck_path: Option<String>,

    /// Effective `TMActorKeeper::mModelLoaderFlags` selected from `TMapObjData::unk34`.
    ///
    /// The default preserves registries serialized before this field was exposed.
    #[serde(default = "default_map_obj_model_load_flags")]
    pub load_flags: u32,
    /// Exact collision basenames and loader flags selected by this stock slot.
    #[serde(default)]
    pub collision_resources: Vec<MapObjCollisionResourceDefinition>,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjAnimationResourceDefinition {
    /// Exact `TMapObjAnimData::unk0` model name passed to `initMActor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// Exact `TMapObjAnimData::unk4` identity passed to `MActor::setAnimation`.
    /// It is an extensionless basename, not specifically a BCK path; consumers
    /// must match it against the supported animation extensions in the actor's
    /// resource folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation_name: Option<String>,
    /// Exact `TMapObjAnimData::unk8` MActor animation channel.
    #[serde(default)]
    pub animation_channel: u8,
    /// Exact, currently unresolved `TMapObjAnimData::unkC` string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_name: Option<String>,
    /// Exact `TMapObjAnimData::unk10` BAS path passed to `setAnmSound`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bas_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjCollisionResourceDefinition {
    pub resource_name: String,
    pub flags: u16,
    /// Low two flag bits passed to `TMapCollisionManager::createCollision`.
    pub collision_kind: u8,
    /// Present for moving collision, whose transformed vertices use a fixed stack array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_vertices: Option<u16>,
}

fn default_map_obj_model_load_flags() -> u32 {
    0x1022_0000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjModelOverrideDefinition {
    pub resource_name: String,
    pub class_name: String,
    pub model_path: String,
    pub load_flags: u32,
    pub tev_color: Option<MapObjTevColorDefinition>,
    pub binding_source_file: String,
    pub model_source_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjTevColorDefinition {
    pub register: u8,
    pub color: [i16; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjStringTevProgramDefinition {
    pub resource_name: String,
    pub class_name: String,
    pub tev_register: u8,
    pub default_color: [i16; 4],
    pub variants: Vec<MapObjStringTevVariantDefinition>,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjStringTevVariantDefinition {
    pub selector_value: String,
    pub color: [i16; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjStreamTevColorDefinition {
    pub class_name: String,
    pub tev_register: u8,
    pub trailing_rgb_u32_count: u8,
    pub alpha: i16,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjBallTransformDefinition {
    pub actor_type: u32,
    pub body_radius: u16,
    pub positive_y_axis_subtract: Option<u16>,
    pub one_minus_y_axis_subtract: Option<u16>,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjFlagDefinition {
    pub factory_name: String,
    pub class_name: String,
    pub texture_path_pattern: String,
    pub registered_texture_names: Vec<String>,
    pub resource_name_stream_index: u8,
    pub default_height: u32,
    pub default_width: u32,
    pub default_segment_size: u32,
    pub default_flutter_speed_degrees_per_frame: u32,
    pub area_flutter_speeds: Vec<MapObjFlagAreaSpeed>,
    pub phase_wrap_degrees: u32,
    pub stage_archive_table_path: String,
    pub source_file: String,
}

/// A named value accepted in the COL triangle type field.
///
/// Property flags are kept alongside concrete types because Sunshine composes
/// them into the same raw `u16` value. Consumers must preserve unknown raw
/// values even when no decomp-authored name is available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollisionSurfaceDefinition {
    pub name: String,
    pub value: u16,
    pub is_property_flag: bool,
    pub source_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapObjFlagAreaSpeed {
    pub area_index: u32,
    pub degrees_per_frame: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticleResourceDefinition {
    pub effect_id: u16,
    pub path: String,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorParticleBinding {
    pub class_name: String,
    pub effect_id: u16,
    pub target: ParticleBindingTarget,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcActorDefinition {
    pub actor_key: String,
    pub source_file: String,
    pub parts: Vec<NpcPartDefinition>,
}

/// A root NPC model color change selected from `TNpcInitInfo::unk34`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcMaterialColorDefinition {
    pub actor_key: String,
    /// Index into the manager's root model-data array.
    pub model_index: u8,
    /// Index into the instance color tuple (`0` body, `1` cloth in retail).
    pub color_index_channel: u8,
    pub change: NpcColorChangeDefinition,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcPartDefinition {
    pub bit_index: u8,
    pub color_index_channel: u8,
    pub models: Vec<NpcPartModelDefinition>,
    pub color_changes: Vec<NpcColorChangeDefinition>,
    pub uses_pollution: bool,
    pub uses_shared_materials: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcPartModelDefinition {
    pub joint_name: Option<String>,
    pub model_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcColorChangeDefinition {
    pub mode: u8,
    pub material_name: String,
    pub colors0: Vec<[i16; 4]>,
    pub colors1: Vec<[i16; 4]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyManagerDefinition {
    pub factory_name: String,
    pub class_name: String,
    pub model_index: Option<usize>,
    #[serde(default)]
    pub spawned_actor_class: Option<String>,
    /// Parameter archive path constructed by the manager's decomp load routine.
    #[serde(default)]
    pub parameter_path: Option<String>,
    pub models: Vec<EnemyModelDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyManagerAnimationFolderDefinition {
    pub factory_name: String,
    pub folder: String,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMapObjDependencyDefinition {
    pub factory_name: String,
    pub resource_name: String,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeNameReferenceTarget {
    Actor { factory_name: String },
    PlacementRecord { type_name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RuntimeNameReferenceDefinition {
    pub factory_name: String,
    pub target: RuntimeNameReferenceTarget,
    pub required: bool,
    pub record_name: String,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyActorDefinition {
    pub factory_name: String,
    pub class_name: String,
    pub model_index: Option<usize>,
    #[serde(default)]
    pub fallback_models: Vec<EnemyModelDefinition>,
    #[serde(default)]
    pub primary_model: Option<String>,
    #[serde(default)]
    pub named_models: Vec<EnemyNamedModelDefinition>,
    #[serde(default)]
    pub indexed_models: Vec<EnemyIndexedModelDefinition>,
    #[serde(default)]
    pub manager_factories: Vec<String>,
    /// Post-load uniform scale selected from named runtime parameters.
    #[serde(default)]
    pub runtime_uniform_scale: Option<EnemyRuntimeUniformScaleDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyRuntimeUniformScaleDefinition {
    pub low_parameter: String,
    pub high_parameter: String,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyModelDefinition {
    pub model_name: String,
    pub load_flags: u32,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyNamedModelDefinition {
    pub actor_name: String,
    pub model_path: String,
    pub load_flags: u32,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyIndexedModelDefinition {
    pub index: u32,
    pub model_path: String,
    pub load_flags: u32,
    pub source_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnemyMaterialTevColorDefinition {
    pub factory_name: String,
    pub material_name: String,
    pub tev_register: u8,
    /// Channels not assigned by the actor retain the color authored in the BMD.
    pub color: [Option<i16>; 4],
    pub source_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticleBindingTarget {
    ActorOrigin,
    ModelJoint(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaSource {
    MarNameRefGen,
    MapObjManager,
    ParamInit,
    Overlay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SchemaOverlay {
    #[serde(default)]
    pub objects: Vec<ObjectOverlay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectOverlay {
    pub factory_name: String,
    pub class_name: Option<String>,
    pub category: Option<String>,
    pub display_name: Option<String>,
    pub preview_model: Option<String>,
    pub hidden: Option<bool>,
    pub unsafe_to_edit: Option<bool>,
}

pub struct SchemaGenerator {
    repo_root: PathBuf,
}

impl SchemaGenerator {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        Self {
            repo_root: repo_root.as_ref().to_path_buf(),
        }
    }

    pub fn generate(&self) -> Result<ObjectRegistry> {
        let sources = SourceInventory::build(&self.repo_root)?;
        let mut registry = ObjectRegistry::default();
        self.scan_mar_name_ref_gen(&sources, &mut registry)?;
        self.scan_enemy_model_data(&sources, &mut registry)?;
        self.scan_map_obj_manager(&sources, &mut registry)?;
        self.scan_map_obj_resources(&sources, &mut registry)?;
        self.scan_map_obj_ball_transforms(&sources, &mut registry)?;
        self.scan_map_obj_model_overrides(&sources, &mut registry)?;
        self.scan_map_obj_string_tev_programs(&sources, &mut registry)?;
        self.scan_map_obj_stream_tev_colors(&sources, &mut registry)?;
        self.scan_params_and_assets(&sources, &mut registry)?;
        self.scan_map_static_models(&sources, &mut registry)?;
        self.scan_map_obj_flags(&sources, &mut registry)?;
        self.scan_collision_surfaces(&sources, &mut registry)?;
        self.scan_particle_bindings(&sources, &mut registry)?;
        self.scan_npc_resource_bindings(&sources, &mut registry)?;
        self.scan_npc_init_data(&sources, &mut registry)?;
        self.scan_map_obj_factory_types(&sources, &mut registry)?;
        dedup_registry(&mut registry)?;
        validate_registry(&registry)?;
        Ok(registry)
    }

    pub fn load_overlay(&self, overlay_path: impl AsRef<Path>) -> Result<SchemaOverlay> {
        let text = fs::read_to_string(overlay_path)?;
        Ok(toml::from_str(&text)?)
    }

    fn scan_mar_name_ref_gen(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        for (file_name, category, has_enemy_variants) in [
            ("MarNameRefGen.cpp", "System", false),
            ("MarNameRefGen_Map.cpp", "Map", false),
            ("MarNameRefGen_MapObj.cpp", "MapObj", false),
            ("MarNameRefGen_Enemy.cpp", "Enemy", true),
            ("MarNameRefGen_BossEnemy.cpp", "Boss", true),
        ] {
            let relative_path = format!("src/System/{file_name}");
            let source_file = sources.required(&relative_path)?;
            let object_count = registry.objects.len();
            extract_string_factory_returns(
                source_file.text(),
                category,
                SchemaSource::MarNameRefGen,
                registry,
            );
            ensure_extracted(
                SchemaExtractor::FactoryRegistration,
                self.repo_root.join(&relative_path),
                registry.objects.len() - object_count,
                "factory-controlled object names",
            )?;
            if has_enemy_variants {
                let variant_count = registry.enemy_actors.len() + registry.enemy_managers.len();
                extract_enemy_factory_variants(source_file.text(), registry);
                ensure_extracted(
                    SchemaExtractor::FactoryRegistration,
                    self.repo_root.join(&relative_path),
                    registry.enemy_actors.len() + registry.enemy_managers.len() - variant_count,
                    "enemy actor or manager registrations",
                )?;
            }
        }
        Ok(())
    }

    fn scan_enemy_model_data(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let mut class_models = BTreeMap::<String, Vec<EnemyModelDefinition>>::new();
        let mut actor_models = BTreeMap::<String, Vec<EnemyModelDefinition>>::new();
        let mut actor_primary_models = BTreeMap::<String, String>::new();
        let mut actor_named_models = BTreeMap::<String, Vec<EnemyNamedModelDefinition>>::new();
        let mut actor_indexed_models = BTreeMap::<String, Vec<EnemyIndexedModelDefinition>>::new();
        let mut actor_runtime_uniform_scales =
            BTreeMap::<String, EnemyRuntimeUniformScaleDefinition>::new();
        let mut owned_actor_classes = BTreeMap::<String, Vec<String>>::new();
        let mut actor_root_parts = BTreeMap::<String, String>::new();
        let mut part_model_indices = BTreeMap::<String, usize>::new();
        let mut manager_actor_classes = BTreeMap::<String, String>::new();
        let mut manager_parameter_paths = BTreeMap::<String, String>::new();
        let mut manager_animation_folders = BTreeMap::<String, Vec<(String, String)>>::new();
        let mut runtime_map_obj_dependencies = BTreeMap::<String, Vec<(String, String)>>::new();
        let mut runtime_name_references =
            BTreeMap::<String, Vec<ExtractedRuntimeNameReference>>::new();
        let mut inheritance = BTreeMap::<String, String>::new();
        let mut tev_color_bindings = Vec::new();
        let mut init_tev_colors = BTreeMap::new();
        let loader_flags_path = "include/JSystem/J3D/J3DGraphLoader/J3DModelLoaderFlags.hpp";
        let flag_symbols = extract_cpp_u32_constants(sources.required(loader_flags_path)?.text());
        ensure_extracted(
            SchemaExtractor::ModelLoaderFlags,
            self.repo_root.join(loader_flags_path),
            flag_symbols.len(),
            "numeric J3D loader flag constants",
        )?;

        let mut enemy_source_count = 0usize;
        for file in sources.files_under_any(&[
            "src/Enemy/",
            "src/Animal/",
            "src/Strategic/",
            "include/Enemy/",
            "include/Animal/",
            "include/Strategic/",
        ]) {
            if !matches!(file.extension(), "cpp" | "hpp" | "h") {
                continue;
            }
            enemy_source_count += 1;
            let text = file.text();
            extract_class_inheritance(text, &mut inheritance);
            if file.extension() == "cpp" {
                let source_file = file.relative_path().to_string();
                for (class_name, models) in
                    extract_enemy_manager_models(text, &source_file, &flag_symbols)
                {
                    class_models.entry(class_name).or_default().extend(models);
                }
                for (class_name, models) in extract_enemy_actor_fallback_models(text, &source_file)
                {
                    actor_models.entry(class_name).or_default().extend(models);
                }
                for (class_name, model_name) in extract_enemy_actor_primary_models(text) {
                    actor_primary_models.entry(class_name).or_insert(model_name);
                }
                for (class_name, models) in extract_enemy_named_models(text, &source_file) {
                    actor_named_models
                        .entry(class_name)
                        .or_default()
                        .extend(models);
                }
                for (class_name, models) in extract_enemy_indexed_models(text, &source_file) {
                    actor_indexed_models
                        .entry(class_name)
                        .or_default()
                        .extend(models);
                }
                for (class_name, definition) in
                    extract_enemy_runtime_uniform_scales(text, &source_file)
                {
                    actor_runtime_uniform_scales
                        .entry(class_name)
                        .or_insert(definition);
                }
                extract_owned_actor_classes(text, &mut owned_actor_classes);
                extract_actor_root_parts(text, &mut actor_root_parts);
                extract_part_model_indices(text, &mut part_model_indices);
                extract_enemy_manager_actor_classes(text, &mut manager_actor_classes);
                for (class_name, path) in extract_enemy_manager_parameter_paths(text) {
                    manager_parameter_paths.entry(class_name).or_insert(path);
                }
                for (class_name, dependencies) in
                    extract_runtime_map_obj_dependencies(text, &source_file)
                {
                    runtime_map_obj_dependencies
                        .entry(class_name)
                        .or_default()
                        .extend(dependencies);
                }
                for (class_name, folders) in
                    extract_enemy_manager_animation_folders(text, &source_file)
                {
                    // Preserve even an empty override: some subclasses call
                    // TObjManager here specifically to avoid inheriting the
                    // parent's hardcoded stage-local animation directory.
                    manager_animation_folders
                        .entry(class_name)
                        .or_insert(folders);
                }
                tev_color_bindings.extend(extract_enemy_tev_color_bindings(text, &source_file));
                extract_enemy_init_tev_colors(text, &mut init_tev_colors);
            }
        }
        ensure_extracted(
            SchemaExtractor::EnemyModelData,
            self.repo_root.join("src/Enemy"),
            enemy_source_count,
            "enemy/animal/strategic C++ sources",
        )?;

        // Fixed actor-name lookups are a game-wide authoring contract, not an
        // enemy-specific one. Scan every gameplay source while keeping the
        // owning class and source provenance decomp-derived.
        for file in sources.files_under_any(&["src/", "include/"]) {
            if !matches!(file.extension(), "cpp" | "hpp" | "h") {
                continue;
            }
            let text = file.text();
            extract_class_inheritance(text, &mut inheritance);
            if file.extension() != "cpp" {
                continue;
            }
            let source_file = file.relative_path().to_string();
            for (class_name, references) in extract_runtime_name_references(text, &source_file) {
                runtime_name_references
                    .entry(class_name)
                    .or_default()
                    .extend(references);
            }
        }

        // A few retail-only manager subclasses are declared beside their
        // factory registrations rather than in public headers.
        for file_name in ["MarNameRefGen_Enemy.cpp", "MarNameRefGen_BossEnemy.cpp"] {
            let relative_path = format!("src/System/{file_name}");
            let text = sources.required(&relative_path)?.text();
            let mut supplemental = BTreeMap::new();
            extract_class_inheritance(text, &mut supplemental);
            for (class_name, parent) in supplemental {
                inheritance.entry(class_name).or_insert(parent);
            }
        }

        for object in &registry.objects {
            registry.runtime_map_obj_dependencies.extend(
                inherited_actor_models_union(
                    &object.class_name,
                    &runtime_map_obj_dependencies,
                    &inheritance,
                )
                .into_iter()
                .map(|(resource_name, source_file)| {
                    RuntimeMapObjDependencyDefinition {
                        factory_name: object.factory_name.clone(),
                        resource_name,
                        source_file,
                    }
                }),
            );
            registry.runtime_name_references.extend(
                inherited_actor_models_union(
                    &object.class_name,
                    &runtime_name_references,
                    &inheritance,
                )
                .into_iter()
                .map(|reference| RuntimeNameReferenceDefinition {
                    factory_name: object.factory_name.clone(),
                    target: reference.target,
                    required: reference.required,
                    record_name: reference.record_name,
                    source_file: reference.source_file,
                }),
            );
        }

        for definition in &mut registry.enemy_actors {
            let Some(object) = registry
                .objects
                .iter()
                .find(|object| object.factory_name == definition.factory_name)
            else {
                continue;
            };
            definition.class_name.clone_from(&object.class_name);
            if definition.model_index.is_none() {
                definition.model_index = actor_root_parts
                    .get(&definition.class_name)
                    .and_then(|part_class| part_model_indices.get(part_class))
                    .copied();
            }
            definition.fallback_models = actor_models
                .get(&definition.class_name)
                .cloned()
                .unwrap_or_default();
            definition.primary_model = inherited_actor_primary_model(
                &definition.class_name,
                &actor_primary_models,
                &inheritance,
            );
            let model_class = owned_actor_classes
                .get(&definition.class_name)
                .and_then(|classes| {
                    classes
                        .iter()
                        .find(|class_name| actor_named_models.contains_key(*class_name))
                })
                .map(String::as_str)
                .unwrap_or(&definition.class_name);
            definition.named_models = actor_named_models
                .get(model_class)
                .cloned()
                .unwrap_or_default();
            definition.indexed_models =
                inherited_actor_models(&definition.class_name, &actor_indexed_models, &inheritance)
                    .unwrap_or_default();
            definition.runtime_uniform_scale = inherited_actor_value(
                &definition.class_name,
                &actor_runtime_uniform_scales,
                &inheritance,
            );
        }

        let manager_objects = registry
            .objects
            .iter()
            .filter(|object| {
                matches!(object.category.as_str(), "Enemy" | "Boss")
                    && object
                        .class_name
                        .rsplit("::")
                        .next()
                        .is_some_and(|class_name| class_name.ends_with("Manager"))
            })
            .cloned()
            .collect::<Vec<_>>();
        for object in manager_objects {
            let Some(models) =
                inherited_enemy_models(&object.class_name, &class_models, &inheritance)
            else {
                continue;
            };
            let model_index = registry
                .enemy_managers
                .iter()
                .find(|definition| definition.factory_name == object.factory_name)
                .and_then(|definition| definition.model_index);
            registry
                .enemy_managers
                .retain(|definition| definition.factory_name != object.factory_name);
            let animation_folders = inherited_actor_models(
                &object.class_name,
                &manager_animation_folders,
                &inheritance,
            )
            .unwrap_or_default();
            let factory_name = object.factory_name;
            registry.enemy_managers.push(EnemyManagerDefinition {
                factory_name: factory_name.clone(),
                spawned_actor_class: inherited_actor_class(
                    &object.class_name,
                    &manager_actor_classes,
                    &inheritance,
                ),
                parameter_path: inherited_string_value(
                    &object.class_name,
                    &manager_parameter_paths,
                    &inheritance,
                ),
                class_name: object.class_name,
                model_index,
                models,
            });
            registry
                .enemy_manager_animation_folders
                .extend(animation_folders.into_iter().map(|(folder, source_file)| {
                    EnemyManagerAnimationFolderDefinition {
                        factory_name: factory_name.clone(),
                        folder,
                        source_file,
                    }
                }));
        }
        registry
            .enemy_managers
            .retain(|definition| !definition.models.is_empty());
        for actor in &mut registry.enemy_actors {
            actor.manager_factories =
                compatible_enemy_managers(actor, &registry.enemy_managers, &inheritance);
        }
        registry.enemy_material_colors = derive_enemy_material_tev_colors(
            &registry.enemy_actors,
            &tev_color_bindings,
            &init_tev_colors,
            &inheritance,
        );
        Ok(())
    }

    fn scan_map_obj_manager(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let relative_path = "src/MoveBG/MapObjManager.cpp";
        let text = sources.required(relative_path)?.text();
        let before = registry.objects.len();
        extract_string_factory_returns(text, "MapObj", SchemaSource::MapObjManager, registry);
        ensure_extracted(
            SchemaExtractor::FactoryRegistration,
            self.repo_root.join(relative_path),
            registry.objects.len() - before,
            "map-object factory-controlled names",
        )?;
        Ok(())
    }

    fn scan_map_obj_resources(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let relative_path = "src/MoveBG/MapObjInit.cpp";
        let text = sources.required(relative_path)?.text();
        // A null animation table is not model-less: this is the retail fallback that
        // turns the exact resource identity into its primary BMD name.
        if !has_null_animation_model_fallback(text) {
            return Err(SchemaError::ExtractionDrift {
                extractor: SchemaExtractor::MapObjResources,
                source_path: self.repo_root.join(relative_path),
                expected: "TMapObjBase null-animation primary-model fallback",
            });
        }
        registry.map_obj_resources =
            extract_map_obj_resources(text, relative_path).map_err(|detail| {
                SchemaError::RegistryInvariant {
                    detail: format!("failed to extract map-object resources: {detail}"),
                }
            })?;
        let collision_limit_path = "src/Map/MapMakeData.cpp";
        let collision_limit =
            extract_moving_collision_vertex_limit(sources.required(collision_limit_path)?.text())
                .ok_or_else(|| SchemaError::ExtractionDrift {
                extractor: SchemaExtractor::CollisionLimits,
                source_path: self.repo_root.join(collision_limit_path),
                expected: "fixed moving-collision vertex transform capacity",
            })?;
        registry.moving_collision_vertex_limit = Some(collision_limit);
        for resource in &mut registry.map_obj_resources {
            for collision in &mut resource.collision_resources {
                if collision.collision_kind == 1 {
                    collision.max_vertices = Some(collision_limit);
                }
            }
        }
        ensure_extracted(
            SchemaExtractor::MapObjResources,
            self.repo_root.join(relative_path),
            registry.map_obj_resources.len(),
            "TMapObjData resource identities",
        )?;
        Ok(())
    }

    fn scan_map_obj_factory_types(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let mut inheritance = BTreeMap::new();
        for file in sources.files_under_any(&["include/", "src/"]) {
            if matches!(file.extension(), "h" | "hpp" | "cpp") {
                extract_class_inheritance(file.text(), &mut inheritance);
            }
        }
        registry.map_obj_factories = registry
            .objects
            .iter()
            .filter(|object| class_is_or_inherits(&object.class_name, "TMapObjBase", &inheritance))
            .map(|object| object.factory_name.clone())
            .collect();
        ensure_extracted(
            SchemaExtractor::MapObjFactoryTypes,
            self.repo_root.join("include"),
            registry.map_obj_factories.len(),
            "registered factories inheriting TMapObjBase",
        )?;
        Ok(())
    }

    fn scan_map_obj_ball_transforms(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let source_path = "src/MoveBG/MapObjBall.cpp";
        registry.map_obj_ball_transforms =
            extract_map_obj_ball_transforms(sources.required(source_path)?.text(), source_path)
                .map_err(|detail| SchemaError::RegistryInvariant {
                    detail: format!("failed to extract map-object ball transforms: {detail}"),
                })?;
        ensure_extracted(
            SchemaExtractor::MapObjBallTransforms,
            self.repo_root.join(source_path),
            registry.map_obj_ball_transforms.len(),
            "actor-type body-radius transforms",
        )?;
        Ok(())
    }

    fn scan_map_obj_string_tev_programs(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let source_path = "src/MoveBG/Item.cpp";
        let header_path = "include/MoveBG/Item.hpp";
        let definition = extract_nozzle_box_tev_program(
            sources.required(source_path)?.text(),
            sources.required(header_path)?.text(),
            source_path,
        )
        .map_err(|detail| SchemaError::RegistryInvariant {
            detail: format!("failed to extract map-object string TEV program: {detail}"),
        })?;
        registry.map_obj_string_tev_programs = vec![definition];
        ensure_extracted(
            SchemaExtractor::MapObjStringTevPrograms,
            self.repo_root.join(source_path),
            registry.map_obj_string_tev_programs.len(),
            "typed string-selected material color programs",
        )?;
        Ok(())
    }

    fn scan_map_obj_stream_tev_colors(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        for file in sources.files_under_any(&["src/MoveBG/"]) {
            if file.extension() != "cpp" {
                continue;
            }
            registry
                .map_obj_stream_tev_colors
                .extend(extract_map_obj_stream_tev_colors(
                    file.text(),
                    file.relative_path(),
                ));
        }
        ensure_extracted(
            SchemaExtractor::MapObjStreamTevColors,
            self.repo_root.join("src/MoveBG"),
            registry.map_obj_stream_tev_colors.len(),
            "fixed-width placement-stream material colors",
        )?;
        Ok(())
    }

    fn scan_map_obj_model_overrides(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let manager_path = "src/MoveBG/MapObjManager.cpp";
        let loaders = extract_shared_model_loaders(sources.required(manager_path)?.text());
        let mut definitions = Vec::new();
        let mut direct_overrides = Vec::new();
        for file in sources.files_under_any(&["src/MoveBG/"]) {
            if file.extension() != "cpp" {
                continue;
            }
            definitions.extend(
                extract_map_obj_shared_models(
                    file.text(),
                    file.relative_path(),
                    manager_path,
                    &loaders,
                )
                .map_err(|detail| SchemaError::RegistryInvariant {
                    detail: format!("failed to extract map-object shared models: {detail}"),
                })?,
            );
            direct_overrides.extend(
                extract_direct_make_mactors_overrides(file.text(), file.relative_path()).map_err(
                    |detail| SchemaError::RegistryInvariant {
                        detail: format!("failed to extract direct map-object models: {detail}"),
                    },
                )?,
            );
        }
        for direct in direct_overrides {
            let factories = registry
                .objects
                .iter()
                .filter(|object| object.class_name == direct.class_name)
                .collect::<Vec<_>>();
            for factory in factories {
                let resources = registry
                    .map_obj_resources
                    .iter()
                    .filter(|resource| resource.primary_model.is_none())
                    .filter(|resource| {
                        resource
                            .resource_name
                            .eq_ignore_ascii_case(&factory.factory_name)
                    })
                    .collect::<Vec<_>>();
                if resources.len() != 1 {
                    return Err(SchemaError::RegistryInvariant {
                        detail: format!(
                            "direct model override {} for factory {} matched {} zero-primary resources",
                            direct.class_name,
                            factory.factory_name,
                            resources.len()
                        ),
                    });
                }
                definitions.push(MapObjModelOverrideDefinition {
                    resource_name: resources[0].resource_name.clone(),
                    class_name: direct.class_name.clone(),
                    model_path: direct.model_path.clone(),
                    load_flags: direct.load_flags,
                    tev_color: None,
                    binding_source_file: direct.source_file.clone(),
                    model_source_file: direct.source_file.clone(),
                });
            }
        }
        registry.map_obj_model_overrides = definitions;
        ensure_extracted(
            SchemaExtractor::MapObjModelOverrides,
            self.repo_root.join("src/MoveBG"),
            registry.map_obj_model_overrides.len(),
            "resource-selected custom or shared models",
        )?;
        Ok(())
    }

    fn scan_params_and_assets(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let param_re = Regex::new(r"PARAM_INIT\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([^)]+)\)")
            .expect("valid param regex");
        let asset_re =
            Regex::new(r#""(/(?:scene|common|select|game_6|guide|option|subtitle)[^"]+)""#)
                .expect("valid asset regex");

        for file in sources.files() {
            let text = file.text();
            let source_file = file.relative_path().to_string();
            let owner_hint = Path::new(file.relative_path())
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_string());

            for cap in param_re.captures_iter(text) {
                registry.params.push(ParamDefinition {
                    owner_hint: owner_hint.clone(),
                    member_name: cap[1].to_string(),
                    default_value: cap[2].trim().to_string(),
                    source_file: source_file.clone(),
                });
            }

            for cap in asset_re.captures_iter(text) {
                registry.asset_hints.push(AssetHint {
                    path: cap[1].to_string(),
                    source_file: source_file.clone(),
                });
            }
        }

        ensure_extracted(
            SchemaExtractor::Params,
            self.repo_root.join("src"),
            registry.params.len(),
            "PARAM_INIT declarations",
        )?;
        ensure_extracted(
            SchemaExtractor::AssetHints,
            self.repo_root.join("src"),
            registry.asset_hints.len(),
            "decomp resource paths",
        )?;

        Ok(())
    }

    fn scan_particle_bindings(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        for file in sources.files_under_any(&["src/"]) {
            if file.extension() != "cpp" {
                continue;
            }
            let source_file = file.relative_path().to_string();
            extract_particle_resources(file.text(), &source_file, registry);
            extract_calc_particle_bindings(file.text(), &source_file, registry);
        }
        ensure_extracted(
            SchemaExtractor::ParticleResources,
            self.repo_root.join("src"),
            registry.particle_resources.len(),
            "loaded JPA resources",
        )?;
        ensure_extracted(
            SchemaExtractor::ParticleBindings,
            self.repo_root.join("src"),
            registry.actor_particle_bindings.len(),
            "persistent actor particle bindings",
        )?;
        Ok(())
    }

    fn scan_map_static_models(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let relative_path = "src/Map/MapStaticObject.cpp";
        let text = sources.required(relative_path)?.text();
        let map_path = "src/Map/Map.cpp";
        let stage_bootstrap_actors =
            extract_stage_bootstrap_map_static_actors(sources.required(map_path)?.text());
        registry.map_static_models = extract_map_static_models(text, relative_path);
        ensure_extracted(
            SchemaExtractor::MapStaticModels,
            self.repo_root.join(relative_path),
            registry.map_static_models.len(),
            "map-static actor/model table entries",
        )?;
        for model in &mut registry.map_static_models {
            model.stage_bootstrap_created = stage_bootstrap_actors
                .iter()
                .any(|actor| actor == &model.actor_name);
        }
        Ok(())
    }

    fn scan_map_obj_flags(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let source_path = "src/MoveBG/MapObjFlag.cpp";
        let source = sources.required(source_path)?.text();
        let factories = sources
            .required("src/System/MarNameRefGen_MapObj.cpp")?
            .text();
        let application = sources.required("src/System/Application.cpp")?.text();
        let Some(definition) =
            extract_map_obj_flag_definition(source, factories, application, source_path)
        else {
            return Err(SchemaError::ExtractionDrift {
                extractor: SchemaExtractor::MapObjFlag,
                source_path: self.repo_root.join(source_path),
                expected: "complete procedural flag metadata",
            });
        };
        if !registry
            .objects
            .iter()
            .any(|object| object.factory_name == definition.factory_name)
        {
            registry.objects.push(ObjectDefinition {
                factory_name: definition.factory_name.clone(),
                class_name: definition.class_name.clone(),
                category: "MapObj".to_string(),
                source: SchemaSource::MarNameRefGen,
                display_name: None,
                preview_model: None,
                hidden: false,
                unsafe_to_edit: false,
            });
        }
        registry.map_obj_flags.push(definition);
        Ok(())
    }

    fn scan_collision_surfaces(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let source_path = "include/Map/MapData.hpp";
        let source = sources.required(source_path)?.text();
        registry.collision_surfaces = extract_collision_surfaces(source, source_path);
        ensure_extracted(
            SchemaExtractor::CollisionSurfaces,
            self.repo_root.join(source_path),
            registry.collision_surfaces.len(),
            "BGTypeBits collision types and property flags",
        )
    }

    fn scan_npc_init_data(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let relative_path = "src/NPC/NpcInitData.cpp";
        let text = sources.required(relative_path)?.text();
        registry.npc_actors = extract_npc_actor_definitions(text, relative_path);
        registry.npc_material_colors = extract_npc_material_color_definitions(text, relative_path);
        ensure_extracted(
            SchemaExtractor::NpcInitData,
            self.repo_root.join(relative_path),
            registry.npc_actors.len(),
            "TNpcInitInfo actor definitions",
        )?;
        ensure_extracted(
            SchemaExtractor::NpcRootColors,
            self.repo_root.join(relative_path),
            registry.npc_material_colors.len(),
            "TNpcInitInfo root-model color bindings",
        )?;
        Ok(())
    }

    fn scan_npc_resource_bindings(
        &self,
        sources: &SourceInventory,
        registry: &mut ObjectRegistry,
    ) -> Result<()> {
        let factory_path = "src/System/MarNameRefGen_NPC.cpp";
        let factory_text = sources.required(factory_path)?.text();
        let before = registry.objects.len();
        extract_string_factory_returns(factory_text, "NPC", SchemaSource::MarNameRefGen, registry);
        ensure_extracted(
            SchemaExtractor::FactoryRegistration,
            self.repo_root.join(factory_path),
            registry.objects.len() - before,
            "NPC actor and manager registrations",
        )?;

        let manager_path = "src/NPC/NpcManager.cpp";
        let manager_text = sources.required(manager_path)?.text();
        let loader_flags_path = "include/JSystem/J3D/J3DGraphLoader/J3DModelLoaderFlags.hpp";
        let flag_symbols = extract_cpp_u32_constants(sources.required(loader_flags_path)?.text());
        let manager_models =
            extract_enemy_manager_models(manager_text, manager_path, &flag_symbols)
                .into_iter()
                .collect::<BTreeMap<_, _>>();
        let resource_bases = extract_npc_manager_resource_bases(manager_text);

        let mut inheritance = BTreeMap::new();
        for file in sources.files_under_any(&["src/NPC/", "include/NPC/"]) {
            extract_class_inheritance(file.text(), &mut inheritance);
        }
        let factory_classes = extract_factory_candidates(factory_text)
            .into_iter()
            .filter_map(|candidate| Some((candidate.factory_name, candidate.class_name?)))
            .collect::<BTreeMap<_, _>>();

        for (factory_name, _) in factory_classes
            .iter()
            .filter(|(factory_name, _)| factory_name.starts_with("NPC"))
        {
            let Some(actor_key) = factory_name.strip_prefix("NPC") else {
                continue;
            };
            let manager_factory = format!("{actor_key}Manager");
            let Some(manager_class) = factory_classes.get(&manager_factory) else {
                continue;
            };
            let Some(models) = inherited_enemy_models(manager_class, &manager_models, &inheritance)
            else {
                continue;
            };
            let resource_base =
                inherited_string_value(manager_class, &resource_bases, &inheritance);
            registry
                .object_resources
                .extend(models.into_iter().enumerate().map(|(model_index, model)| {
                    ObjectResourceBinding {
                        factory_name: factory_name.clone(),
                        model_index,
                        role: if model_index == 0 {
                            ObjectResourceRole::Primary
                        } else {
                            ObjectResourceRole::Secondary
                        },
                        model_name: model.model_name,
                        resource_base: resource_base.clone(),
                        load_flags: model.load_flags,
                        source_file: model.source_file,
                    }
                }));
        }

        ensure_extracted(
            SchemaExtractor::NpcResources,
            self.repo_root.join(manager_path),
            registry.object_resources.len(),
            "NPC factory-to-model bindings",
        )?;
        Ok(())
    }
}

#[derive(Clone)]
struct ParsedNpcModelData {
    joints: [Option<String>; 2],
    model_names: Vec<String>,
    color_changes: Vec<NpcColorChangeDefinition>,
    color_index_channel: u8,
    uses_pollution: bool,
    uses_shared_materials: bool,
}

fn extract_npc_actor_definitions(text: &str, source_file: &str) -> Vec<NpcActorDefinition> {
    let color_arrays = extract_npc_color_arrays(text);
    let color_changes = extract_npc_color_changes(text, &color_arrays);
    let model_data = extract_npc_model_data(text, &color_changes);
    let initializer_re =
        Regex::new(r"static\s+const\s+TNpcInitInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid NPC initializer regex");
    let reference_re =
        Regex::new(r"&([A-Za-z_][A-Za-z0-9_]*)|nullptr").expect("valid NPC model reference regex");
    let mut actors = Vec::new();

    for captures in initializer_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let fields = split_cpp_initializer_fields(body);
        let Some(parts_field) = fields.get(1) else {
            continue;
        };
        let actor_key = captures[1]
            .strip_prefix('s')
            .and_then(|name| name.strip_suffix("_InitData"))
            .unwrap_or(&captures[1])
            .to_string();
        let mut parts = Vec::new();
        for (bit_index, reference) in reference_re.captures_iter(parts_field).enumerate() {
            let Some(name) = reference.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let Some(model) = model_data.get(name) else {
                continue;
            };
            let models = model
                .model_names
                .iter()
                .enumerate()
                .map(|(index, model_name)| NpcPartModelDefinition {
                    joint_name: model.joints.get(index).cloned().flatten(),
                    model_name: model_name.clone(),
                })
                .collect();
            parts.push(NpcPartDefinition {
                bit_index: bit_index as u8,
                color_index_channel: model.color_index_channel,
                models,
                color_changes: model.color_changes.clone(),
                uses_pollution: model.uses_pollution,
                uses_shared_materials: model.uses_shared_materials,
            });
        }
        actors.push(NpcActorDefinition {
            actor_key,
            source_file: source_file.to_string(),
            parts,
        });
    }
    actors
}

fn extract_npc_material_color_definitions(
    text: &str,
    source_file: &str,
) -> Vec<NpcMaterialColorDefinition> {
    let color_arrays = extract_npc_color_arrays(text);
    let color_changes = extract_npc_color_changes(text, &color_arrays);
    let initializer_re =
        Regex::new(r"static\s+const\s+TNpcInitInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid NPC initializer regex");
    let reference_re = Regex::new(r"&([A-Za-z_][A-Za-z0-9_]*)|\b(?:nullptr|0)\b")
        .expect("valid NPC root-color reference regex");
    let mut definitions = Vec::new();

    for captures in initializer_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let fields = split_cpp_initializer_fields(body);
        let Some(root_colors) = fields.get(2) else {
            continue;
        };
        let Some(open_brace) = root_colors.find('{') else {
            continue;
        };
        let Some(root_colors_body) = braced_body(root_colors, open_brace) else {
            continue;
        };
        let actor_key = captures[1]
            .strip_prefix('s')
            .and_then(|name| name.strip_suffix("_InitData"))
            .unwrap_or(&captures[1]);

        for (color_index_channel, slot) in split_cpp_initializer_fields(root_colors_body)
            .into_iter()
            .enumerate()
        {
            for (model_index, reference) in reference_re.captures_iter(slot).enumerate() {
                let Some(change_name) = reference.get(1).map(|capture| capture.as_str()) else {
                    continue;
                };
                let Some(change) = color_changes.get(change_name) else {
                    continue;
                };
                let (Ok(color_index_channel), Ok(model_index)) =
                    (u8::try_from(color_index_channel), u8::try_from(model_index))
                else {
                    continue;
                };
                definitions.push(NpcMaterialColorDefinition {
                    actor_key: actor_key.to_string(),
                    model_index,
                    color_index_channel,
                    change: change.clone(),
                    source_file: source_file.to_string(),
                });
            }
        }
    }

    definitions
}

fn extract_npc_color_arrays(text: &str) -> BTreeMap<String, Vec<[i16; 4]>> {
    let initializer_re =
        Regex::new(r"static\s+const\s+GXColorS10\s+([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*\]\s*=\s*\{")
            .expect("valid NPC color array regex");
    let color_re =
        Regex::new(r"\{\s*(-?[0-9]+)\s*,\s*(-?[0-9]+)\s*,\s*(-?[0-9]+)\s*,\s*(-?[0-9]+)\s*\}")
            .expect("valid NPC color regex");
    let mut arrays = BTreeMap::new();
    for captures in initializer_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let colors = color_re
            .captures_iter(body)
            .filter_map(|color| {
                Some([
                    color[1].parse().ok()?,
                    color[2].parse().ok()?,
                    color[3].parse().ok()?,
                    color[4].parse().ok()?,
                ])
            })
            .collect::<Vec<_>>();
        arrays.insert(captures[1].to_string(), colors);
    }
    arrays
}

fn extract_npc_color_changes(
    text: &str,
    arrays: &BTreeMap<String, Vec<[i16; 4]>>,
) -> BTreeMap<String, NpcColorChangeDefinition> {
    let initializer_re =
        Regex::new(r"static\s+const\s+TColorChangeInfo\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid NPC color-change regex");
    let mut changes = BTreeMap::new();
    for captures in initializer_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 4 {
            continue;
        }
        let Some(mode) = parse_cpp_u32(fields[0]).and_then(|value| u8::try_from(value).ok()) else {
            continue;
        };
        let Some(material_name) = parse_cpp_string(fields[1]) else {
            continue;
        };
        let colors_for = |field: &str| {
            cpp_identifier(field)
                .and_then(|name| arrays.get(name))
                .cloned()
                .unwrap_or_default()
        };
        changes.insert(
            captures[1].to_string(),
            NpcColorChangeDefinition {
                mode,
                material_name,
                colors0: colors_for(fields[2]),
                colors1: colors_for(fields[3]),
            },
        );
    }
    changes
}

fn extract_npc_model_data(
    text: &str,
    color_changes: &BTreeMap<String, NpcColorChangeDefinition>,
) -> BTreeMap<String, ParsedNpcModelData> {
    let initializer_re =
        Regex::new(r"static\s+(?:const\s+)?TNpcModelData\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{")
            .expect("valid NPC model-data regex");
    let string_re = Regex::new(r#""([^"]+)""#).expect("valid C++ string regex");
    let reference_re = Regex::new(r"&([A-Za-z_][A-Za-z0-9_]*)").expect("valid C++ reference regex");
    let mut models = BTreeMap::new();
    for captures in initializer_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let fields = split_cpp_initializer_fields(body);
        if fields.len() < 7 {
            continue;
        }
        let model_names = string_re
            .captures_iter(fields[2])
            .map(|model| model[1].to_string())
            .collect::<Vec<_>>();
        if model_names.is_empty() {
            continue;
        }
        let parsed_changes = reference_re
            .captures_iter(fields[3])
            .filter_map(|reference| color_changes.get(&reference[1]).cloned())
            .collect::<Vec<_>>();
        let Some(color_index_channel) =
            parse_cpp_u32(fields[4]).and_then(|value| u8::try_from(value).ok())
        else {
            continue;
        };
        models.insert(
            captures[1].to_string(),
            ParsedNpcModelData {
                joints: [parse_npc_joint(fields[0]), parse_npc_joint(fields[1])],
                model_names,
                color_changes: parsed_changes,
                color_index_channel,
                uses_pollution: parse_cpp_u32(fields[5]).is_some_and(|value| value != 0),
                uses_shared_materials: parse_cpp_u32(fields[6]).is_some_and(|value| value != 0),
            },
        );
    }
    models
}

fn parse_npc_joint(field: &str) -> Option<String> {
    if field.contains("cNpcPartsNameRootJoint") || field.trim() == "0" || field.trim() == "nullptr"
    {
        return None;
    }
    parse_cpp_string(field)
}

fn parse_cpp_string(field: &str) -> Option<String> {
    let start = field.find('"')? + 1;
    let end = field[start..].find('"')? + start;
    Some(field[start..end].to_string())
}

fn cpp_identifier(field: &str) -> Option<&str> {
    let field = field.trim().trim_start_matches('&').trim();
    (!field.is_empty() && field != "nullptr" && field != "0").then_some(field)
}

fn split_cpp_initializer_fields(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut fields = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            b'"' | b'\'' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            b',' if depth == 0 => {
                fields.push(body[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    let tail = body[start..].trim();
    if !tail.is_empty() {
        fields.push(tail);
    }
    fields
}

fn parse_cpp_u32(value: &str) -> Option<u32> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

fn extract_map_static_models(text: &str, source_file: &str) -> Vec<MapStaticModelDefinition> {
    let table_re = Regex::new(
        r"static\s+const\s+TMapStaticObj::ActorDataTableEntry\s+actor_data_table\s*\[\s*\]\s*=\s*\{",
    )
    .expect("valid map-static table regex");
    let Some(table) = table_re.find(text) else {
        return Vec::new();
    };
    let Some(body) = braced_body(text, table.end() - 1) else {
        return Vec::new();
    };
    let entry_re = Regex::new(r"\{([^{}]+)\}").expect("valid map-static entry regex");
    let mut models = Vec::new();

    for entry in entry_re.captures_iter(body) {
        let fields = split_cpp_initializer_fields(&entry[1]);
        if fields.len() < 17 {
            continue;
        }
        let Some(actor_name) = parse_cpp_string(fields[0]) else {
            continue;
        };
        let Some(load_flags) = parse_cpp_u32(fields[9]) else {
            continue;
        };
        let Some(resource_flags) = parse_cpp_u32(fields[16]) else {
            continue;
        };
        let directory = if resource_flags & 0x2 != 0 {
            "/common/map"
        } else if resource_flags & 0x4 != 0 {
            "/scene/mapObj"
        } else {
            "/scene/map/map"
        };
        let model_path =
            parse_cpp_string(fields[8]).map(|model_name| format!("{directory}/{model_name}.bmd"));
        models.push(MapStaticModelDefinition {
            actor_name,
            model_path,
            load_flags,
            source_file: source_file.to_string(),
            stage_bootstrap_created: false,
        });
    }

    models
}

fn extract_stage_bootstrap_map_static_actors(text: &str) -> Vec<String> {
    let function_re = Regex::new(r"static\s+void\s+initStageCommon\s*\([^)]*\)\s*\{")
        .expect("valid stage-bootstrap function regex");
    let Some(function) = function_re.find(text) else {
        return Vec::new();
    };
    let Some(body) = braced_body(text, function.end() - 1) else {
        return Vec::new();
    };
    let init_re = Regex::new(r#"->\s*init\s*\(\s*"([^"]+)"\s*\)"#)
        .expect("valid runtime map-static init regex");
    init_re
        .captures_iter(body)
        .map(|captures| captures[1].to_string())
        .collect()
}

fn extract_particle_resources(text: &str, source_file: &str, registry: &mut ObjectRegistry) {
    let load_re = Regex::new(
        r#"(?:gpResourceManager|[A-Za-z_][A-Za-z0-9_]*ResourceManager)\s*->\s*load\s*\(\s*\"([^\"]+\.jpa)\"\s*,\s*(0[xX][0-9A-Fa-f]+|[0-9]+)"#,
    )
    .expect("valid particle resource regex");
    for captures in load_re.captures_iter(text) {
        let Some(effect_id) = parse_cpp_u16(&captures[2]) else {
            continue;
        };
        registry
            .particle_resources
            .push(ParticleResourceDefinition {
                effect_id,
                path: captures[1].to_string(),
                source_file: source_file.to_string(),
            });
    }
}

fn extract_calc_particle_bindings(text: &str, source_file: &str, registry: &mut ObjectRegistry) {
    let calc_re = Regex::new(r"([A-Za-z_][A-Za-z0-9_:]*)::calc\s*\([^)]*\)\s*(?:const\s*)?\{")
        .expect("valid calc method regex");
    let matrix_re = Regex::new(
        r"(?:MtxPtr|Mtx\s*\*)\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[^;]*mNodeMatrices\s*\[\s*([0-9]+)\s*\]",
    )
    .expect("valid particle matrix regex");
    let emit_re = Regex::new(
        r"emitAndBind(ToPosPtr|ToMtxPtr|ToSRTMtxPtr|ToMtx)\s*\(\s*(0[xX][0-9A-Fa-f]+|[0-9]+)\s*,\s*([^,\n]+)",
    )
    .expect("valid actor particle emission regex");
    let direct_joint_re =
        Regex::new(r"mNodeMatrices\s*\[\s*([0-9]+)\s*\]").expect("valid direct joint regex");

    for captures in calc_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let matrix_joints = matrix_re
            .captures_iter(body)
            .filter_map(|captures| {
                Some((captures[1].to_string(), captures[2].parse::<usize>().ok()?))
            })
            .collect::<BTreeMap<_, _>>();
        for emission in emit_re.captures_iter(body) {
            let Some(effect_id) = parse_cpp_u16(&emission[2]) else {
                continue;
            };
            let target = if &emission[1] == "ToPosPtr" {
                Some(ParticleBindingTarget::ActorOrigin)
            } else {
                let argument = emission[3].trim();
                matrix_joints
                    .get(argument)
                    .copied()
                    .map(ParticleBindingTarget::ModelJoint)
                    .or_else(|| {
                        direct_joint_re
                            .captures(argument)
                            .and_then(|captures| captures[1].parse::<usize>().ok())
                            .map(ParticleBindingTarget::ModelJoint)
                    })
            };
            let Some(target) = target else {
                continue;
            };
            registry.actor_particle_bindings.push(ActorParticleBinding {
                class_name: captures[1].to_string(),
                effect_id,
                target,
                source_file: source_file.to_string(),
            });
        }
    }
}

fn parse_cpp_u16(value: &str) -> Option<u16> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

fn extract_enemy_factory_variants(text: &str, registry: &mut ObjectRegistry) {
    let factory_re = Regex::new(
        r#"(?s)if\s*\(\s*strcmp\s*\(\s*name\s*,\s*\"([^\"]+)\"\s*\)\s*==\s*0\s*\)\s*(?:\{\s*)?return\s+(?:[A-Za-z0-9_:]+\s*=\s*)?new\s+([A-Za-z_:][A-Za-z0-9_:]*)\s*(?:\(\s*([0-9]+)\s*,)?"#,
    )
    .expect("valid enemy factory variant regex");

    for captures in factory_re.captures_iter(text) {
        let factory_name = captures[1].to_string();
        let class_name = captures[2].to_string();
        let model_index = captures
            .get(3)
            .and_then(|capture| capture.as_str().parse::<usize>().ok());
        if class_name
            .rsplit("::")
            .next()
            .is_some_and(|name| name.ends_with("Manager"))
        {
            registry.enemy_managers.push(EnemyManagerDefinition {
                factory_name,
                class_name,
                model_index,
                spawned_actor_class: None,
                parameter_path: None,
                models: Vec::new(),
            });
        } else {
            registry.enemy_actors.push(EnemyActorDefinition {
                factory_name,
                class_name,
                model_index,
                fallback_models: Vec::new(),
                primary_model: None,
                named_models: Vec::new(),
                indexed_models: Vec::new(),
                manager_factories: Vec::new(),
                runtime_uniform_scale: None,
            });
        }
    }
}

fn extract_class_inheritance(text: &str, inheritance: &mut BTreeMap<String, String>) {
    let class_re = Regex::new(
        r"class\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*public\s+(?:virtual\s+)?([A-Za-z_:][A-Za-z0-9_:]*)",
    )
    .expect("valid class inheritance regex");
    for captures in class_re.captures_iter(text) {
        inheritance.insert(captures[1].to_string(), captures[2].to_string());
    }
}

fn extract_enemy_manager_parameter_paths(text: &str) -> Vec<(String, String)> {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::load\s*\([^)]*\)\s*\{")
        .expect("valid enemy manager parameter method regex");
    let path_re = Regex::new(r#"new\s+[A-Za-z_][A-Za-z0-9_:<>]*\s*\(\s*\"([^\"]+\.prm)\"\s*\)"#)
        .expect("valid enemy manager parameter path regex");
    method_re
        .captures_iter(text)
        .filter_map(|method| {
            let whole = method.get(0)?;
            let body = braced_body(text, whole.end() - 1)?;
            let path = path_re.captures(body)?;
            Some((method[1].to_string(), path[1].to_string()))
        })
        .collect()
}

fn extract_runtime_map_obj_dependencies(
    text: &str,
    source_file: &str,
) -> Vec<(String, Vec<(String, String)>)> {
    let method_re = Regex::new(
        r"(?m)^[^\r\n{};]*\b([A-Za-z_][A-Za-z0-9_]*)::[A-Za-z_][A-Za-z0-9_]*\s*\([^;{}]*\)\s*(?:const\s*)?\{",
    )
    .expect("valid runtime map-object method regex");
    let dependency_re = Regex::new(r#"(?:newAndRegisterObj)\s*\(\s*"([^"]+)""#)
        .expect("valid runtime map-object dependency regex");
    let mut by_class = BTreeMap::<String, BTreeSet<(String, String)>>::new();
    for method in method_re.captures_iter(text) {
        let Some(whole) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole.end() - 1) else {
            continue;
        };
        by_class.entry(method[1].to_string()).or_default().extend(
            dependency_re
                .captures_iter(body)
                .map(|dependency| (dependency[1].to_string(), source_file.to_string())),
        );
    }
    by_class
        .into_iter()
        .map(|(class_name, dependencies)| (class_name, dependencies.into_iter().collect()))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExtractedRuntimeNameReference {
    target: RuntimeNameReferenceTarget,
    required: bool,
    record_name: String,
    source_file: String,
}

fn extract_runtime_name_references(
    text: &str,
    source_file: &str,
) -> Vec<(String, Vec<ExtractedRuntimeNameReference>)> {
    let method_re = Regex::new(
        r"(?m)^[^\r\n{};]*\b([A-Za-z_][A-Za-z0-9_]*)::[A-Za-z_][A-Za-z0-9_]*\s*\([^;{}]*\)\s*(?:const\s*)?\{",
    )
    .expect("valid runtime name-reference method regex");
    let shine_demo_re =
        Regex::new(r#"(?s)makeShineAppearWithDemo(?:Offset)?\s*\(\s*"([^"]+)"\s*,\s*"([^"]+)""#)
            .expect("valid Shine demo dependency regex");
    let shine_time_re = Regex::new(r#"(?s)makeShineAppearWithTime(?:Offset)?\s*\(\s*"([^"]+)""#)
        .expect("valid timed Shine dependency regex");
    let mut by_class = BTreeMap::<String, BTreeSet<ExtractedRuntimeNameReference>>::new();
    let nerve_re = Regex::new(r"(?m)^\s*DEFINE_NERVE\s*\([^)]*\)\s*\{")
        .expect("valid nerve implementation regex");
    let nerve_body_re = Regex::new(
        r"([A-Za-z_][A-Za-z0-9_]*)\s*\*\s*[A-Za-z_][A-Za-z0-9_]*\s*=\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\*\s*\)\s*spine->getBody\s*\(",
    )
    .expect("valid nerve body actor regex");

    for method in method_re.captures_iter(text) {
        let Some(whole) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole.end() - 1) else {
            continue;
        };
        let references = by_class.entry(method[1].to_string()).or_default();
        for call in shine_demo_re.captures_iter(body) {
            references.insert(ExtractedRuntimeNameReference {
                target: RuntimeNameReferenceTarget::Actor {
                    factory_name: "Shine".to_string(),
                },
                required: false,
                record_name: call[1].to_string(),
                source_file: source_file.to_string(),
            });
            references.insert(ExtractedRuntimeNameReference {
                target: RuntimeNameReferenceTarget::PlacementRecord {
                    type_name: "CameraMapInfo".to_string(),
                },
                required: false,
                record_name: call[2].to_string(),
                source_file: source_file.to_string(),
            });
        }
        for call in shine_time_re.captures_iter(body) {
            references.insert(ExtractedRuntimeNameReference {
                target: RuntimeNameReferenceTarget::Actor {
                    factory_name: "Shine".to_string(),
                },
                required: false,
                record_name: call[1].to_string(),
                source_file: source_file.to_string(),
            });
        }
    }

    // State-machine implementations are emitted through DEFINE_NERVE rather than
    // ordinary Class::method definitions. Attribute a name lookup to the actor
    // type recovered from the nerve body's checked getBody cast.
    for nerve in nerve_re.find_iter(text) {
        let Some(body) = braced_body(text, nerve.end() - 1) else {
            continue;
        };
        let Some(actor) = nerve_body_re.captures(body) else {
            continue;
        };
        if actor[1] != actor[2] {
            continue;
        }
        let references = by_class.entry(actor[1].to_string()).or_default();
        for call in shine_demo_re.captures_iter(body) {
            references.insert(ExtractedRuntimeNameReference {
                target: RuntimeNameReferenceTarget::Actor {
                    factory_name: "Shine".to_string(),
                },
                required: true,
                record_name: call[1].to_string(),
                source_file: source_file.to_string(),
            });
            references.insert(ExtractedRuntimeNameReference {
                target: RuntimeNameReferenceTarget::PlacementRecord {
                    type_name: "CameraMapInfo".to_string(),
                },
                required: true,
                record_name: call[2].to_string(),
                source_file: source_file.to_string(),
            });
        }
        for call in shine_time_re.captures_iter(body) {
            references.insert(ExtractedRuntimeNameReference {
                target: RuntimeNameReferenceTarget::Actor {
                    factory_name: "Shine".to_string(),
                },
                required: true,
                record_name: call[1].to_string(),
                source_file: source_file.to_string(),
            });
        }
    }
    by_class
        .into_iter()
        .map(|(class_name, references)| (class_name, references.into_iter().collect()))
        .collect()
}

fn extract_enemy_manager_animation_folders(
    text: &str,
    source_file: &str,
) -> Vec<(String, Vec<(String, String)>)> {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::createAnmData\s*\([^)]*\)\s*\{")
        .expect("valid enemy manager animation method regex");
    let folder_re = Regex::new(r#"(?:->|\.)init\s*\(\s*"(/scene/[^"]+)"\s*,\s*nullptr"#)
        .expect("valid enemy manager animation folder regex");
    method_re
        .captures_iter(text)
        .filter_map(|method| {
            let whole = method.get(0)?;
            let body = braced_body(text, whole.end() - 1)?;
            let folders = folder_re
                .captures_iter(body)
                .map(|folder| (folder[1].to_string(), source_file.to_string()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            Some((method[1].to_string(), folders))
        })
        .collect()
}

fn extract_enemy_runtime_uniform_scales(
    text: &str,
    source_file: &str,
) -> Vec<(String, EnemyRuntimeUniformScaleDefinition)> {
    let alias_re =
        Regex::new(r"(?m)^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_]*)\.get\(\)\s*;")
            .expect("valid enemy scale parameter alias regex");
    let aliases = alias_re
        .captures_iter(text)
        .map(|capture| (capture[1].to_string(), capture[2].to_string()))
        .collect::<BTreeMap<_, _>>();
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::reset\s*\([^)]*\)\s*\{")
        .expect("valid enemy reset method regex");
    let uniform_re = Regex::new(
        r"mScaling\.set\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .expect("valid enemy uniform scale regex");
    let mut definitions = Vec::new();
    for method in method_re.captures_iter(text) {
        let Some(whole) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole.end() - 1) else {
            continue;
        };
        let Some(uniform) = uniform_re.captures(body) else {
            continue;
        };
        if uniform[1] != uniform[2] || uniform[1] != uniform[3] {
            continue;
        }
        let scale_member = regex::escape(&uniform[1]);
        let range_re = Regex::new(&format!(
            r"(?s)\b{scale_member}\s*=\s*[A-Za-z_][A-Za-z0-9_]*\s*\([^;]*?->\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*[^;]*?->\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*;"
        ))
        .expect("valid enemy scale range regex");
        let Some(range) = range_re.captures(body) else {
            continue;
        };
        let Some(low_parameter) = aliases.get(&range[1]) else {
            continue;
        };
        let Some(high_parameter) = aliases.get(&range[2]) else {
            continue;
        };
        definitions.push((
            method[1].to_string(),
            EnemyRuntimeUniformScaleDefinition {
                low_parameter: low_parameter.clone(),
                high_parameter: high_parameter.clone(),
                source_file: source_file.to_string(),
            },
        ));
    }
    definitions
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnemyTevColorBinding {
    actor_class: String,
    material_name: String,
    tev_register: u8,
    color_member: String,
    source_file: String,
}

fn extract_enemy_tev_color_bindings(text: &str, source_file: &str) -> Vec<EnemyTevColorBinding> {
    let method_re = Regex::new(
        r"(?m)^[ \t]*[^/\n;{}]+::[A-Za-z_][A-Za-z0-9_]*[ \t]*\([^;{}]*\)(?:[ \t]*const)?[ \t\r\n]*\{",
    )
    .expect("valid enemy method regex");
    let material_re = Regex::new(
        r#"(?s)(?:u16|u32|s16|s32|int)\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[^;]{0,512}?getIndex\s*\(\s*"([^"]+)"\s*\)\s*;"#,
    )
    .expect("valid enemy material-index regex");
    let actor_re = Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*\*\s*([A-Za-z_][A-Za-z0-9_]*)\s*=")
        .expect("valid enemy actor-variable regex");
    let binding_re = Regex::new(
        r"(?s)SMS_InitPacket_OneTevColor\s*\([^;]*?,\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*GX_TEVREG([0-3])\s*,\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*->\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*;",
    )
    .expect("valid enemy TEV-color binding regex");
    let mut bindings = Vec::new();

    for method in method_re.find_iter(text) {
        let Some(body) = braced_body(text, method.end() - 1) else {
            continue;
        };
        let material_vars = material_re
            .captures_iter(body)
            .map(|captures| (captures[1].to_string(), captures[2].to_string()))
            .collect::<BTreeMap<_, _>>();
        let actor_vars = actor_re
            .captures_iter(body)
            .map(|captures| (captures[2].to_string(), captures[1].to_string()))
            .collect::<BTreeMap<_, _>>();

        for captures in binding_re.captures_iter(body) {
            let (Some(material_name), Some(actor_class), Ok(tev_register)) = (
                material_vars.get(&captures[1]),
                actor_vars.get(&captures[3]),
                captures[2].parse::<u8>(),
            ) else {
                continue;
            };
            bindings.push(EnemyTevColorBinding {
                actor_class: actor_class.clone(),
                material_name: material_name.clone(),
                tev_register,
                color_member: captures[4].to_string(),
                source_file: source_file.to_string(),
            });
        }
    }
    bindings
}

fn extract_enemy_init_tev_colors(
    text: &str,
    colors: &mut BTreeMap<(String, String), [Option<i16>; 4]>,
) {
    let init_re = Regex::new(r"(?:void\s+)?([A-Za-z_][A-Za-z0-9_]*)::init\s*\([^;{}]*\)\s*\{")
        .expect("valid enemy init-method regex");
    let assignment_re = Regex::new(
        r"([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*([rgba])\s*=\s*(-?(?:0[xX][0-9A-Fa-f]+|[0-9]+))\s*;",
    )
    .expect("valid enemy color-assignment regex");

    for init in init_re.captures_iter(text) {
        let Some(whole_match) = init.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        for assignment in assignment_re.captures_iter(body) {
            let Some(value) = parse_cpp_i16(&assignment[3]) else {
                continue;
            };
            let channel = match &assignment[2] {
                "r" => 0,
                "g" => 1,
                "b" => 2,
                "a" => 3,
                _ => continue,
            };
            colors
                .entry((init[1].to_string(), assignment[1].to_string()))
                .or_insert([None; 4])[channel] = Some(value);
        }
    }
}

fn parse_cpp_i16(value: &str) -> Option<i16> {
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok()?
    } else {
        value.parse::<i64>().ok()?
    };
    if !(-32768..=65535).contains(&parsed) {
        return None;
    }
    Some((parsed as u16) as i16)
}

fn derive_enemy_material_tev_colors(
    actors: &[EnemyActorDefinition],
    bindings: &[EnemyTevColorBinding],
    init_colors: &BTreeMap<(String, String), [Option<i16>; 4]>,
    inheritance: &BTreeMap<String, String>,
) -> Vec<EnemyMaterialTevColorDefinition> {
    let mut definitions =
        BTreeMap::<(String, String, u8), (usize, EnemyMaterialTevColorDefinition)>::new();
    for actor in actors {
        for binding in bindings {
            let Some(distance) =
                inheritance_distance(&actor.class_name, &binding.actor_class, inheritance)
            else {
                continue;
            };
            let Some(color) = inherited_enemy_init_color(
                &actor.class_name,
                &binding.color_member,
                init_colors,
                inheritance,
            ) else {
                continue;
            };
            let key = (
                actor.factory_name.clone(),
                binding.material_name.clone(),
                binding.tev_register,
            );
            let definition = EnemyMaterialTevColorDefinition {
                factory_name: actor.factory_name.clone(),
                material_name: binding.material_name.clone(),
                tev_register: binding.tev_register,
                color,
                source_file: binding.source_file.clone(),
            };
            match definitions.get(&key) {
                Some((existing_distance, _)) if *existing_distance <= distance => {}
                _ => {
                    definitions.insert(key, (distance, definition));
                }
            }
        }
    }
    definitions
        .into_values()
        .map(|(_, definition)| definition)
        .collect()
}

fn inherited_enemy_init_color(
    class_name: &str,
    member_name: &str,
    init_colors: &BTreeMap<(String, String), [Option<i16>; 4]>,
    inheritance: &BTreeMap<String, String>,
) -> Option<[Option<i16>; 4]> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = std::collections::BTreeSet::new();
    let mut chain = Vec::new();
    while visited.insert(current.to_string()) {
        chain.push(current.to_string());
        let Some(parent) = inheritance.get(current) else {
            break;
        };
        current = parent.rsplit("::").next().unwrap_or(parent);
    }

    let mut color = [None; 4];
    let mut found = false;
    for class in chain.iter().rev() {
        let Some(assignments) = init_colors.get(&(class.clone(), member_name.to_string())) else {
            continue;
        };
        for (target, source) in color.iter_mut().zip(assignments) {
            if source.is_some() {
                *target = *source;
                found = true;
            }
        }
    }
    found.then_some(color)
}

fn extract_enemy_manager_actor_classes(
    text: &str,
    manager_actor_classes: &mut BTreeMap<String, String>,
) {
    let method_re = Regex::new(
        r"[A-Za-z_][A-Za-z0-9_:]*\s*\*\s*([A-Za-z_][A-Za-z0-9_]*Manager)::createEnemyInstance\s*\([^)]*\)\s*\{",
    )
    .expect("valid enemy manager factory method regex");
    let return_re = Regex::new(r"return\s+new\s+([A-Za-z_][A-Za-z0-9_]*)\b")
        .expect("valid enemy manager actor return regex");
    for method in method_re.captures_iter(text) {
        let Some(whole_match) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let Some(actor) = return_re.captures(body) else {
            continue;
        };
        manager_actor_classes
            .entry(method[1].to_string())
            .or_insert_with(|| actor[1].to_string());
    }
}

fn inherited_actor_class(
    manager_class: &str,
    manager_actor_classes: &BTreeMap<String, String>,
    inheritance: &BTreeMap<String, String>,
) -> Option<String> {
    let mut current = manager_class.rsplit("::").next().unwrap_or(manager_class);
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(actor_class) = manager_actor_classes.get(current) {
            return Some(actor_class.clone());
        }
        current = inheritance.get(current)?.rsplit("::").next()?;
    }
    None
}

fn class_is_or_inherits(
    class_name: &str,
    expected_base: &str,
    inheritance: &BTreeMap<String, String>,
) -> bool {
    inheritance_distance(class_name, expected_base, inheritance).is_some()
}

fn inheritance_distance(
    class_name: &str,
    expected_base: &str,
    inheritance: &BTreeMap<String, String>,
) -> Option<usize> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let expected_base = expected_base.rsplit("::").next().unwrap_or(expected_base);
    let mut visited = std::collections::BTreeSet::new();
    let mut distance = 0;
    while visited.insert(current.to_string()) {
        if current == expected_base {
            return Some(distance);
        }
        let parent = inheritance.get(current)?;
        current = parent.rsplit("::").next().unwrap_or(parent);
        distance += 1;
    }
    None
}

fn factory_without_manager(factory_name: &str) -> &str {
    factory_name.strip_suffix("Manager").unwrap_or(factory_name)
}

fn compatible_enemy_managers(
    actor: &EnemyActorDefinition,
    managers: &[EnemyManagerDefinition],
    inheritance: &BTreeMap<String, String>,
) -> Vec<String> {
    let primary_model_manager = unique_primary_model_manager(actor, managers);
    let mut candidates = managers
        .iter()
        .filter(|manager| {
            if actor.model_index.is_some()
                && manager.model_index.is_some()
                && actor.model_index != manager.model_index
            {
                return false;
            }
            factory_without_manager(&manager.factory_name) == actor.factory_name
                || manager_actor_class(manager).is_some_and(|spawned_class| {
                    class_is_or_inherits(&actor.class_name, spawned_class, inheritance)
                })
                || primary_model_manager == Some(manager.factory_name.as_str())
        })
        .map(|manager| {
            let exact_factory =
                factory_without_manager(&manager.factory_name) == actor.factory_name;
            let actor_distance = manager_actor_class(manager)
                .and_then(|spawned_class| {
                    inheritance_distance(&actor.class_name, spawned_class, inheritance)
                })
                .unwrap_or(usize::MAX);
            let primary_model_match = primary_model_manager == Some(manager.factory_name.as_str());
            (
                (
                    !exact_factory,
                    actor_distance,
                    !primary_model_match,
                    manager.factory_name.clone(),
                ),
                manager.factory_name.clone(),
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates
        .into_iter()
        .map(|(_, factory_name)| factory_name)
        .collect()
}

fn unique_primary_model_manager<'a>(
    actor: &EnemyActorDefinition,
    managers: &'a [EnemyManagerDefinition],
) -> Option<&'a str> {
    let primary_model = actor.primary_model.as_ref()?;
    let mut matches = managers.iter().filter(|manager| {
        manager
            .models
            .iter()
            .any(|model| model.model_name.eq_ignore_ascii_case(primary_model))
    });
    let manager = matches.next()?;
    matches
        .next()
        .is_none()
        .then_some(manager.factory_name.as_str())
}

fn manager_actor_class(manager: &EnemyManagerDefinition) -> Option<&str> {
    manager.spawned_actor_class.as_deref().or_else(|| {
        manager
            .class_name
            .rsplit("::")
            .next()
            .and_then(|class_name| class_name.strip_suffix("Manager"))
    })
}

fn extract_enemy_manager_models(
    text: &str,
    source_file: &str,
    flag_symbols: &BTreeMap<String, u32>,
) -> Vec<(String, Vec<EnemyModelDefinition>)> {
    let constants = extract_cpp_string_constants(text);
    let all_tables = extract_model_data_tables(text, source_file, &constants, flag_symbols);
    let method_re =
        Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*Manager)::createModelData\s*\([^)]*\)\s*\{")
            .expect("valid enemy createModelData regex");
    let reference_re = Regex::new(r"createModelDataArray\s*\(\s*&?\s*([A-Za-z_][A-Za-z0-9_]*)")
        .expect("valid model-data reference regex");
    let mut managers = Vec::new();

    for captures in method_re.captures_iter(text) {
        let Some(method_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, method_match.end() - 1) else {
            continue;
        };
        let local_tables = extract_model_data_tables(body, source_file, &constants, flag_symbols);
        let local_names = local_tables
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let mut models = local_tables
            .into_iter()
            .flat_map(|(_, models)| models)
            .collect::<Vec<_>>();
        for reference in reference_re.captures_iter(body) {
            let name = &reference[1];
            if local_names.iter().any(|local| local == name) {
                continue;
            }
            for (_, referenced_models) in all_tables.iter().filter(|(table, _)| table == name) {
                models.extend(referenced_models.iter().cloned());
            }
        }
        models.retain(|model| {
            let lower = model.model_name.to_ascii_lowercase();
            lower.ends_with(".bmd") || lower.ends_with(".bdl")
        });
        models.dedup_by(|left, right| {
            left.model_name.eq_ignore_ascii_case(&right.model_name)
                && left.load_flags == right.load_flags
        });
        if !models.is_empty() {
            managers.push((captures[1].to_string(), models));
        }
    }
    managers
}

fn extract_npc_manager_resource_bases(text: &str) -> BTreeMap<String, String> {
    let method_re =
        Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*Manager)::createModelData\s*\([^)]*\)\s*\{")
            .expect("valid NPC createModelData regex");
    let base_re =
        Regex::new(r#"createModelDataArrayBase\s*\(\s*[A-Za-z_][A-Za-z0-9_]*\s*,\s*"([^"]+)""#)
            .expect("valid NPC resource-base regex");
    let mut bases = BTreeMap::new();

    for captures in method_re.captures_iter(text) {
        let Some(method_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, method_match.end() - 1) else {
            continue;
        };
        if let Some(resource_base) = base_re.captures(body) {
            bases.insert(captures[1].to_string(), resource_base[1].to_string());
        }
    }

    bases
}

fn inherited_string_value(
    class_name: &str,
    values: &BTreeMap<String, String>,
    inheritance: &BTreeMap<String, String>,
) -> Option<String> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(value) = values.get(current) {
            return Some(value.clone());
        }
        current = inheritance.get(current)?.rsplit("::").next()?;
    }
    None
}

fn extract_cpp_string_constants(text: &str) -> BTreeMap<String, String> {
    let constant_re = Regex::new(
        r#"(?:static\s+)?(?:const\s+)?char\s*(?:\*\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*(?:\[[^\]]*\])?\s*=\s*\"([^\"]+)\""#,
    )
    .expect("valid C++ string constant regex");
    constant_re
        .captures_iter(text)
        .map(|captures| (captures[1].to_string(), captures[2].to_string()))
        .collect()
}

fn extract_model_data_tables(
    text: &str,
    source_file: &str,
    constants: &BTreeMap<String, String>,
    flag_symbols: &BTreeMap<String, u32>,
) -> Vec<(String, Vec<EnemyModelDefinition>)> {
    let declaration_re = Regex::new(
        r"(?:static\s+)?(?:const\s+)?TModelDataLoadEntry\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:\[[^\]]*\])?\s*=\s*\{",
    )
    .expect("valid model-data declaration regex");
    let entry_re = Regex::new(r"\{([^{}]+)\}").expect("valid model-data entry regex");
    let mut tables = Vec::new();

    for captures in declaration_re.captures_iter(text) {
        let Some(declaration) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, declaration.end() - 1) else {
            continue;
        };
        let nested_entries = entry_re
            .captures_iter(body)
            .map(|entry| entry[1].to_string())
            .collect::<Vec<_>>();
        let entries = if nested_entries.is_empty() {
            vec![body.to_string()]
        } else {
            nested_entries
        };
        let mut models = Vec::new();
        for entry in entries {
            let fields = split_cpp_initializer_fields(&entry);
            let (Some(name_field), Some(flags_field)) = (fields.first(), fields.get(1)) else {
                continue;
            };
            let model_name = parse_cpp_string(name_field).or_else(|| {
                cpp_identifier(name_field).and_then(|name| constants.get(name).cloned())
            });
            let (Some(model_name), Some(load_flags)) = (
                model_name,
                parse_cpp_u32_expression(flags_field, flag_symbols),
            ) else {
                continue;
            };
            models.push(EnemyModelDefinition {
                model_name,
                load_flags,
                source_file: source_file.to_string(),
            });
        }
        tables.push((captures[1].to_string(), models));
    }
    tables
}

fn extract_collision_surfaces(text: &str, source_file: &str) -> Vec<CollisionSurfaceDefinition> {
    let declaration = Regex::new(r"enum\s+BGTypeBits\s*\{")
        .expect("valid BGTypeBits declaration regex")
        .find(text);
    let Some(declaration) = declaration else {
        return Vec::new();
    };
    let Some(body) = braced_body(text, declaration.end() - 1) else {
        return Vec::new();
    };
    let block_comments = Regex::new(r"(?s)/\*.*?\*/").expect("valid block-comment regex");
    let line_comments = Regex::new(r"//[^\r\n]*").expect("valid line-comment regex");
    let without_comments = block_comments.replace_all(body, " ");
    let without_comments = line_comments.replace_all(&without_comments, " ");
    let name_re = Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)$").expect("valid identifier regex");

    let mut symbols = BTreeMap::new();
    let mut definitions = Vec::new();
    for entry in without_comments.split(',') {
        let Some((raw_name, raw_expression)) = entry.split_once('=') else {
            continue;
        };
        let name = raw_name.split_whitespace().last().unwrap_or_default();
        if !name_re.is_match(name) {
            continue;
        }
        let Some(value) = parse_cpp_u32_expression(raw_expression.trim(), &symbols) else {
            continue;
        };
        symbols.insert(name.to_string(), value);
        if !(name.starts_with("BG_TYPE_") || name.starts_with("BG_PROPERTY_FLAG_")) {
            continue;
        }
        let Ok(value) = u16::try_from(value) else {
            continue;
        };
        definitions.push(CollisionSurfaceDefinition {
            name: name.to_string(),
            value,
            is_property_flag: name.starts_with("BG_PROPERTY_FLAG_"),
            source_file: source_file.to_string(),
        });
    }
    definitions
}

fn extract_moving_collision_vertex_limit(text: &str) -> Option<u16> {
    let array_re = Regex::new(r"\bVec\s+[A-Za-z_][A-Za-z0-9_]*\s*\[\s*([0-9]+)\s*\]")
        .expect("valid moving-collision stack-array regex");
    let values = array_re
        .captures_iter(text)
        .filter_map(|captures| captures[1].parse::<u16>().ok())
        .collect::<BTreeSet<_>>();
    match values.into_iter().collect::<Vec<_>>().as_slice() {
        [value] => Some(*value),
        _ => None,
    }
}

fn extract_cpp_u32_constants(text: &str) -> BTreeMap<String, u32> {
    let constant_re = Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(0[xX][0-9A-Fa-f]+|[0-9]+)")
        .expect("valid numeric constant regex");
    constant_re
        .captures_iter(text)
        .filter_map(|captures| Some((captures[1].to_string(), parse_cpp_u32(&captures[2])?)))
        .collect()
}

fn parse_cpp_u32_expression(value: &str, symbols: &BTreeMap<String, u32>) -> Option<u32> {
    let shift_re = Regex::new(
        r"^\(?\s*(0[xX][0-9A-Fa-f]+|[0-9]+)\s*<<\s*([A-Za-z_][A-Za-z0-9_]*|[0-9]+)\s*\)?$",
    )
    .expect("valid loader-flag shift regex");
    let mut result = 0_u32;
    for term in value.split('|') {
        let term = term.trim();
        let parsed = parse_cpp_u32(term)
            .or_else(|| symbols.get(term).copied())
            .or_else(|| {
                let captures = shift_re.captures(term)?;
                let left = parse_cpp_u32(&captures[1])?;
                let right =
                    parse_cpp_u32(&captures[2]).or_else(|| symbols.get(&captures[2]).copied())?;
                left.checked_shl(right)
            })?;
        result |= parsed;
    }
    Some(result)
}

fn extract_enemy_actor_fallback_models(
    text: &str,
    source_file: &str,
) -> Vec<(String, Vec<EnemyModelDefinition>)> {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::init\s*\([^)]*\)\s*\{")
        .expect("valid enemy init regex");
    let flags_re = Regex::new(r"mModelLoaderFlags\s*=\s*(0[xX][0-9A-Fa-f]+|[0-9]+)")
        .expect("valid actor loader flags regex");
    let mut actors = Vec::new();
    for captures in method_re.captures_iter(text) {
        let Some(method_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, method_match.end() - 1) else {
            continue;
        };
        if !body.contains("createMActorFromDefaultBmd") {
            continue;
        }
        let Some(flags) = flags_re
            .captures(body)
            .and_then(|flags| parse_cpp_u32(&flags[1]))
        else {
            continue;
        };
        actors.push((
            captures[1].to_string(),
            vec![EnemyModelDefinition {
                model_name: "default.bmd".to_string(),
                load_flags: flags,
                source_file: source_file.to_string(),
            }],
        ));
    }
    actors
}

fn extract_enemy_actor_primary_models(text: &str) -> Vec<(String, String)> {
    let assignment_re = Regex::new(
        r#"(?m)^\s*mMActor\s*=\s*([^;\n]*(?:createMActor|getMActor)\s*\(\s*\"([^\"]+\.(?:bmd|bdl))\"[^;\n]*);"#,
    )
    .expect("valid primary actor model regex");
    let method_re = Regex::new(
        r"([A-Za-z_][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]*)\s*\([^;{}]*\)\s*(?:const\s*)?\{",
    )
    .expect("valid C++ method regex");
    let mut candidates = BTreeMap::<String, Vec<(u8, usize, String)>>::new();

    for assignment in assignment_re.captures_iter(text) {
        let Some(whole_match) = assignment.get(0) else {
            continue;
        };
        let Some(method) = method_re.captures_iter(&text[..whole_match.start()]).last() else {
            continue;
        };
        let method_name = &method[2];
        let is_creation = assignment[1].contains("createMActor");
        let method_priority = matches!(method_name, "init" | "load" | "loadAfter" | "initModel");
        let priority = match (is_creation, method_priority) {
            (true, true) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        };
        candidates.entry(method[1].to_string()).or_default().push((
            priority,
            whole_match.start(),
            assignment[2].to_string(),
        ));
    }

    candidates
        .into_iter()
        .filter_map(|(class_name, mut models)| {
            models.sort_by_key(|(priority, offset, _)| (*priority, *offset));
            Some((class_name, models.into_iter().next()?.2))
        })
        .collect()
}

fn extract_owned_actor_classes(
    text: &str,
    owned_actor_classes: &mut BTreeMap<String, Vec<String>>,
) {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::load\s*\([^)]*\)\s*\{")
        .expect("valid actor owner load regex");
    let owned_re = Regex::new(r"m[A-Za-z_][A-Za-z0-9_]*\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_]*)\b")
        .expect("valid owned actor regex");
    for method in method_re.captures_iter(text) {
        let Some(whole_match) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        for owned in owned_re.captures_iter(body) {
            let classes = owned_actor_classes
                .entry(method[1].to_string())
                .or_default();
            let class_name = owned[1].to_string();
            if !classes.contains(&class_name) {
                classes.push(class_name);
            }
        }
    }
}

fn extract_actor_root_parts(text: &str, actor_root_parts: &mut BTreeMap<String, String>) {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::init\s*\([^)]*\)\s*\{")
        .expect("valid actor root-part init regex");
    let root_re = Regex::new(r"mMActor\s*=\s*(m[A-Za-z_][A-Za-z0-9_]*)\s*->\s*mMActor")
        .expect("valid actor root-part assignment regex");
    for method in method_re.captures_iter(text) {
        let Some(whole_match) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let Some(root) = root_re.captures(body) else {
            continue;
        };
        let constructor_re = Regex::new(&format!(
            r"\b{}\s*=\s*new\s+([A-Za-z_][A-Za-z0-9_]*)\b",
            regex::escape(&root[1])
        ))
        .expect("valid actor root-part constructor regex");
        let Some(part_class) = constructor_re.captures(body) else {
            continue;
        };
        actor_root_parts.insert(method[1].to_string(), part_class[1].to_string());
    }
}

fn extract_part_model_indices(text: &str, part_model_indices: &mut BTreeMap<String, usize>) {
    let constructor_re = Regex::new(
        r"([A-Za-z_][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]*)\s*\([^;{}]*\)\s*:\s*[A-Za-z_][A-Za-z0-9_]*\s*\(([^)]*)\)",
    )
    .expect("valid part model constructor regex");
    for constructor in constructor_re.captures_iter(text) {
        if constructor[1] != constructor[2] {
            continue;
        }
        let Some(model_index) = split_cpp_initializer_fields(&constructor[3])
            .into_iter()
            .filter_map(parse_cpp_u32)
            .next_back()
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        part_model_indices.insert(constructor[1].to_string(), model_index);
    }
}

fn extract_cpp_string_arrays(text: &str) -> BTreeMap<String, Vec<String>> {
    let array_re = Regex::new(
        r"(?:static\s+)?const\s+char\s*\*\s*([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*\]\s*=\s*\{",
    )
    .expect("valid C++ string-array regex");
    let string_re = Regex::new(r#"\"([^\"]+)\""#).expect("valid C++ array string regex");
    let mut arrays = BTreeMap::new();
    for captures in array_re.captures_iter(text) {
        let Some(declaration) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, declaration.end() - 1) else {
            continue;
        };
        arrays.insert(
            captures[1].to_string(),
            string_re
                .captures_iter(body)
                .map(|value| value[1].to_string())
                .collect(),
        );
    }
    arrays
}

fn extract_enemy_named_models(
    text: &str,
    source_file: &str,
) -> Vec<(String, Vec<EnemyNamedModelDefinition>)> {
    let arrays = extract_cpp_string_arrays(text);
    let method_re =
        Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::[A-Za-z_][A-Za-z0-9_]*\s*\([^)]*\)\s*\{")
            .expect("valid named enemy model method regex");
    let name_array_re = Regex::new(
        r"strcmp\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*i\s*\]\s*,\s*[^;]*getName\s*\(\s*\)",
    )
    .expect("valid named enemy selector regex");
    let model_array_re =
        Regex::new(r"getGlbResource\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*modelType\s*\]\s*\)")
            .expect("valid named enemy resource regex");
    let selected_index_re =
        Regex::new(r"modelType\s*==\s*([0-9]+)").expect("valid named enemy index regex");
    let flags_re = Regex::new(
        r"J3DModelLoaderDataBase::load\s*\(\s*[A-Za-z_][A-Za-z0-9_]*\s*,\s*(0[xX][0-9A-Fa-f]+|[0-9]+)\s*\)",
    )
    .expect("valid named enemy loader flags regex");
    let mut actors = Vec::new();

    for method in method_re.captures_iter(text) {
        let Some(whole_match) = method.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let (Some(name_array), Some(model_array), Some(index), Some(load_flags)) = (
            name_array_re
                .captures(body)
                .map(|captures| captures[1].to_string()),
            model_array_re
                .captures(body)
                .map(|captures| captures[1].to_string()),
            selected_index_re
                .captures(body)
                .and_then(|captures| captures[1].parse::<usize>().ok()),
            flags_re
                .captures(body)
                .and_then(|captures| parse_cpp_u32(&captures[1])),
        ) else {
            continue;
        };
        let Some(actor_name) = arrays.get(&name_array).and_then(|values| values.get(index)) else {
            continue;
        };
        let Some(model_path) = arrays
            .get(&model_array)
            .and_then(|values| values.get(index))
        else {
            continue;
        };
        actors.push((
            method[1].to_string(),
            vec![EnemyNamedModelDefinition {
                actor_name: actor_name.clone(),
                model_path: model_path.clone(),
                load_flags,
                source_file: source_file.to_string(),
            }],
        ));
    }
    actors
}

fn extract_enemy_indexed_models(
    text: &str,
    source_file: &str,
) -> Vec<(String, Vec<EnemyIndexedModelDefinition>)> {
    let method_re = Regex::new(r"void\s+([A-Za-z_][A-Za-z0-9_]*)::load\s*\([^)]*\)\s*\{")
        .expect("valid indexed enemy load regex");
    let direct_model_re = Regex::new(
        r#"(?s)JKRGetResource\s*\(\s*\"([^\"]+\.(?:bmd|bdl))\"\s*\)\s*;.*?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*new\s+SDLModelData\s*\(\s*J3DModelLoaderDataBase::load\s*\([^,]+,\s*(0[xX][0-9A-Fa-f]+|[0-9]+)\s*\)\s*\)"#,
    )
    .expect("valid direct manager model regex");
    let default_model_re = Regex::new(
        r"SDLModelData\s*\*\s*[A-Za-z_][A-Za-z0-9_]*\s*=\s*\(\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\*\s*\)\s*mManager\s*\)\s*->\s*([A-Za-z_][A-Za-z0-9_]*)",
    )
    .expect("valid indexed enemy default model regex");
    let case_re = Regex::new(
        r#"(?s)case\s+([0-9]+)\s*:.*?JKRGetResource\s*\(\s*\"([^\"]+\.(?:bmd|bdl))\"\s*\).*?J3DModelLoaderDataBase::load\s*\([^,]+,\s*(0[xX][0-9A-Fa-f]+|[0-9]+)\s*\).*?break\s*;"#,
    )
    .expect("valid indexed enemy case regex");

    let mut direct_models = BTreeMap::<(String, String), EnemyIndexedModelDefinition>::new();
    let mut methods = Vec::new();
    for captures in method_re.captures_iter(text) {
        let Some(whole_match) = captures.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole_match.end() - 1) else {
            continue;
        };
        let class_name = captures[1].to_string();
        for model in direct_model_re.captures_iter(body) {
            let Some(load_flags) = parse_cpp_u32(&model[3]) else {
                continue;
            };
            direct_models.insert(
                (class_name.clone(), model[2].to_string()),
                EnemyIndexedModelDefinition {
                    index: 0,
                    model_path: model[1].to_string(),
                    load_flags,
                    source_file: source_file.to_string(),
                },
            );
        }
        methods.push((class_name, body));
    }

    let mut actors = Vec::new();
    for (class_name, body) in methods {
        if !body.contains("switch") {
            continue;
        }
        let Some(default_model) = default_model_re.captures(body) else {
            continue;
        };
        let Some(default) = direct_models
            .get(&(default_model[1].to_string(), default_model[2].to_string()))
            .cloned()
        else {
            continue;
        };
        let mut models = vec![default];
        models.extend(case_re.captures_iter(body).filter_map(|case| {
            Some(EnemyIndexedModelDefinition {
                index: case[1].parse().ok()?,
                model_path: case[2].to_string(),
                load_flags: parse_cpp_u32(&case[3])?,
                source_file: source_file.to_string(),
            })
        }));
        models.sort_by_key(|model| model.index);
        models.dedup_by_key(|model| model.index);
        if models.len() > 1 {
            actors.push((class_name, models));
        }
    }
    actors
}

fn inherited_actor_primary_model(
    class_name: &str,
    actor_models: &BTreeMap<String, String>,
    inheritance: &BTreeMap<String, String>,
) -> Option<String> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(model) = actor_models.get(current) {
            return Some(model.clone());
        }
        current = inheritance.get(current)?.rsplit("::").next()?;
    }
    None
}

fn inherited_actor_models<T: Clone>(
    class_name: &str,
    actor_models: &BTreeMap<String, Vec<T>>,
    inheritance: &BTreeMap<String, String>,
) -> Option<Vec<T>> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(models) = actor_models.get(current) {
            return Some(models.clone());
        }
        current = inheritance.get(current)?.rsplit("::").next()?;
    }
    None
}

fn inherited_actor_models_union<T: Clone + Ord>(
    class_name: &str,
    actor_models: &BTreeMap<String, Vec<T>>,
    inheritance: &BTreeMap<String, String>,
) -> Vec<T> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = BTreeSet::new();
    let mut models = BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(current_models) = actor_models.get(current) {
            models.extend(current_models.iter().cloned());
        }
        let Some(parent) = inheritance.get(current) else {
            break;
        };
        current = parent.rsplit("::").next().unwrap_or(parent);
    }
    models.into_iter().collect()
}

fn inherited_actor_value<T: Clone>(
    class_name: &str,
    values: &BTreeMap<String, T>,
    inheritance: &BTreeMap<String, String>,
) -> Option<T> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(value) = values.get(current) {
            return Some(value.clone());
        }
        current = inheritance.get(current)?.rsplit("::").next()?;
    }
    None
}

fn inherited_enemy_models(
    class_name: &str,
    class_models: &BTreeMap<String, Vec<EnemyModelDefinition>>,
    inheritance: &BTreeMap<String, String>,
) -> Option<Vec<EnemyModelDefinition>> {
    let mut current = class_name.rsplit("::").next().unwrap_or(class_name);
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.to_string()) {
        if let Some(models) = class_models.get(current) {
            return Some(models.clone());
        }
        current = inheritance.get(current)?.rsplit("::").next()?;
    }
    None
}

fn braced_body(text: &str, open_brace: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.get(open_brace).copied() != Some(b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut index = open_brace;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return text.get(open_brace + 1..index);
                }
            }
            b'"' | b'\'' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1).copied() == Some(b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1).copied() == Some(b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn extract_map_obj_flag_definition(
    source: &str,
    factories: &str,
    application: &str,
    source_file: &str,
) -> Option<MapObjFlagDefinition> {
    let class_name = Regex::new(r"f32\s+([A-Za-z_][A-Za-z0-9_:]*)::mFlutterSpeed\s*=")
        .expect("valid map-object flag class regex")
        .captures(source)?[1]
        .to_string();
    let factory_re = Regex::new(&format!(
        r#"strcmp\s*\(\s*name\s*,\s*"([^"]+)"\s*\)\s*==\s*0\s*\)\s*(?:\{{[^}}]*?)?return\s+new\s+{}\b"#,
        regex::escape(&class_name)
    ))
    .expect("valid map-object flag factory regex");
    let factory_name = factory_re.captures(factories)?[1].to_string();
    let texture_path_pattern = Regex::new(
        r#"snprintf\s*\([^;]*?"(/[^"]*%s[^"]*\.bti)"\s*,\s*[A-Za-z_][A-Za-z0-9_]*\s*\)"#,
    )
    .expect("valid map-object flag texture path regex")
    .captures(source)?[1]
        .to_string();
    let registered_texture_names = Regex::new(r#"REGISTER_FLAG\s*\(\s*\d+\s*,\s*"([^"]+)"\s*\)"#)
        .expect("valid registered map-object flag regex")
        .captures_iter(source)
        .map(|capture| capture[1].to_string())
        .collect::<Vec<_>>();
    if registered_texture_names.is_empty() {
        return None;
    }

    let resource_name_stream_index = extract_flag_resource_stream_index(source, &class_name)?;
    let (default_flutter_speed_degrees_per_frame, area_flutter_speeds) =
        extract_flag_flutter_speeds(source, &class_name)?;
    let phase_wrap_degrees = extract_flag_phase_wrap(source, &class_name)?;
    Some(MapObjFlagDefinition {
        factory_name,
        class_name,
        texture_path_pattern,
        registered_texture_names,
        resource_name_stream_index,
        default_height: extract_u32_member_assignment(source, "mFlagHeight")?,
        default_width: extract_u32_member_assignment(source, "mFlagWidth")?,
        default_segment_size: extract_u32_member_assignment(source, "mSegmentSize")?,
        default_flutter_speed_degrees_per_frame,
        area_flutter_speeds,
        phase_wrap_degrees,
        stage_archive_table_path: extract_stage_archive_table_path(application)?,
        source_file: source_file.to_string(),
    })
}

fn extract_flag_flutter_speeds(
    source: &str,
    class_name: &str,
) -> Option<(u32, Vec<MapObjFlagAreaSpeed>)> {
    let assignment = format!(r"{}::mFlutterSpeed\s*=\s*", regex::escape(class_name));
    let switch_re =
        Regex::new(r"switch\s*\([^)]*\)\s*\{").expect("valid flag flutter switch regex");
    let switch_body = switch_re.find_iter(source).find_map(|switch_match| {
        let body = braced_body(source, switch_match.end() - 1)?;
        body.contains("mFlutterSpeed").then_some(body)
    })?;
    let case_re = Regex::new(&format!(
        r"(?s)case\s+([0-9]+)\s*:.*?{}([0-9]+)(?:\.0+)?f?\s*;",
        assignment
    ))
    .expect("valid flag flutter case regex");
    let area_flutter_speeds = case_re
        .captures_iter(switch_body)
        .map(|capture| {
            Some(MapObjFlagAreaSpeed {
                area_index: capture[1].parse().ok()?,
                degrees_per_frame: capture[2].parse().ok()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if area_flutter_speeds.is_empty() {
        return None;
    }
    let default_re = Regex::new(&format!(
        r"(?s)default\s*:.*?{}([0-9]+)(?:\.0+)?f?\s*;",
        assignment
    ))
    .expect("valid default flag flutter speed regex");
    let default_speed = default_re.captures(switch_body)?[1].parse().ok()?;
    Some((default_speed, area_flutter_speeds))
}

fn extract_flag_phase_wrap(source: &str, class_name: &str) -> Option<u32> {
    let perform_re = Regex::new(r"void\s+[A-Za-z_][A-Za-z0-9_:]*::perform\s*\([^)]*\)\s*\{")
        .expect("valid flag manager perform regex");
    let speed = format!("{}::mFlutterSpeed", class_name);
    let body = perform_re.find_iter(source).find_map(|perform_match| {
        let body = braced_body(source, perform_match.end() - 1)?;
        (body.contains("mPhase") && body.contains(&speed)).then_some(body)
    })?;
    let wrap_re =
        Regex::new(r"mPhase\s*>\s*([0-9]+)(?:\.0+)?f?").expect("valid flag phase wrap regex");
    wrap_re.captures(body)?[1].parse().ok()
}

fn extract_stage_archive_table_path(application: &str) -> Option<String> {
    Regex::new(r#"bufStageArcBin\s*=\s*JKRDvdRipper::loadToMainRAM\s*\(\s*"([^"]+)""#)
        .expect("valid stage archive table path regex")
        .captures(application)
        .map(|capture| capture[1].to_string())
}

fn extract_flag_resource_stream_index(source: &str, class_name: &str) -> Option<u8> {
    let load_re = Regex::new(&format!(
        r"void\s+{}::load\s*\([^)]*\)\s*\{{",
        regex::escape(class_name)
    ))
    .expect("valid map-object flag load regex");
    let load = load_re.find(source)?;
    let body = braced_body(source, load.end() - 1)?;
    let resource_buffer = Regex::new(r"\binit\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)")
        .expect("valid map-object flag init regex")
        .captures(body)?[1]
        .to_string();
    let read_re = Regex::new(r"\bstream\.readString\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,")
        .expect("valid placement stream string read regex");
    let index = read_re
        .captures_iter(body)
        .position(|capture| capture[1] == resource_buffer)?;
    index.try_into().ok()
}

fn extract_u32_member_assignment(source: &str, member_name: &str) -> Option<u32> {
    let assignment_re = Regex::new(&format!(
        r"\b{}\s*=\s*([0-9]+)(?:\.0+)?f?\s*;",
        regex::escape(member_name)
    ))
    .expect("valid numeric member assignment regex");
    assignment_re.captures(source)?[1].parse().ok()
}

fn extract_string_factory_returns(
    text: &str,
    category: &str,
    source: SchemaSource,
    registry: &mut ObjectRegistry,
) {
    for candidate in extract_factory_candidates(text) {
        let class_name = match candidate.evidence {
            FactoryEvidence::ConstructedReturn => candidate
                .class_name
                .expect("constructed factory candidates always carry a class"),
            FactoryEvidence::NameComparison => "Unknown".to_string(),
        };
        registry.objects.push(ObjectDefinition {
            factory_name: candidate.factory_name,
            class_name,
            category: category.to_string(),
            source: source.clone(),
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        });
    }
}

fn dedup_registry(registry: &mut ObjectRegistry) -> Result<()> {
    let mut objects = BTreeMap::<String, ObjectDefinition>::new();
    for object in registry.objects.drain(..) {
        let key = object.factory_name.clone();
        match objects.get_mut(&key) {
            None => {
                objects.insert(key, object);
            }
            Some(existing)
                if existing.class_name == "Unknown" && object.class_name != "Unknown" =>
            {
                *existing = object;
            }
            Some(existing)
                if object.class_name != "Unknown"
                    && existing.class_name != "Unknown"
                    && existing.class_name != object.class_name =>
            {
                return Err(SchemaError::RegistryInvariant {
                    detail: format!(
                        "factory {} resolves to conflicting classes {} and {}",
                        object.factory_name, existing.class_name, object.class_name
                    ),
                });
            }
            Some(_) => {}
        }
    }
    registry.objects = objects.into_values().collect();
    registry.objects.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.factory_name.cmp(&b.factory_name))
    });

    registry.params.sort_by(|a, b| {
        a.source_file
            .cmp(&b.source_file)
            .then_with(|| a.member_name.cmp(&b.member_name))
    });
    registry.params.dedup();

    registry.asset_hints.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.asset_hints.dedup();

    registry.object_resources.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.model_index.cmp(&b.model_index))
            .then_with(|| a.model_name.cmp(&b.model_name))
            .then_with(|| a.load_flags.cmp(&b.load_flags))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.object_resources.dedup_by(|a, b| {
        a.factory_name == b.factory_name
            && a.model_index == b.model_index
            && a.model_name == b.model_name
            && a.resource_base == b.resource_base
            && a.load_flags == b.load_flags
    });

    // Keep the order of the retail lookup table while enforcing its exact key space.
    let mut map_obj_resource_names = BTreeSet::new();
    for resource in &registry.map_obj_resources {
        if !map_obj_resource_names.insert(resource.resource_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "duplicate map-object resource identity {}",
                    resource.resource_name
                ),
            });
        }
    }
    registry.map_obj_model_overrides.sort_by(|a, b| {
        a.resource_name
            .cmp(&b.resource_name)
            .then_with(|| a.class_name.cmp(&b.class_name))
    });
    for duplicate in registry.map_obj_model_overrides.windows(2) {
        if duplicate[0].resource_name == duplicate[1].resource_name
            && duplicate[0].class_name == duplicate[1].class_name
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "duplicate map-object model override {} for {}",
                    duplicate[0].resource_name, duplicate[0].class_name
                ),
            });
        }
    }
    registry.map_obj_string_tev_programs.sort_by(|a, b| {
        a.resource_name
            .cmp(&b.resource_name)
            .then_with(|| a.class_name.cmp(&b.class_name))
    });
    for duplicate in registry.map_obj_string_tev_programs.windows(2) {
        if duplicate[0].resource_name == duplicate[1].resource_name
            && duplicate[0].class_name == duplicate[1].class_name
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "duplicate map-object string TEV program {} for {}",
                    duplicate[0].resource_name, duplicate[0].class_name
                ),
            });
        }
    }
    registry
        .map_obj_stream_tev_colors
        .sort_by(|a, b| a.class_name.cmp(&b.class_name));
    for duplicate in registry.map_obj_stream_tev_colors.windows(2) {
        if duplicate[0].class_name == duplicate[1].class_name {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "duplicate map-object stream TEV color for {}",
                    duplicate[0].class_name
                ),
            });
        }
    }
    registry
        .map_obj_ball_transforms
        .sort_by_key(|definition| definition.actor_type);
    for duplicate in registry.map_obj_ball_transforms.windows(2) {
        if duplicate[0].actor_type == duplicate[1].actor_type {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "duplicate map-object ball transform actor type {:#010x}",
                    duplicate[0].actor_type
                ),
            });
        }
    }
    registry.map_obj_factories.sort();
    registry.map_obj_factories.dedup();

    registry.map_static_models.sort_by(|a, b| {
        a.model_path
            .cmp(&b.model_path)
            .then_with(|| a.actor_name.cmp(&b.actor_name))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.map_static_models.dedup();

    registry.map_obj_flags.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry
        .map_obj_flags
        .dedup_by(|a, b| a.factory_name == b.factory_name);

    registry.collision_surfaces.sort_by(|a, b| {
        a.value
            .cmp(&b.value)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry
        .collision_surfaces
        .dedup_by(|a, b| a.name == b.name && a.value == b.value);

    registry.particle_resources.sort_by(|a, b| {
        a.effect_id
            .cmp(&b.effect_id)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.particle_resources.dedup();

    registry.actor_particle_bindings.sort_by(|a, b| {
        a.class_name
            .cmp(&b.class_name)
            .then_with(|| a.effect_id.cmp(&b.effect_id))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.actor_particle_bindings.dedup();

    registry
        .npc_actors
        .sort_by(|a, b| a.actor_key.cmp(&b.actor_key));
    registry
        .npc_actors
        .dedup_by(|a, b| a.actor_key == b.actor_key);
    registry.npc_material_colors.sort_by(|a, b| {
        a.actor_key
            .cmp(&b.actor_key)
            .then_with(|| a.color_index_channel.cmp(&b.color_index_channel))
            .then_with(|| a.model_index.cmp(&b.model_index))
            .then_with(|| a.change.material_name.cmp(&b.change.material_name))
    });
    registry.npc_material_colors.dedup();

    registry.runtime_map_obj_dependencies.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.resource_name.cmp(&b.resource_name))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry
        .runtime_map_obj_dependencies
        .dedup_by(|a, b| a.factory_name == b.factory_name && a.resource_name == b.resource_name);
    registry.runtime_name_references.sort();
    registry.runtime_name_references.dedup();

    registry.enemy_manager_animation_folders.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.folder.cmp(&b.folder))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry
        .enemy_manager_animation_folders
        .dedup_by(|a, b| a.factory_name == b.factory_name && a.folder == b.folder);

    registry.enemy_managers.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.class_name.cmp(&b.class_name))
    });
    registry
        .enemy_managers
        .dedup_by(|a, b| a.factory_name == b.factory_name);
    registry.enemy_actors.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.class_name.cmp(&b.class_name))
    });
    registry
        .enemy_actors
        .dedup_by(|a, b| a.factory_name == b.factory_name);
    registry.enemy_material_colors.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.material_name.cmp(&b.material_name))
            .then_with(|| a.tev_register.cmp(&b.tev_register))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.enemy_material_colors.dedup_by(|a, b| {
        a.factory_name == b.factory_name
            && a.material_name == b.material_name
            && a.tev_register == b.tev_register
    });
    Ok(())
}

fn ensure_extracted(
    extractor: SchemaExtractor,
    source_path: PathBuf,
    count: usize,
    expected: &'static str,
) -> Result<()> {
    if count == 0 {
        return Err(SchemaError::ExtractionDrift {
            extractor,
            source_path,
            expected,
        });
    }
    Ok(())
}

fn validate_registry(registry: &ObjectRegistry) -> Result<()> {
    if registry.objects.is_empty() {
        return Err(SchemaError::RegistryInvariant {
            detail: "object registry is empty".to_string(),
        });
    }

    let mut collision_surface_values = BTreeMap::new();
    for surface in &registry.collision_surfaces {
        if surface.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!("collision surface {} has no provenance", surface.name),
            });
        }
        if let Some(previous) = collision_surface_values.insert(&surface.name, surface.value) {
            if previous != surface.value {
                return Err(SchemaError::RegistryInvariant {
                    detail: format!(
                        "collision surface {} resolves to conflicting values {previous:#06x} and {:#06x}",
                        surface.name, surface.value
                    ),
                });
            }
        }
    }

    let objects = registry
        .objects
        .iter()
        .map(|object| (object.factory_name.as_str(), object))
        .collect::<BTreeMap<_, _>>();
    let map_obj_resource_names = registry
        .map_obj_resources
        .iter()
        .map(|resource| resource.resource_name.as_str())
        .collect::<BTreeSet<_>>();
    for dependency in &registry.runtime_map_obj_dependencies {
        if dependency.resource_name.is_empty() || dependency.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "factory {} has a runtime map-object dependency without a name or provenance",
                    dependency.factory_name
                ),
            });
        }
        if !objects.contains_key(dependency.factory_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "runtime map-object dependency {} has no registered factory {}",
                    dependency.resource_name, dependency.factory_name
                ),
            });
        }
        if !map_obj_resource_names.contains(dependency.resource_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "factory {} instantiates unknown map-object resource {}",
                    dependency.factory_name, dependency.resource_name
                ),
            });
        }
    }

    for reference in &registry.runtime_name_references {
        if reference.record_name.is_empty() || reference.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "factory {} has a runtime name reference without a name or provenance",
                    reference.factory_name
                ),
            });
        }
        if !objects.contains_key(reference.factory_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "runtime name reference {:?} has no registered owner factory {}",
                    reference.record_name, reference.factory_name
                ),
            });
        }
        match &reference.target {
            RuntimeNameReferenceTarget::Actor { factory_name }
                if !objects.contains_key(factory_name.as_str()) =>
            {
                return Err(SchemaError::RegistryInvariant {
                    detail: format!(
                        "runtime name reference {:?} targets unknown actor factory {}",
                        reference.record_name, factory_name
                    ),
                });
            }
            RuntimeNameReferenceTarget::PlacementRecord { type_name } if type_name.is_empty() => {
                return Err(SchemaError::RegistryInvariant {
                    detail: format!(
                        "runtime name reference {:?} has an empty placement-record type",
                        reference.record_name
                    ),
                });
            }
            _ => {}
        }
    }

    let enemy_manager_factories = registry
        .enemy_managers
        .iter()
        .map(|manager| manager.factory_name.as_str())
        .collect::<BTreeSet<_>>();
    for animation in &registry.enemy_manager_animation_folders {
        if animation.folder.is_empty() || animation.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "enemy manager {} has an animation folder without a path or provenance",
                    animation.factory_name
                ),
            });
        }
        if !enemy_manager_factories.contains(animation.factory_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "animation folder {} has no enemy manager factory {}",
                    animation.folder, animation.factory_name
                ),
            });
        }
    }

    for resource in &registry.object_resources {
        if !objects.contains_key(resource.factory_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "model resource {} has no registered factory {}",
                    resource.model_name, resource.factory_name
                ),
            });
        }
        if resource.model_name.is_empty() || resource.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "factory {} has a model resource without a name or provenance",
                    resource.factory_name
                ),
            });
        }
    }
    for factory_name in registry
        .object_resources
        .iter()
        .map(|resource| resource.factory_name.as_str())
        .collect::<BTreeSet<_>>()
    {
        let primary_count = registry
            .object_resources
            .iter()
            .filter(|resource| {
                resource.factory_name == factory_name
                    && resource.role == ObjectResourceRole::Primary
            })
            .count();
        if primary_count != 1 {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "factory {factory_name} has {primary_count} primary model resources; expected exactly one"
                ),
            });
        }
    }

    let mut map_obj_resource_names = BTreeSet::new();
    for resource in &registry.map_obj_resources {
        if resource.resource_name.is_empty() || resource.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: "map-object resource has an empty identity or provenance".to_string(),
            });
        }
        if resource.load_flags == 0 {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object resource {} has zero loader flags",
                    resource.resource_name
                ),
            });
        }
        if !map_obj_resource_names.insert(resource.resource_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "duplicate map-object resource identity {}",
                    resource.resource_name
                ),
            });
        }
        if resource
            .primary_model
            .as_ref()
            .is_some_and(|model| model.is_empty() || !model.to_ascii_lowercase().ends_with(".bmd"))
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object resource {} has invalid primary BMD {:?}",
                    resource.resource_name, resource.primary_model
                ),
            });
        }
    }
    for definition in &registry.map_obj_model_overrides {
        let resource = registry
            .find_map_obj_resource(&definition.resource_name)
            .ok_or_else(|| SchemaError::RegistryInvariant {
                detail: format!(
                    "model override references unknown map-object resource {}",
                    definition.resource_name
                ),
            })?;
        if resource.primary_model.is_some() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "model override {} is not attached to a zero-primary resource",
                    definition.resource_name
                ),
            });
        }
        let matching_factory = registry.objects.iter().find(|object| {
            object.class_name == definition.class_name
                && object
                    .factory_name
                    .eq_ignore_ascii_case(&definition.resource_name)
        });
        if matching_factory.is_none_or(|object| !registry.is_map_obj_factory(&object.factory_name))
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "model override {} / {} has no TMapObjBase-derived factory",
                    definition.resource_name, definition.class_name
                ),
            });
        }
        if definition.model_path.is_empty()
            || !definition.model_path.to_ascii_lowercase().ends_with(".bmd")
            || definition.load_flags == 0
            || definition.binding_source_file.is_empty()
            || definition.model_source_file.is_empty()
            || definition
                .tev_color
                .is_some_and(|color| color.register >= 4)
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object model override {} has invalid model, color, flags, or provenance",
                    definition.resource_name
                ),
            });
        }
    }
    for definition in &registry.map_obj_string_tev_programs {
        if registry
            .find_map_obj_resource(&definition.resource_name)
            .is_none()
            || !registry.objects.iter().any(|object| {
                object.factory_name == definition.resource_name
                    && object.class_name == definition.class_name
            })
            || definition.tev_register >= 4
            || definition.source_file.is_empty()
            || definition.variants.is_empty()
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object string TEV program {} / {} has invalid binding, register, variants, or provenance",
                    definition.resource_name, definition.class_name
                ),
            });
        }
        let mut selectors = BTreeSet::new();
        for variant in &definition.variants {
            if variant.selector_value.is_empty() || !selectors.insert(&variant.selector_value) {
                return Err(SchemaError::RegistryInvariant {
                    detail: format!(
                        "map-object string TEV program {} has an empty or duplicate selector",
                        definition.resource_name
                    ),
                });
            }
        }
    }
    for definition in &registry.map_obj_stream_tev_colors {
        let matching_factory = registry
            .objects
            .iter()
            .find(|object| object.class_name == definition.class_name);
        if matching_factory.is_none_or(|object| !registry.is_map_obj_factory(&object.factory_name))
            || definition.tev_register >= 4
            || definition.trailing_rgb_u32_count != 3
            || !(0..=255).contains(&definition.alpha)
            || definition.source_file.is_empty()
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object stream TEV color for {} has an invalid binding, register, layout, alpha, or provenance",
                    definition.class_name
                ),
            });
        }
    }
    for definition in &registry.map_obj_ball_transforms {
        if definition.actor_type == 0
            || definition.body_radius == 0
            || definition.source_file.is_empty()
            || (definition.positive_y_axis_subtract.is_some()
                && definition.one_minus_y_axis_subtract.is_some())
        {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object ball transform {:#010x} has invalid radius, correction, or provenance",
                    definition.actor_type
                ),
            });
        }
    }
    for factory_name in &registry.map_obj_factories {
        if !objects.contains_key(factory_name.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object factory type references unknown factory {factory_name}"
                ),
            });
        }
    }

    let npc_keys = registry
        .npc_actors
        .iter()
        .map(|actor| actor.actor_key.as_str())
        .collect::<BTreeSet<_>>();
    for color in &registry.npc_material_colors {
        if !npc_keys.contains(color.actor_key.as_str()) {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "NPC root color references unknown actor key {}",
                    color.actor_key
                ),
            });
        }
        if color.change.material_name.is_empty() || color.source_file.is_empty() {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "NPC {} has root color metadata without material/provenance",
                    color.actor_key
                ),
            });
        }
    }

    for flag in &registry.map_obj_flags {
        let Some(object) = objects.get(flag.factory_name.as_str()) else {
            return Err(SchemaError::RegistryInvariant {
                detail: format!("map-object flag {} is not registered", flag.factory_name),
            });
        };
        if object.class_name != flag.class_name {
            return Err(SchemaError::RegistryInvariant {
                detail: format!(
                    "map-object flag {} has class {}, registry has {}",
                    flag.factory_name, flag.class_name, object.class_name
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    struct SchemaFixture {
        root: PathBuf,
    }

    impl SchemaFixture {
        fn new(label: &str) -> Self {
            Self {
                root: std::env::temp_dir().join(format!(
                    "sms-schema-generator-{label}-{}-{}",
                    std::process::id(),
                    NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
                )),
            }
        }

        fn write(&self, relative_path: &str, text: &str) {
            let path = self.root.join(relative_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for SchemaFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn extracts_procedural_flag_resources_and_geometry_from_decomp() {
        let source = r#"
            f32 TMapObjFlag::mFlutterSpeed = 4.0f;
            #define REGISTER_FLAG(N, NAME) \
                snprintf(buf, 64, "/scene/mapObj/%s.bti", name);
            void TMapObjFlagManager::registerObj(TMapObjFlag*, const char*) {
                REGISTER_FLAG(0, "flagSun")
                REGISTER_FLAG(1, "flagWhite")
            }
            void TMapObjFlag::load(JSUMemoryInputStream& stream) {
                JDrama::TActor::load(stream);
                char buf[64];
                stream.readString(buf, 64);
                init(buf);
            }
            TMapObjFlag::TMapObjFlag(const char*) {
                mFlagHeight = 125.0f;
                mFlagWidth = 130.0f;
                mSegmentSize = 20.0f;
            }
            void TMapObjFlagManager::load(JSUMemoryInputStream&) {
                switch (gpMarDirector->mMap) {
                case 2:
                    TMapObjFlag::mFlutterSpeed = 16.0f;
                    break;
                case 4:
                    TMapObjFlag::mFlutterSpeed = 12.0f;
                    break;
                default:
                    TMapObjFlag::mFlutterSpeed = 8.0f;
                    break;
                }
            }
            void TMapObjFlagManager::perform(u32, JDrama::TGraphics*) {
                flag->mPhase += TMapObjFlag::mFlutterSpeed;
                if (flag->mPhase > 360.0f)
                    flag->mPhase -= 360.0f;
            }
        "#;
        let factories = r#"
            if (strcmp(name, "MapObjFlag") == 0)
                return new TMapObjFlag("flag");
        "#;
        let application = r#"
            bufStageArcBin = JKRDvdRipper::loadToMainRAM(
                "/data/stageArc.bin", nullptr, EXPAND_SWITCH_DEFAULT, 0, mHeap);
        "#;

        let definition = extract_map_obj_flag_definition(
            source,
            factories,
            application,
            "src/MoveBG/MapObjFlag.cpp",
        )
        .expect("extract flag definition");

        assert_eq!(definition.factory_name, "MapObjFlag");
        assert_eq!(definition.class_name, "TMapObjFlag");
        assert_eq!(definition.texture_path_pattern, "/scene/mapObj/%s.bti");
        assert_eq!(
            definition.registered_texture_names,
            ["flagSun".to_string(), "flagWhite".to_string()]
        );
        assert_eq!(definition.resource_name_stream_index, 0);
        assert_eq!(definition.default_height, 125);
        assert_eq!(definition.default_width, 130);
        assert_eq!(definition.default_segment_size, 20);
        assert_eq!(definition.default_flutter_speed_degrees_per_frame, 8);
        assert_eq!(
            definition.area_flutter_speeds,
            [
                MapObjFlagAreaSpeed {
                    area_index: 2,
                    degrees_per_frame: 16,
                },
                MapObjFlagAreaSpeed {
                    area_index: 4,
                    degrees_per_frame: 12,
                },
            ]
        );
        assert_eq!(definition.phase_wrap_degrees, 360);
        assert_eq!(definition.stage_archive_table_path, "/data/stageArc.bin");
    }

    #[test]
    fn extracts_simple_factory_return() {
        let text = r#"
            if (strcmp(name, "Mario") == 0)
                return new TMario;
            if (strcmp(name, "MarScene") == 0)
                return new JDrama::TSmJ3DScn;
        "#;
        let mut registry = ObjectRegistry::default();
        extract_string_factory_returns(text, "System", SchemaSource::MarNameRefGen, &mut registry);
        assert_eq!(registry.objects.len(), 2);
        assert_eq!(registry.objects[0].factory_name, "Mario");
        assert_eq!(registry.objects[1].class_name, "JDrama::TSmJ3DScn");
    }

    #[test]
    fn extracts_enemy_actor_and_manager_variant_indices() {
        let text = r#"
            if (strcmp(name, "FishoidC") == 0)
                return new TFishoid(2, "fish");
            if (strcmp(name, "FruitsBoatManagerD") == 0)
                return new TFruitsBoatManager(3, "boat manager");
        "#;
        let mut registry = ObjectRegistry::default();
        extract_enemy_factory_variants(text, &mut registry);

        assert_eq!(registry.enemy_actors[0].factory_name, "FishoidC");
        assert_eq!(registry.enemy_actors[0].model_index, Some(2));
        assert_eq!(
            registry.enemy_managers[0].factory_name,
            "FruitsBoatManagerD"
        );
        assert_eq!(registry.enemy_managers[0].model_index, Some(3));
    }

    #[test]
    fn derives_inherited_enemy_material_tev_colors() {
        let text = r#"
            void TPoiHanaManager::initSetEnemies() {
                int bodyIdx = getObj(0)->getModel()->getModelData()
                    ->getMaterialName()->getIndex("_body");
                for (int i = 0; i < mObjNum; ++i) {
                    TPoiHana* poiHana = (TPoiHana*)unk18[i];
                    SMS_InitPacket_OneTevColor(
                        poiHana->getMActor()->getModel(), bodyIdx,
                        GX_TEVREG0, &poiHana->unk1C0);
                }
            }

            void TPoiHana::init(TLiveManager* manager) {
                unk1C0.r = 0xff41;
                unk1C0.g = 0x8;
                unk1C0.b = 0x12F;
            }

            void TPoiHanaRed::init(TLiveManager* manager) {
                TPoiHana::init(manager);
                unk1C0.r = 0x11B;
                unk1C0.g = 0xFFCB;
                unk1C0.b = 0xFF86;
            }
        "#;
        let bindings = extract_enemy_tev_color_bindings(text, "src/Enemy/poihana.cpp");
        let mut init_colors = BTreeMap::new();
        extract_enemy_init_tev_colors(text, &mut init_colors);
        let inheritance = BTreeMap::from([
            ("TPoiHanaRed".to_string(), "TPoiHana".to_string()),
            ("TSleepPoiHana".to_string(), "TPoiHana".to_string()),
        ]);
        let actors = [
            ("PoiHana", "TPoiHana"),
            ("PoiHanaRed", "TPoiHanaRed"),
            ("SleepPoiHana", "TSleepPoiHana"),
        ]
        .into_iter()
        .map(|(factory_name, class_name)| EnemyActorDefinition {
            factory_name: factory_name.to_string(),
            class_name: class_name.to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        })
        .collect::<Vec<_>>();

        let definitions =
            derive_enemy_material_tev_colors(&actors, &bindings, &init_colors, &inheritance);
        let by_factory = definitions
            .into_iter()
            .map(|definition| (definition.factory_name.clone(), definition))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(bindings[0].material_name, "_body");
        assert_eq!(bindings[0].tev_register, 0);
        assert_eq!(
            by_factory["PoiHana"].color,
            [Some(-191), Some(8), Some(303), None]
        );
        assert_eq!(
            by_factory["PoiHanaRed"].color,
            [Some(283), Some(-53), Some(-122), None]
        );
        assert_eq!(
            by_factory["SleepPoiHana"].color,
            [Some(-191), Some(8), Some(303), None]
        );
    }

    #[test]
    fn associates_enemy_actors_with_the_closest_spawn_manager() {
        let text = r#"
            THaneHamuKuri* THaneHamuKuriManager::createEnemyInstance() {
                return new THaneHamuKuri("winged goop enemy");
            }
        "#;
        let mut spawned_classes = BTreeMap::new();
        extract_enemy_manager_actor_classes(text, &mut spawned_classes);
        assert_eq!(
            spawned_classes
                .get("THaneHamuKuriManager")
                .map(String::as_str),
            Some("THaneHamuKuri")
        );

        let inheritance = BTreeMap::from([
            ("THaneHamuKuri2".to_string(), "THaneHamuKuri".to_string()),
            ("THaneHamuKuri".to_string(), "THamuKuri".to_string()),
        ]);
        let actor = EnemyActorDefinition {
            factory_name: "HaneHamuKuri2".to_string(),
            class_name: "THaneHamuKuri2".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        };
        let managers = vec![
            EnemyManagerDefinition {
                factory_name: "HamuKuriManager".to_string(),
                class_name: "THamuKuriManager".to_string(),
                model_index: None,
                spawned_actor_class: Some("THamuKuri".to_string()),
                parameter_path: None,
                models: Vec::new(),
            },
            EnemyManagerDefinition {
                factory_name: "HaneHamuKuriManager".to_string(),
                class_name: "THaneHamuKuriManager".to_string(),
                model_index: None,
                spawned_actor_class: Some("THaneHamuKuri".to_string()),
                parameter_path: None,
                models: Vec::new(),
            },
        ];

        assert_eq!(
            compatible_enemy_managers(&actor, &managers, &inheritance),
            ["HaneHamuKuriManager", "HamuKuriManager"]
        );
    }

    #[test]
    fn extracts_inherited_runtime_scale_parameters_and_manager_path() {
        let text = r#"
            TSmallEnemyParams::TSmallEnemyParams(const char* path)
                : PARAM_INIT(mSLBodyScaleLow, 0.5f)
                , PARAM_INIT(mSLBodyScaleHigh, 1.5f)
            {
                TParams::load(path);
                unk2CC = mSLBodyScaleLow.get();
                unk2D0 = mSLBodyScaleHigh.get();
            }

            void TSmallEnemy::reset() {
                TSmallEnemyParams* params = getSaveParam2();
                mBodyScale = chooseScale(params->unk2CC, params->unk2D0);
                mScaling.set(mBodyScale, mBodyScale, mBodyScale);
            }

            void TFixtureManager::load(JSUMemoryInputStream& stream) {
                unk38 = new TSmallEnemyParams("/enemy/fixture.prm");
            }
        "#;
        let scales = extract_enemy_runtime_uniform_scales(text, "src/Enemy/fixture.cpp");
        assert_eq!(scales.len(), 1);
        assert_eq!(scales[0].0, "TSmallEnemy");
        assert_eq!(scales[0].1.low_parameter, "mSLBodyScaleLow");
        assert_eq!(scales[0].1.high_parameter, "mSLBodyScaleHigh");
        assert_eq!(
            extract_enemy_manager_parameter_paths(text),
            [(
                "TFixtureManager".to_string(),
                "/enemy/fixture.prm".to_string()
            )]
        );

        let inherited = inherited_actor_value(
            "TFixtureEnemy",
            &scales.into_iter().collect(),
            &BTreeMap::from([("TFixtureEnemy".to_string(), "TSmallEnemy".to_string())]),
        )
        .expect("inherit runtime scale metadata");
        assert_eq!(inherited.source_file, "src/Enemy/fixture.cpp");
    }

    #[test]
    fn extracts_runtime_map_object_dependencies_across_class_inheritance() {
        let text = r#"
            void TFixtureManager::loadAfter() {
                TSmallEnemyManager::loadAfter();
                TMapObjBaseManager::newAndRegisterObj("mushroom1up");
            }

            void TFixtureVariantManager::loadAfter() {
                TFixtureManager::loadAfter();
                gpMapObjManager->newAndRegisterObj("mario_cap");
            }
        "#;
        let extracted = extract_runtime_map_obj_dependencies(text, "src/Enemy/fixture.cpp")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let inheritance = BTreeMap::from([(
            "TFixtureVariantManager".to_string(),
            "TFixtureManager".to_string(),
        )]);
        assert_eq!(
            inherited_actor_models_union("TFixtureVariantManager", &extracted, &inheritance,),
            [
                ("mario_cap".to_string(), "src/Enemy/fixture.cpp".to_string()),
                (
                    "mushroom1up".to_string(),
                    "src/Enemy/fixture.cpp".to_string()
                ),
            ]
        );
    }
    #[test]
    fn extracts_runtime_shine_and_demo_camera_name_references() {
        let text = r#"
            DEFINE_NERVE(TNerveFixtureBossDie, TLiveActor) {
                TFixtureBoss* boss = (TFixtureBoss*)spine->getBody();
                gpItemManager->makeShineAppearWithDemo(
                    "reward shine", "reward camera", 1.0f, 2.0f, 3.0f);
            }

            void TTimedActor::finish() {
                gpItemManager->makeShineAppearWithTime(
                    "timed reward", 60, 1.0f, 2.0f, 3.0f, 0, 0, 0);
            }
        "#;
        let extracted = extract_runtime_name_references(text, "src/Enemy/fixture.cpp")
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        assert!(extracted["TFixtureBoss"].iter().any(|reference| {
            reference.required
                && reference.record_name == "reward shine"
                && matches!(
                    &reference.target,
                    RuntimeNameReferenceTarget::Actor { factory_name }
                        if factory_name == "Shine"
                )
        }));
        assert!(extracted["TFixtureBoss"].iter().any(|reference| {
            reference.record_name == "reward camera"
                && matches!(
                    &reference.target,
                    RuntimeNameReferenceTarget::PlacementRecord { type_name }
                        if type_name == "CameraMapInfo"
                )
        }));
        assert_eq!(extracted["TTimedActor"][0].record_name, "timed reward");
        assert!(!extracted["TTimedActor"][0].required);
    }

    #[test]
    fn extracts_direct_manager_animation_folders_and_preserves_empty_overrides() {
        let text = r#"
            void TFixtureManager::createAnmData() {
                MActorAnmData* data = new MActorAnmData;
                data->init("/scene/fixtureanm", nullptr);
                unk20 = data;
            }

            void TFixtureVariantManager::createAnmData() {
                TObjManager::createAnmData();
            }
        "#;
        let extracted = extract_enemy_manager_animation_folders(text, "src/Enemy/fixture.cpp")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            extracted["TFixtureManager"],
            [(
                "/scene/fixtureanm".to_string(),
                "src/Enemy/fixture.cpp".to_string()
            )]
        );
        assert!(extracted["TFixtureVariantManager"].is_empty());

        let inheritance = BTreeMap::from([(
            "TFixtureVariantManager".to_string(),
            "TFixtureManager".to_string(),
        )]);
        assert!(
            inherited_actor_models("TFixtureVariantManager", &extracted, &inheritance,)
                .expect("explicit override")
                .is_empty()
        );
    }

    #[test]
    fn associates_indexed_variants_with_the_manager_class_stem() {
        let manager = EnemyManagerDefinition {
            factory_name: "ButterflyManager".to_string(),
            class_name: "TButterfloidManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: Vec::new(),
        };
        let actor = EnemyActorDefinition {
            factory_name: "ButterflyC".to_string(),
            class_name: "TButterfloid".to_string(),
            model_index: Some(2),
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        };

        assert_eq!(
            compatible_enemy_managers(&actor, &[manager], &BTreeMap::new()),
            ["ButterflyManager"]
        );

        let manager = EnemyManagerDefinition {
            factory_name: "FishoidManager".to_string(),
            class_name: "TFishoidManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: Vec::new(),
        };
        let actor = EnemyActorDefinition {
            factory_name: "FishoidD".to_string(),
            class_name: "TFishoid".to_string(),
            model_index: Some(3),
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        };
        assert_eq!(
            compatible_enemy_managers(&actor, &[manager], &BTreeMap::new()),
            ["FishoidManager"]
        );

        let manager = EnemyManagerDefinition {
            factory_name: "EggGenManager".to_string(),
            class_name: "TEggGenManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: vec![EnemyModelDefinition {
                model_name: "gene_egg_model1.bmd".to_string(),
                load_flags: 0,
                source_file: "src/Enemy/egggen.cpp".to_string(),
            }],
        };
        let actor = EnemyActorDefinition {
            factory_name: "WickedEggGenerator".to_string(),
            class_name: "TEggGenerator".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: Some("gene_egg_model1.bmd".to_string()),
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        };
        assert_eq!(
            compatible_enemy_managers(&actor, &[manager], &BTreeMap::new()),
            ["EggGenManager"]
        );
    }

    #[test]
    fn manager_factory_stem_matching_and_ranking_are_case_sensitive() {
        let actor = EnemyActorDefinition {
            factory_name: "Fishoid".to_string(),
            class_name: "TFishoid".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: None,
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        };
        let exact = EnemyManagerDefinition {
            factory_name: "FishoidManager".to_string(),
            class_name: "TUnrelatedManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: Vec::new(),
        };
        let wrong_case = EnemyManagerDefinition {
            factory_name: "fishoidManager".to_string(),
            class_name: "TWrongManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: Vec::new(),
        };

        assert_eq!(
            compatible_enemy_managers(&actor, std::slice::from_ref(&exact), &BTreeMap::new()),
            ["FishoidManager"]
        );
        assert!(compatible_enemy_managers(
            &actor,
            std::slice::from_ref(&wrong_case),
            &BTreeMap::new()
        )
        .is_empty());

        let mut class_confirmed_wrong_case = wrong_case;
        class_confirmed_wrong_case.spawned_actor_class = Some("TFishoid".to_string());
        assert_eq!(
            compatible_enemy_managers(
                &actor,
                &[class_confirmed_wrong_case, exact],
                &BTreeMap::new()
            ),
            ["FishoidManager", "fishoidManager"]
        );
        assert_eq!(
            factory_without_manager("ManagerFactoryManager"),
            "ManagerFactory"
        );
        assert_eq!(factory_without_manager("Fishoidmanager"), "Fishoidmanager");
    }

    #[test]
    fn model_name_matching_remains_case_insensitive() {
        let actor = EnemyActorDefinition {
            factory_name: "UnrelatedActor".to_string(),
            class_name: "TUnrelatedActor".to_string(),
            model_index: None,
            fallback_models: Vec::new(),
            primary_model: Some("fishoid.bmd".to_string()),
            named_models: Vec::new(),
            indexed_models: Vec::new(),
            manager_factories: Vec::new(),
            runtime_uniform_scale: None,
        };
        let manager = EnemyManagerDefinition {
            factory_name: "UnrelatedManager".to_string(),
            class_name: "TNoClassMatchManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
            parameter_path: None,
            models: vec![EnemyModelDefinition {
                model_name: "FISHOID.BMD".to_string(),
                load_flags: 0,
                source_file: "src/Enemy/Test.cpp".to_string(),
            }],
        };

        assert_eq!(
            compatible_enemy_managers(&actor, &[manager], &BTreeMap::new()),
            ["UnrelatedManager"]
        );
    }

    #[test]
    fn extracts_local_global_and_symbolic_enemy_model_tables() {
        let text = r#"
            static const char cHeadModel[] = "head.bmd";
            static const TModelDataLoadEntry sGlobalModels[] = {
                { "body.bmd", 0x10300000, 0 },
                { cHeadModel, 0x10100000, 0 },
                { nullptr, 0, 0 },
            };
            void TBossManager::createModelData() {
                createModelDataArray(sGlobalModels);
            }
            void TDefaultManager::createModelData() {
                static TModelDataLoadEntry entry = {
                    "default.bmd",
                    J3DMLF_MaterialPEFull | J3DMLF_UseUniqueMaterials
                        | (1 << J3DMLF_TevStageNumShift),
                    0
                };
                createModelDataArray(&entry);
            }
        "#;
        let symbols = BTreeMap::from([
            ("J3DMLF_MaterialPEFull".to_string(), 0x1000_0000),
            ("J3DMLF_UseUniqueMaterials".to_string(), 0x0020_0000),
            ("J3DMLF_TevStageNumShift".to_string(), 16),
        ]);
        let managers = extract_enemy_manager_models(text, "src/Enemy/Test.cpp", &symbols)
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        assert_eq!(managers["TBossManager"][0].model_name, "body.bmd");
        assert_eq!(managers["TBossManager"][1].model_name, "head.bmd");
        assert_eq!(managers["TDefaultManager"][0].load_flags, 0x1021_0000);
    }

    #[test]
    fn resolves_inherited_enemy_manager_models() {
        let models = BTreeMap::from([(
            "TObjManager".to_string(),
            vec![EnemyModelDefinition {
                model_name: "default.bmd".to_string(),
                load_flags: 0x1021_0000,
                source_file: "src/Strategic/objmanager.cpp".to_string(),
            }],
        )]);
        let inheritance = BTreeMap::from([
            (
                "THamuKuriLauncherManager".to_string(),
                "TLauncherManager".to_string(),
            ),
            ("TLauncherManager".to_string(), "TEnemyManager".to_string()),
            ("TEnemyManager".to_string(), "TLiveManager".to_string()),
            ("TLiveManager".to_string(), "TObjManager".to_string()),
        ]);

        assert_eq!(
            inherited_enemy_models("THamuKuriLauncherManager", &models, &inheritance).unwrap()[0]
                .load_flags,
            0x1021_0000
        );
    }

    #[test]
    fn extracts_managerless_actor_default_model_override() {
        let text = r#"
            void TEMario::init(TLiveManager* manager) {
                mMActorKeeper->mModelLoaderFlags = 0x11300000;
                mMActor = mMActorKeeper->createMActorFromDefaultBmd(chara->mFolder, 0);
            }
        "#;
        let actors = extract_enemy_actor_fallback_models(text, "src/Enemy/emario.cpp");
        assert_eq!(actors[0].0, "TEMario");
        assert_eq!(actors[0].1[0].model_name, "default.bmd");
        assert_eq!(actors[0].1[0].load_flags, 0x1130_0000);
    }

    #[test]
    fn extracts_owner_named_enemy_model_override() {
        let text = r#"
            static const char* sNames[] = { "shadow", "Monteman" };
            static const char* sModels[] = {
                "/kagemario/kagemario_model.bmd",
                "/scene/map/map/pad/monteman_model.bmd",
            };
            void TEnemyMario::initEnemyValues() {
                for (int i = 0; i < 2; ++i) {
                    if (strcmp(sNames[i], emOwner(this)->getName()) == 0)
                        modelType = i;
                }
                if (modelType == 1) {
                    void* resource = JKRFileLoader::getGlbResource(sModels[modelType]);
                    J3DModelLoaderDataBase::load(resource, 0x10040000);
                }
            }
            void TEMario::load(JSUMemoryInputStream& stream) {
                mEnemyMario = new TEnemyMario;
            }
        "#;
        let named = extract_enemy_named_models(text, "src/Enemy/EnemyMario.cpp");
        assert_eq!(named[0].0, "TEnemyMario");
        assert_eq!(named[0].1[0].actor_name, "Monteman");
        assert_eq!(named[0].1[0].load_flags, 0x1004_0000);

        let mut owned = BTreeMap::new();
        extract_owned_actor_classes(text, &mut owned);
        assert_eq!(owned["TEMario"], ["TEnemyMario"]);
    }

    #[test]
    fn extracts_default_and_case_selected_enemy_models() {
        let text = r#"
            void TTelesaManager::load(JSUMemoryInputStream& stream) {
                void* data = JKRGetResource("/scene/telesa/modoki.bmd");
                mModoki = new SDLModelData(
                    J3DModelLoaderDataBase::load(data, 0x11020000));
            }
            void TMarioModokiTelesa::load(JSUMemoryInputStream& stream) {
                SDLModelData* model = ((TTelesaManager*)mManager)->mModoki;
                switch (mImitationIndex) {
                case 2:
                    if (void* data = JKRGetResource("/scene/mapObj/coin_red.bmd"))
                        model = new SDLModelData(
                            J3DModelLoaderDataBase::load(data, 0x10020000));
                    break;
                case 12:
                    if (void* data = JKRGetResource("/scene/monteM/mom_model.bmd"))
                        model = new SDLModelData(
                            J3DModelLoaderDataBase::load(data, 0x10020000));
                    break;
                }
            }
        "#;
        let models = extract_enemy_indexed_models(text, "src/Enemy/telesa.cpp");
        assert_eq!(models[0].0, "TMarioModokiTelesa");
        assert_eq!(models[0].1.len(), 3);
        assert_eq!(models[0].1[0].model_path, "/scene/telesa/modoki.bmd");
        assert_eq!(models[0].1[1].index, 2);
        assert_eq!(models[0].1[2].index, 12);
    }

    #[test]
    fn derives_root_part_model_index_from_actor_and_part_constructors() {
        let actor_text = r#"
            void TBoss::init(TLiveManager* manager) {
                mHead = new TBossHead(this, "head");
                mMActor = mHead->mMActor;
            }
        "#;
        let part_text = r#"
            TBossHead::TBossHead(TBoss* owner, const char* name)
                : TBossPart(owner, 0x08000014, 1, name) {}
        "#;
        let mut roots = BTreeMap::new();
        let mut indices = BTreeMap::new();
        extract_actor_root_parts(actor_text, &mut roots);
        extract_part_model_indices(part_text, &mut indices);

        assert_eq!(roots["TBoss"], "TBossHead");
        assert_eq!(indices[&roots["TBoss"]], 1);
    }

    #[test]
    fn primary_actor_model_prefers_initial_creation_over_state_swaps() {
        let text = r#"
            void THive::breakApart() {
                mMActor = mMActorKeeper->getMActor("broken.bmd");
            }
            void THive::loadAfter() {
                mMActor = mMActorKeeper->createMActor("intact.bmd", 3);
            }
        "#;
        let models = extract_enemy_actor_primary_models(text)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(models["THive"], "intact.bmd");
    }

    #[test]
    fn keeps_compare_only_factory_names() {
        let text = r#"
            if (strcmp(name, "coin") == 0)
                return gpItemManager->unk78;
        "#;
        let mut registry = ObjectRegistry::default();
        extract_string_factory_returns(text, "MapObj", SchemaSource::MapObjManager, &mut registry);
        assert_eq!(registry.objects[0].factory_name, "coin");
        assert_eq!(registry.objects[0].class_name, "Unknown");
    }

    #[test]
    fn discovers_particle_resources_and_calc_joint_bindings() {
        let resources = r#"
            gpResourceManager->load("ms_glow.jpa", 7);
        "#;
        let actor = r#"
            void TExample::calc()
            {
                MtxPtr effectMtx = mMActor->getModel()->mNodeMatrices[2];
                gpMarioParticleManager->emitAndBindToMtxPtr(7, effectMtx, 1, this);
            }
        "#;
        let mut registry = ObjectRegistry::default();
        extract_particle_resources(resources, "src/System/Resources.cpp", &mut registry);
        extract_calc_particle_bindings(actor, "src/MoveBG/Example.cpp", &mut registry);

        assert_eq!(registry.particle_resources[0].effect_id, 7);
        assert_eq!(registry.particle_resources[0].path, "ms_glow.jpa");
        assert_eq!(registry.actor_particle_bindings[0].class_name, "TExample");
        assert_eq!(
            registry.actor_particle_bindings[0].target,
            ParticleBindingTarget::ModelJoint(2)
        );
    }

    #[test]
    fn ignores_transient_particle_emissions_outside_calc() {
        let actor = r#"
            void TExample::explode()
            {
                gpMarioParticleManager->emitAndBindToPosPtr(0x80, &mPosition, 1, this);
            }
        "#;
        let mut registry = ObjectRegistry::default();
        extract_calc_particle_bindings(actor, "src/MoveBG/Example.cpp", &mut registry);

        assert!(registry.actor_particle_bindings.is_empty());
    }

    #[test]
    fn extracts_npc_parts_and_palettes_from_initializers() {
        let text = r#"
            static const GXColorS10 sHatColors0[] = {
                { 10, 20, 30, 255 }, { -40, 50, 60, 255 },
            };
            static const GXColorS10 sHatColors1[] = {
                { 70, 80, 90, 255 }, { 100, 110, 120, 255 },
            };
            static const TColorChangeInfo sHatChange = {
                0x00000002, "_hat", sHatColors0, sHatColors1
            };
            static const TNpcModelData sHatData = {
                "kubi", 0, { "customHat.bmd" }, { { &sHatChange, 0 } }, 1, 1, 1,
            };
            static TNpcModelData sRodData = {
                cNpcPartsNameRootJoint, 0, { "customRod.bmd" }, {}, 0, 0, 0,
            };
            static const TNpcInitInfo sMareM_InitData = {
                nullptr, { &sHatData, nullptr, &sRodData }, {}, 1.0f, 2.0f, 3.0f, 4.0f,
            };
        "#;
        let actors = extract_npc_actor_definitions(text, "src/NPC/NpcInitData.cpp");
        let actor = &actors[0];
        assert_eq!(actor.actor_key, "MareM");
        assert_eq!(actor.parts.len(), 2);
        assert_eq!(actor.parts[0].bit_index, 0);
        assert_eq!(actor.parts[0].color_index_channel, 1);
        assert_eq!(actor.parts[0].models[0].joint_name.as_deref(), Some("kubi"));
        assert_eq!(actor.parts[0].models[0].model_name, "customHat.bmd");
        assert_eq!(actor.parts[0].color_changes[0].mode, 2);
        assert_eq!(
            actor.parts[0].color_changes[0].colors0[1],
            [-40, 50, 60, 255]
        );
        assert!(actor.parts[0].uses_pollution);
        assert!(actor.parts[0].uses_shared_materials);
        assert_eq!(actor.parts[1].bit_index, 2);
        assert_eq!(actor.parts[1].models[0].joint_name, None);
    }

    #[test]
    fn extracts_map_static_model_paths_and_loader_flags_from_decomp_table() {
        let text = r#"
            static const TMapStaticObj::ActorDataTableEntry actor_data_table[] = {
                { "BiancoRiver", 0, 0, 0.0f, 0.0f, 0.0f, 0.0f, nullptr,
                  "BiancoRiver", 0x10210000, nullptr, 0, 0xFFFFFFFF, 0, 0, 0, 0x40 },
                { "CommonThing", 0, 0, 0.0f, 0.0f, 0.0f, 0.0f, nullptr,
                  "SharedModel", 0x11220000, nullptr, 0, 0xFFFFFFFF, 0, 0, 0, 0x2 },
                { "NoModel", 0, 0, 0.0f, 0.0f, 0.0f, 0.0f, nullptr,
                  nullptr, 0x10210000, nullptr, 0, 0xFFFFFFFF, 0, 0, 0, 0 },
            };
        "#;

        let models = extract_map_static_models(text, "src/Map/MapStaticObject.cpp");

        assert_eq!(models.len(), 3);
        assert_eq!(models[0].actor_name, "BiancoRiver");
        assert_eq!(
            models[0].model_path.as_deref(),
            Some("/scene/map/map/BiancoRiver.bmd")
        );
        assert_eq!(models[0].load_flags, 0x1021_0000);
        assert_eq!(
            models[1].model_path.as_deref(),
            Some("/common/map/SharedModel.bmd")
        );
        assert_eq!(models[1].load_flags, 0x1122_0000);
        assert_eq!(models[2].actor_name, "NoModel");
        assert_eq!(models[2].model_path, None);
        assert_eq!(models[2].load_flags, 0x1021_0000);
    }

    #[test]
    fn extracts_stage_bootstrap_map_static_actors_from_decomp_function() {
        let text = r#"
            static void initMare() {
                TMapStaticObj* gate = new TMapStaticObj("gate");
                gate->init("Mare5ExGate");
            }

            static void initStageCommon() {
                if (map == 4 || map <= 1) {
                    TMapStaticObj* waveFar = new TMapStaticObj("wave");
                    waveFar->init("sea");
                    TMapStaticObj* indirectWave = new TMapStaticObj("indirect");
                    indirectWave->init("SeaIndirect");
                }
            }
        "#;

        assert_eq!(
            extract_stage_bootstrap_map_static_actors(text),
            ["sea", "SeaIndirect"]
        );
    }

    #[test]
    fn npc_schema_lookup_prefers_the_longest_actor_key() {
        let registry = ObjectRegistry {
            npc_actors: vec![
                NpcActorDefinition {
                    actor_key: "MareM".to_string(),
                    source_file: String::new(),
                    parts: Vec::new(),
                },
                NpcActorDefinition {
                    actor_key: "MareMB".to_string(),
                    source_file: String::new(),
                    parts: Vec::new(),
                },
            ],
            ..ObjectRegistry::default()
        };
        assert_eq!(
            registry.find_npc_actor("NPCMareMA").unwrap().actor_key,
            "MareM"
        );
        assert_eq!(
            registry.find_npc_actor("NPCMareMB").unwrap().actor_key,
            "MareMB"
        );
        assert!(registry.find_npc_actor("npcMareMB").is_none());
        assert!(registry.find_npc_actor("NPCmareMB").is_none());
    }

    #[test]
    fn map_static_bootstrap_factory_identity_is_case_sensitive() {
        let fixture = complete_generator_fixture();
        fixture.write(
            "src/Map/Map.cpp",
            r#"static void initStageCommon() { actor->init("fixturemap"); }"#,
        );

        let registry = SchemaGenerator::new(&fixture.root).generate().unwrap();
        let fixture_map = registry
            .map_static_models
            .iter()
            .find(|model| model.actor_name == "FixtureMap")
            .unwrap();
        assert!(!fixture_map.stage_bootstrap_created);
    }

    #[test]
    fn overlay_updates_existing_objects() {
        let mut registry = ObjectRegistry {
            objects: vec![ObjectDefinition {
                factory_name: "coin".to_string(),
                class_name: "TCoin".to_string(),
                category: "MapObj".to_string(),
                source: SchemaSource::MapObjManager,
                display_name: None,
                preview_model: None,
                hidden: false,
                unsafe_to_edit: false,
            }],
            ..Default::default()
        };

        registry.apply_overlay(SchemaOverlay {
            objects: vec![ObjectOverlay {
                factory_name: "coin".to_string(),
                class_name: None,
                category: Some("Item".to_string()),
                display_name: Some("Coin".to_string()),
                preview_model: Some("/scene/mapObj/coin.bmd".to_string()),
                hidden: None,
                unsafe_to_edit: Some(true),
            }],
        });

        let coin = registry.find_object("coin").unwrap();
        assert_eq!(coin.class_name, "TCoin");
        assert_eq!(coin.category, "Item");
        assert!(coin.unsafe_to_edit);
    }

    #[test]
    fn overlay_updates_existing_object_class_name() {
        let mut registry = ObjectRegistry {
            objects: vec![ObjectDefinition {
                factory_name: "coin".to_string(),
                class_name: "Unknown".to_string(),
                category: "MapObj".to_string(),
                source: SchemaSource::MapObjManager,
                display_name: None,
                preview_model: None,
                hidden: false,
                unsafe_to_edit: false,
            }],
            ..Default::default()
        };

        registry.apply_overlay(SchemaOverlay {
            objects: vec![ObjectOverlay {
                factory_name: "coin".to_string(),
                class_name: Some("TCoin".to_string()),
                category: None,
                display_name: None,
                preview_model: None,
                hidden: None,
                unsafe_to_edit: None,
            }],
        });

        assert_eq!(registry.find_object("coin").unwrap().class_name, "TCoin");
    }

    #[test]
    fn extracts_root_npc_material_color_channels() {
        let text = r#"
            static const GXColorS10 sBodyColors[] = {
                { 10, 20, 30, 255 }, { 40, 50, 60, 255 },
            };
            static const GXColorS10 sClothReg1[] = {
                { 70, 80, 90, 255 },
            };
            static const GXColorS10 sClothReg2[] = {
                { 100, 110, 120, 255 },
            };
            static const TColorChangeInfo sBody = {
                0x00000001, "_hand_mat", sBodyColors, nullptr
            };
            static const TColorChangeInfo sCloth = {
                0x00000002, "_fuku_mat", sClothReg1, sClothReg2
            };
            static const TNpcInitInfo sMonteMA_InitData = {
                nullptr, {}, { { &sBody, nullptr }, { &sCloth } },
                1.0f, 2.0f, 3.0f, 4.0f,
            };
        "#;

        let colors = extract_npc_material_color_definitions(text, "src/NPC/NpcInitData.cpp");
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0].actor_key, "MonteMA");
        assert_eq!(colors[0].color_index_channel, 0);
        assert_eq!(colors[0].model_index, 0);
        assert_eq!(colors[0].change.mode, 1);
        assert_eq!(colors[0].change.colors0[1], [40, 50, 60, 255]);
        assert_eq!(colors[1].color_index_channel, 1);
        assert_eq!(colors[1].model_index, 0);
        assert_eq!(colors[1].change.mode, 2);
        assert_eq!(colors[1].change.colors1[0], [100, 110, 120, 255]);
    }

    #[test]
    fn extracts_explicit_npc_resource_bases() {
        let text = r#"
            void TMareMBaseManager::createModelData() {
                static const TModelDataLoadEntry entry[] = {
                    { "mareM.bmd", 0x10300000, 0 },
                    { nullptr, 0, 0 },
                };
                createModelDataArrayBase(entry, "/scene/mareM");
            }
        "#;

        let bases = extract_npc_manager_resource_bases(text);
        assert_eq!(bases["TMareMBaseManager"], "/scene/mareM");
    }

    #[test]
    fn generator_fixture_is_deterministic_and_rejects_extractor_drift() {
        let fixture = complete_generator_fixture();

        let first = SchemaGenerator::new(&fixture.root).generate().unwrap();
        let second = SchemaGenerator::new(&fixture.root).generate().unwrap();
        assert_eq!(first, second);
        assert!(!first
            .objects
            .iter()
            .any(|object| object.factory_name == "H_ma_rak_dummy"));
        let primary = first.primary_object_resource("NPCExample").unwrap();
        assert_eq!(primary.model_index, 0);
        assert_eq!(primary.role, ObjectResourceRole::Primary);
        assert_eq!(primary.model_name, "example.bmd");
        assert_eq!(primary.resource_base.as_deref(), Some("/scene/example"));
        assert_eq!(first.npc_material_colors_for("NPCExampleA").count(), 1);
        assert!(first.is_map_obj_factory("MapObjBase"));
        assert!(!first.is_map_obj_factory("MapObjFlag"));
        assert_eq!(
            first
                .collision_surfaces
                .iter()
                .find(|surface| surface.name == "BG_TYPE_SHADED_WET_GROUND")
                .map(|surface| surface.value),
            Some(0x4004)
        );
        assert_eq!(
            first
                .find_map_obj_resource("FixtureMapObj")
                .and_then(|resource| resource.primary_model.as_deref()),
            Some("FixturePrimary.bmd")
        );
        let fixture_resource = first.find_map_obj_resource("FixtureMapObj").unwrap();
        assert_eq!(first.moving_collision_vertex_limit, Some(350));
        assert_eq!(fixture_resource.collision_resources.len(), 1);
        assert_eq!(
            fixture_resource.collision_resources[0].resource_name,
            "FixturePrimary"
        );
        assert_eq!(fixture_resource.collision_resources[0].collision_kind, 1);
        assert_eq!(
            fixture_resource.collision_resources[0].max_vertices,
            Some(350)
        );
        let shared = first
            .find_map_obj_model_override("SharedFixture", "SharedFixture")
            .expect("fixture shared model override");
        assert_eq!(shared.model_path, "/scene/mapObj/shared_fixture.bmd");
        assert_eq!(
            shared.tev_color,
            Some(MapObjTevColorDefinition {
                register: 1,
                color: [1, 2, 3, 255]
            })
        );

        fixture.write(
            "src/System/MarNameRefGen_BossEnemy.cpp",
            r#"static const char* texture = "H_ma_rak_dummy";"#,
        );
        let error = SchemaGenerator::new(&fixture.root).generate().unwrap_err();
        assert!(matches!(
            error,
            SchemaError::ExtractionDrift {
                extractor: SchemaExtractor::FactoryRegistration,
                source_path,
                ..
            } if source_path.ends_with("MarNameRefGen_BossEnemy.cpp")
        ));
    }

    #[test]
    fn collision_surface_extractor_resolves_symbolic_compositions() {
        let definitions = extract_collision_surfaces(
            r#"
                enum BGTypeBits {
                    BG_TYPE_WET_GROUND = 0x4,
                    BG_PROPERTY_FLAG_SHADOW = 0x4000,
                    BG_PROPERTY_FLAG_CAMERA_WONT_CLIP = 0x8000,
                    BG_TYPE_SHADED_WET_GROUND
                        = BG_TYPE_WET_GROUND | BG_PROPERTY_FLAG_SHADOW,
                    BG_TYPE_CAM_NOCLIP_SHADED_WET_GROUND
                        = BG_TYPE_WET_GROUND | BG_PROPERTY_FLAG_SHADOW
                          | BG_PROPERTY_FLAG_CAMERA_WONT_CLIP, // 0xC004
                };
            "#,
            "include/Map/MapData.hpp",
        );
        let by_name = definitions
            .iter()
            .map(|definition| (definition.name.as_str(), definition))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_name["BG_TYPE_SHADED_WET_GROUND"].value, 0x4004);
        assert_eq!(
            by_name["BG_TYPE_CAM_NOCLIP_SHADED_WET_GROUND"].value,
            0xC004
        );
        assert!(by_name["BG_PROPERTY_FLAG_SHADOW"].is_property_flag);
        assert!(!by_name["BG_TYPE_WET_GROUND"].is_property_flag);
    }

    #[test]
    fn registry_rejects_ambiguous_primary_object_resources() {
        let object = ObjectDefinition {
            factory_name: "NPCExample".to_string(),
            class_name: "TBaseNPC".to_string(),
            category: "NPC".to_string(),
            source: SchemaSource::MarNameRefGen,
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        };
        let resource = |model_index| ObjectResourceBinding {
            factory_name: "NPCExample".to_string(),
            model_index,
            role: ObjectResourceRole::Primary,
            model_name: format!("example{model_index}.bmd"),
            resource_base: None,
            load_flags: 0x1021_0000,
            source_file: "src/NPC/NpcManager.cpp".to_string(),
        };
        let registry = ObjectRegistry {
            objects: vec![object],
            object_resources: vec![resource(0), resource(1)],
            ..ObjectRegistry::default()
        };

        let error = validate_registry(&registry).unwrap_err();
        assert!(
            matches!(error, SchemaError::RegistryInvariant { detail } if detail.contains("2 primary"))
        );
    }

    #[test]
    fn map_obj_resource_lookup_is_exact_and_rejects_duplicate_identities() {
        let resource = MapObjResourceDefinition {
            resource_name: "WoodBox".to_string(),
            actor_type: 0x4000_0003,
            object_flags: 0,
            required_manager_name: "map object manager".to_string(),
            has_hold_dependency: false,
            has_move_dependency: false,
            uses_resource_name_model_fallback: true,
            primary_model: Some("kibako.bmd".to_string()),
            animation_resources: Vec::new(),
            hold_model_path: None,
            move_bck_path: None,
            load_flags: 0x1022_0000,
            collision_resources: Vec::new(),
            source_file: "src/MoveBG/MapObjInit.cpp".to_string(),
        };
        let mut registry = ObjectRegistry {
            objects: vec![ObjectDefinition {
                factory_name: "MapObjBase".to_string(),
                class_name: "TMapObjBase".to_string(),
                category: "MapObj".to_string(),
                source: SchemaSource::MarNameRefGen,
                display_name: None,
                preview_model: None,
                hidden: false,
                unsafe_to_edit: false,
            }],
            map_obj_resources: vec![resource.clone()],
            ..ObjectRegistry::default()
        };
        assert_eq!(
            registry
                .find_map_obj_resource("WoodBox")
                .and_then(|definition| definition.primary_model.as_deref()),
            Some("kibako.bmd")
        );
        assert!(registry.find_map_obj_resource("woodbox").is_none());

        registry.map_obj_resources.push(resource);
        let error = dedup_registry(&mut registry).unwrap_err();
        assert!(
            matches!(error, SchemaError::RegistryInvariant { detail } if detail.contains("duplicate map-object resource identity WoodBox"))
        );
    }

    #[test]
    fn legacy_map_obj_resources_deserialize_with_runtime_default_loader_flags() {
        let resource: MapObjResourceDefinition = toml::from_str(
            r#"
                resource_name = "WoodBox"
                primary_model = "kibako.bmd"
                source_file = "src/MoveBG/MapObjInit.cpp"
            "#,
        )
        .expect("deserialize pre-loader-flags schema");
        assert_eq!(resource.actor_type, 0);
        assert_eq!(resource.object_flags, 0);
        assert!(resource.required_manager_name.is_empty());
        assert!(!resource.has_hold_dependency);
        assert!(!resource.has_move_dependency);
        assert!(!resource.uses_resource_name_model_fallback);
        assert!(resource.animation_resources.is_empty());
        assert_eq!(resource.hold_model_path, None);
        assert_eq!(resource.move_bck_path, None);
        assert_eq!(resource.load_flags, 0x1022_0000);

        let map_static: MapStaticModelDefinition = toml::from_str(
            r#"
                actor_name = "BiancoRiver"
                model_path = "/scene/map/map/BiancoRiver.bmd"
                load_flags = 270598144
                source_file = "src/Map/MapStaticObject.cpp"
            "#,
        )
        .expect("deserialize pre-optional-path map-static schema");
        assert_eq!(
            map_static.model_path.as_deref(),
            Some("/scene/map/map/BiancoRiver.bmd")
        );
    }

    #[test]
    fn case_distinct_retail_factory_names_remain_distinct() {
        let object = |factory_name: &str, class_name: &str| ObjectDefinition {
            factory_name: factory_name.to_string(),
            class_name: class_name.to_string(),
            category: "MapObj".to_string(),
            source: SchemaSource::MapObjManager,
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        };
        let mut registry = ObjectRegistry {
            objects: vec![
                object("maregate", "TMapObjBase"),
                object("MareGate", "TMareGate"),
            ],
            ..ObjectRegistry::default()
        };

        dedup_registry(&mut registry).unwrap();
        validate_registry(&registry).unwrap();

        assert_eq!(registry.objects.len(), 2);
        assert_eq!(
            registry.find_object("maregate").unwrap().class_name,
            "TMapObjBase"
        );
        assert_eq!(
            registry.find_object("MareGate").unwrap().class_name,
            "TMareGate"
        );
        assert!(registry.find_object("MAREGATE").is_none());
    }

    #[test]
    #[ignore = "requires the neighboring Super Mario Sunshine decompilation checkout"]
    fn generated_neighboring_decomp_schema_satisfies_registry_invariants() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = SchemaGenerator::new(root).generate().unwrap();

        assert!(!registry
            .objects
            .iter()
            .any(|object| object.factory_name == "H_ma_rak_dummy"));
        assert!(!registry.object_resources.is_empty());
        // MapObjInit declares 363 records, but retail lookup contains 359
        // resources plus its terminator. Three unregistered declarations must
        // not leak into the generated registry.
        assert_eq!(registry.map_obj_resources.len(), 359);
        assert_eq!(
            registry
                .map_obj_resources
                .iter()
                .filter(|resource| resource.primary_model.is_some())
                .count(),
            332
        );
        assert_eq!(
            registry
                .map_obj_resources
                .iter()
                .filter(|resource| resource.primary_model.is_none())
                .count(),
            27
        );
        let normal_block = registry
            .find_map_obj_resource("NormalBlock")
            .expect("decomp stock NormalBlock slot");
        assert_eq!(
            normal_block.primary_model.as_deref(),
            Some("NormalBlock.bmd")
        );
        assert_eq!(normal_block.load_flags, 0x1022_0000);
        assert_eq!(
            normal_block.required_manager_name,
            "地形オブジェマネージャー"
        );
        assert_eq!(normal_block.collision_resources.len(), 1);
        assert_eq!(
            normal_block.collision_resources[0],
            MapObjCollisionResourceDefinition {
                resource_name: "NormalBlock".to_string(),
                flags: 1,
                collision_kind: 1,
                max_vertices: Some(350),
            }
        );
        let z_turn_disk = registry
            .find_map_obj_resource("zTurnDisk")
            .expect("decomp stock zTurnDisk slot");
        assert!(z_turn_disk.uses_resource_name_model_fallback);
        assert_eq!(z_turn_disk.primary_model.as_deref(), Some("zTurnDisk.bmd"));
        assert!(!z_turn_disk.has_hold_dependency);
        assert!(z_turn_disk.has_move_dependency);
        assert!(z_turn_disk.animation_resources.is_empty());
        assert_eq!(
            z_turn_disk.move_bck_path.as_deref(),
            Some("/scene/mapObj/zTurnDisk.bck")
        );

        let wood_barrel = registry
            .find_map_obj_resource("wood_barrel")
            .expect("decomp stock wood_barrel slot");
        assert!(wood_barrel.has_hold_dependency);
        assert!(!wood_barrel.has_move_dependency);
        assert_eq!(
            wood_barrel.hold_model_path.as_deref(),
            Some("/scene/mapObj/barrel_offset.bmd")
        );
        assert_eq!(wood_barrel.animation_resources.len(), 7);
        assert_eq!(
            wood_barrel.animation_resources[0].model_name.as_deref(),
            Some("barrel_normal.bmd")
        );
        assert_eq!(
            wood_barrel.animation_resources[2].model_name.as_deref(),
            Some("barrel_crash.bmd")
        );
        assert_eq!(
            wood_barrel.animation_resources[2].animation_name.as_deref(),
            Some("barrel_crash")
        );
        assert_eq!(
            wood_barrel.animation_resources[2].bas_path.as_deref(),
            Some("/scene/mapObj/barrel_crash.bas")
        );
        assert_eq!(
            wood_barrel.animation_resources[5].animation_name.as_deref(),
            Some("barrel_rot")
        );
        assert_eq!(
            wood_barrel.animation_resources[5].bas_path.as_deref(),
            Some("/scene/mapObj/barrel_rot.bas")
        );
        assert_eq!(
            registry
                .map_obj_resources
                .iter()
                .filter(|resource| resource.load_flags == 0x1122_0000)
                .map(|resource| resource.resource_name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["DokanGate", "IceBlock"])
        );
        assert!(registry
            .map_obj_resources
            .iter()
            .filter(|resource| !matches!(resource.resource_name.as_str(), "DokanGate" | "IceBlock"))
            .all(|resource| resource.load_flags == 0x1022_0000));
        let ball_transforms = registry
            .map_obj_ball_transforms
            .iter()
            .map(|definition| (definition.actor_type, definition))
            .collect::<BTreeMap<_, _>>();
        for (actor_type, radius) in [
            (0x4000_0390, 40),
            (0x4000_0391, 40),
            (0x4000_0392, 50),
            (0x4000_0393, 45),
            (0x4000_0394, 50),
            (0x4000_0395, 50),
        ] {
            assert_eq!(ball_transforms[&actor_type].body_radius, radius);
        }
        assert_eq!(
            ball_transforms[&0x4000_0394].positive_y_axis_subtract,
            Some(50)
        );
        assert_eq!(
            ball_transforms[&0x4000_0392].one_minus_y_axis_subtract,
            Some(10)
        );
        assert_eq!(ball_transforms[&0x4000_0395].positive_y_axis_subtract, None);
        for (actor_name, has_model) in [
            ("mareSeaPollutionS0", true),
            ("mareSeaPollutionS12", true),
            ("mareSeaPollutionS34567", false),
        ] {
            let definition = registry
                .map_static_models
                .iter()
                .find(|definition| definition.actor_name == actor_name)
                .unwrap_or_else(|| panic!("missing map-static resource {actor_name}"));
            assert_eq!(definition.load_flags, 0x1021_0000, "{actor_name}");
            assert_eq!(definition.model_path.is_some(), has_model, "{actor_name}");
        }
        assert_eq!(registry.map_obj_model_overrides.len(), 4);
        for (factory_name, resource_name, model, color) in [
            (
                "SurfGesoRed",
                "SurfGesoRed",
                "/scene/mapObj/surfgeso.bmd",
                Some([255, 180, 255, 255]),
            ),
            (
                "SurfGesoYellow",
                "SurfGesoYellow",
                "/scene/mapObj/surfgeso.bmd",
                Some([255, 255, 125, 255]),
            ),
            (
                "SurfGesoGreen",
                "SurfGesoGreen",
                "/scene/mapObj/surfgeso.bmd",
                Some([180, 255, 180, 255]),
            ),
            ("Shine", "shine", "shine.bmd", None),
        ] {
            let definition = registry
                .find_map_obj_model_override(factory_name, resource_name)
                .unwrap_or_else(|| panic!("missing override {factory_name}/{resource_name}"));
            assert_eq!(definition.model_path, model);
            assert_eq!(definition.load_flags, 0x1022_0000);
            assert_eq!(
                definition.tev_color.map(|color| color.color),
                color,
                "{resource_name} color"
            );
        }
        assert_eq!(registry.map_obj_string_tev_programs.len(), 1);
        let nozzle = &registry.map_obj_string_tev_programs[0];
        assert_eq!(nozzle.resource_name, "NozzleBox");
        assert_eq!(nozzle.class_name, "TNozzleBox");
        assert_eq!(nozzle.tev_register, 1);
        assert_eq!(nozzle.default_color, [255, 255, 255, 100]);
        assert_eq!(
            nozzle
                .variants
                .iter()
                .map(|variant| (variant.selector_value.as_str(), variant.color))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("back_nozzle_item", [90, 90, 120, 100]),
                ("normal_nozzle_item", [0, 0, 255, 100]),
                ("rocket_nozzle_item", [255, 0, 0, 100]),
            ])
        );
        assert_eq!(
            registry.map_obj_stream_tev_colors,
            [MapObjStreamTevColorDefinition {
                class_name: "TWaterHitPictureHideObj".to_string(),
                tev_register: 0,
                trailing_rgb_u32_count: 3,
                alpha: 255,
                source_file: "src/MoveBG/MapObjHide.cpp".to_string(),
            }]
        );
        for unregistered in ["telegraph_pole_l", "telegraph_pole_s", "move_ice_car"] {
            assert!(registry.find_map_obj_resource(unregistered).is_none());
        }
        assert_eq!(
            registry
                .find_map_obj_resource("WoodBox")
                .and_then(|resource| resource.primary_model.as_deref()),
            Some("kibako.bmd")
        );
        assert_eq!(
            registry
                .find_map_obj_resource("FruitPapaya")
                .and_then(|resource| resource.primary_model.as_deref()),
            Some("FruitPapaya.bmd")
        );
        assert_eq!(
            registry
                .find_map_obj_resource("MapSmoke")
                .and_then(|resource| resource.primary_model.as_deref()),
            None
        );
        for factory_name in ["MapObjBase", "MareEventBumpyWall", "WoodBox", "ResetFruit"] {
            assert!(
                registry.is_map_obj_factory(factory_name),
                "missing TMapObjBase-derived factory {factory_name}"
            );
        }
        for factory_name in ["MapStaticObj", "MapObjFlag", "NPCMareMA", "Mario"] {
            assert!(
                !registry.is_map_obj_factory(factory_name),
                "non-TMapObjBase factory {factory_name} was misclassified"
            );
        }
        assert!(!registry.npc_material_colors.is_empty());
        for factory_name in [
            "mario_cap",
            "bottle_large",
            "bottle_short",
            "GesoSurfBoardStatic",
            "GesoSurfBoard",
        ] {
            let object = registry
                .find_object(factory_name)
                .unwrap_or_else(|| panic!("missing table-driven factory {factory_name}"));
            assert_eq!(object.class_name, "TItem");
        }
        for (factory_name, class_name) in [
            ("Map", "TMap"),
            ("Sky", "TSky"),
            ("Shimmer", "TShimmer"),
            ("Pollution", "TPollutionManager"),
            ("MapObjBase", "TMapObjBase"),
            ("MapObjGeneral", "TMapObjGeneral"),
            ("MapStaticObj", "TMapStaticObj"),
            ("WoodBarrel", "TWoodBarrel"),
            ("NozzleBox", "TNozzleBox"),
            ("Coin", "TCoin"),
        ] {
            let object = registry
                .find_object(factory_name)
                .unwrap_or_else(|| panic!("missing factory {factory_name}"));
            assert_eq!(object.class_name, class_name);
        }
        assert_eq!(
            registry.find_object("maregate").unwrap().class_name,
            "TMapObjBase"
        );
        assert_eq!(
            registry.find_object("MareGate").unwrap().class_name,
            "TMareGate"
        );
        let mare = registry
            .primary_object_resource("NPCMareMA")
            .expect("Mare variants inherit the MareM root model binding");
        assert_eq!(mare.model_name, "mareM.bmd");
        assert_eq!(mare.resource_base.as_deref(), Some("/scene/mareM"));
        assert_eq!(mare.load_flags, 0x1030_0000);
        let monte = registry
            .primary_object_resource("NPCMonteMH")
            .expect("Monte MH has a decomp-derived root model binding");
        assert_eq!(monte.model_name, "momA_model.bmd");
        assert_eq!(monte.resource_base.as_deref(), Some("/scene/monteMA"));
        assert_eq!(registry.npc_material_colors_for("NPCMonteMA").count(), 2);
        assert_eq!(registry.npc_material_colors_for("NPCMareMA").count(), 1);
    }

    fn complete_generator_fixture() -> SchemaFixture {
        let fixture = SchemaFixture::new("complete");
        fixture.write(
            "include/Map/MapData.hpp",
            r#"
                enum BGTypeBits {
                    BG_TYPE_WET_GROUND = 0x4,
                    BG_TYPE_WATER = 0x100,
                    BG_PROPERTY_FLAG_SHADOW = 0x4000,
                    BG_PROPERTY_FLAG_CAMERA_WONT_CLIP = 0x8000,
                    BG_TYPE_SHADED_WET_GROUND
                        = BG_TYPE_WET_GROUND | BG_PROPERTY_FLAG_SHADOW,
                };
            "#,
        );
        fixture.write(
            "include/JSystem/J3D/J3DGraphLoader/J3DModelLoaderFlags.hpp",
            "J3DMLF_Test = 0x10000000;",
        );
        fixture.write(
            "src/System/MarNameRefGen.cpp",
            r#"if (strcmp(name, "Mario") == 0) return new TMario;"#,
        );
        fixture.write(
            "src/System/MarNameRefGen_Enemy.cpp",
            r#"
                if (strcmp(name, "FixtureEnemy") == 0) return new TFixtureEnemy;
                if (strcmp(name, "FixtureEnemyManager") == 0) return new TFixtureEnemyManager;
            "#,
        );
        fixture.write(
            "src/System/MarNameRefGen_BossEnemy.cpp",
            r#"
                static const char* texture = "H_ma_rak_dummy";
                if (strcmp(name, "FixtureBoss") == 0) return new TFixtureBoss;
                if (strcmp(name, "FixtureBossManager") == 0) return new TFixtureBossManager;
            "#,
        );
        fixture.write(
            "src/System/MarNameRefGen_Map.cpp",
            r#"if (strcmp(name, "Map") == 0) return new TMap;"#,
        );
        fixture.write(
            "src/System/MarNameRefGen_MapObj.cpp",
            r#"
                if (strcmp(name, "MapObjBase") == 0) return new TMapObjBase;
                if (strcmp(name, "MapObjFlag") == 0) return new TMapObjFlag("flag");
                if (strcmp(name, "SharedFixture") == 0) return new TMapObjBase;
                if (strcmp(name, "NozzleBox") == 0) return new TNozzleBox("box");
                if (strcmp(name, "FixturePaint") == 0) return new TFixturePaint("paint");
            "#,
        );
        fixture.write(
            "src/MoveBG/MapObjManager.cpp",
            r#"
                if (strcmp(name, "coin") == 0) return gpItemManager->coin;
                mSharedFixtureModelData = SMS_MakeSDLModelData(
                    "/scene/mapObj/shared_fixture.bmd", 0x10220000);
            "#,
        );
        fixture.write(
            "src/MoveBG/MapObjInit.cpp",
            r#"
                static const TMapObjCollisionData fixture_collision_data[] = {
                    { "FixturePrimary", 1 },
                };
                static const TMapObjCollisionInfo fixture_collision_info = {
                    1, 1, fixture_collision_data,
                };
                static const TMapObjAnimData fixture_anim_data[] = {
                    { "FixturePrimary.bmd", nullptr, 0, nullptr, nullptr },
                };
                static const TMapObjAnimDataInfo fixture_anim_info = {
                    1, 1, fixture_anim_data,
                };
                static const TMapObjAnimDataInfo no_data_anim_info = { 0, 0, nullptr };
                static TMapObjData fixture_data = {
                    "FixtureMapObj", 0x40000001, "fixture map object manager", nullptr, &fixture_anim_info,
                    nullptr, &fixture_collision_info, nullptr, nullptr, nullptr, nullptr, nullptr,
                    0.0f, 0x00000000, 0,
                };
                static TMapObjData shared_fixture_data = {
                    "SharedFixture", 0x40000002, "fixture map object manager", nullptr, &no_data_anim_info,
                    nullptr, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
                    0.0f, 0x00008000, 0,
                };
                static TMapObjData nozzle_box_data = {
                    "NozzleBox", 0x40000003, "fixture map object manager", nullptr, nullptr,
                    nullptr, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
                    0.0f, 0x00000000, 0,
                };
                static TMapObjData end_data = {
                    nullptr, 0, nullptr, nullptr, &no_data_anim_info,
                    nullptr, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr,
                    0.0f, 0x00000000, 0,
                };
                static TMapObjData* sObjDataTable[] = {
                    &fixture_data, &shared_fixture_data, &nozzle_box_data, &end_data
                };
                void TMapObjBase::makeMActors() {
                    if (unkF8 & 0x8000) {
                        mMActorKeeper->mModelLoaderFlags = 0x11220000;
                    } else {
                        mMActorKeeper->mModelLoaderFlags = 0x10220000;
                    }
                    if (mMapObjData->mAnim) {
                        mMActor = initMActor(mMapObjData->mAnim->unk4[0].unk0, nullptr, 0);
                    } else {
                        char buffer[64];
                        snprintf(buffer, 64, "%s.bmd", mMapObjData->unk0);
                        mMActor = initMActor(buffer, nullptr, 0);
                    }
                }
                void TMapObjBase::initActorData() {
                    unkF8 = mMapObjData->unk34;
                }
            "#,
        );
        fixture.write(
            "src/Map/MapMakeData.cpp",
            "void TMapCollisionBase::update() { Vec transformed[350]; }",
        );
        fixture.write(
            "src/MoveBG/SharedFixture.cpp",
            r#"
                void TMapObjBase::initMapObj() {
                    if (strcmp(unkF4, "SharedFixture") == 0) {
                        tint.r = 1; tint.g = 2; tint.b = 3; tint.a = 255;
                    }
                    SDLModelData* modelData = gpMapObjManager->mSharedFixtureModelData;
                    mMActor = SMS_MakeMActorFromSDLModelData(modelData, animationData, 3);
                    initPacketMatColor(getModel(), GX_TEVREG1, &tint);
                }
            "#,
        );
        fixture.write(
            "include/MoveBG/Item.hpp",
            r#"
                class TNozzleBox : public TMapObjGeneral {
                    /* 0x15E */ u16 unk15E;
                    /* 0x160 */ u16 unk160;
                    /* 0x162 */ u16 unk162;
                    /* 0x164 */ u16 unk164;
                };
            "#,
        );
        fixture.write(
            "src/MoveBG/Item.cpp",
            r#"
                TNozzleBox::TNozzleBox(const char* name)
                    : TMapObjGeneral(name), unk15E(0xFF), unk160(0xFF),
                      unk162(0xFF), unk164(100) {}
                void TNozzleBox::load(JSUMemoryInputStream& stream) {
                    TMapObjBase::load(stream);
                    unk158 = stream.readString();
                    if (strcmp(unk158, "normal_nozzle_item") == 0) {
                        unk15E = 0; unk160 = 0; unk162 = 0xFF;
                    } else if (strcmp(unk158, "rocket_nozzle_item") == 0) {
                        unk15E = 0xFF; unk160 = 0; unk162 = 0;
                    } else if (strcmp(unk158, "back_nozzle_item") == 0) {
                        unk15E = 0x5A; unk160 = 0x5A; unk162 = 0x78;
                    }
                    initPacketMatColor(getModel(), GX_TEVREG1,
                        (const GXColorS10*)&unk15E);
                }
            "#,
        );
        fixture.write(
            "src/MoveBG/MapObjBall.cpp",
            r#"
                void TMapObjBall::initMapObj() {
                    switch (mActorType) {
                    case 0x40000001:
                        mBodyRadius = 50.0f * mScaling.y;
                        break;
                    }
                }
                void TResetFruit::makeObjAppeared() {
                    (*m)[1][3] = mPosition.y + mBodyRadius;
                }
            "#,
        );
        fixture.write(
            "include/MoveBG/FixturePaint.hpp",
            "class TFixturePaint : public TMapObjBase {};",
        );
        fixture.write(
            "src/MoveBG/FixturePaint.cpp",
            r#"
                void TFixturePaint::load(JSUMemoryInputStream& stream) {
                    TMapObjBase::load(stream);
                    u32 r, g, b;
                    stream.read(&r, 4);
                    stream.read(&g, 4);
                    stream.read(&b, 4);
                    tint0 = (u16)(u8)r;
                    tint1 = (u16)(u8)g;
                    tint2 = (u16)(u8)b;
                    tint3 = 255;
                    initPacketMatColor(getModel(), GX_TEVREG1, (GXColorS10*)&tint0);
                }
            "#,
        );
        fixture.write(
            "src/Enemy/Fixture.cpp",
            r#"
                PARAM_INIT(mFixture, 1)
                static const char* model = "/scene/fixture/model.bmd";
                void loadFixture() { gpResourceManager->load("fixture.jpa", 7); }
                void TFixtureEnemy::calc() {
                    gpMarioParticleManager->emitAndBindToPosPtr(7, &mPosition, 1, this);
                }
            "#,
        );
        fixture.write(
            "src/Map/MapStaticObject.cpp",
            r#"
                static const TMapStaticObj::ActorDataTableEntry actor_data_table[] = {
                    { "FixtureMap", 0, 0, 0.0f, 0.0f, 0.0f, 0.0f, nullptr,
                      "FixtureMap", 0x10210000, nullptr, 0, 0xFFFFFFFF, 0, 0, 0, 0x40 },
                };
            "#,
        );
        fixture.write(
            "src/Map/Map.cpp",
            r#"static void initStageCommon() { actor->init("FixtureMap"); }"#,
        );
        fixture.write(
            "src/MoveBG/MapObjFlag.cpp",
            r#"
                f32 TMapObjFlag::mFlutterSpeed = 4.0f;
                #define REGISTER_FLAG(N, NAME) snprintf(buf, 64, "/scene/mapObj/%s.bti", name);
                void TMapObjFlagManager::registerObj(TMapObjFlag*, const char*) {
                    REGISTER_FLAG(0, "flagSun")
                }
                void TMapObjFlag::load(JSUMemoryInputStream& stream) {
                    char buf[64]; stream.readString(buf, 64); init(buf);
                }
                TMapObjFlag::TMapObjFlag(const char*) {
                    mFlagHeight = 125.0f; mFlagWidth = 130.0f; mSegmentSize = 20.0f;
                }
                void TMapObjFlagManager::load(JSUMemoryInputStream&) {
                    switch (gpMarDirector->mMap) {
                    case 2: TMapObjFlag::mFlutterSpeed = 16.0f; break;
                    default: TMapObjFlag::mFlutterSpeed = 8.0f; break;
                    }
                }
                void TMapObjFlagManager::perform(u32, JDrama::TGraphics*) {
                    flag->mPhase += TMapObjFlag::mFlutterSpeed;
                    if (flag->mPhase > 360.0f) flag->mPhase -= 360.0f;
                }
            "#,
        );
        fixture.write(
            "src/System/Application.cpp",
            r#"bufStageArcBin = JKRDvdRipper::loadToMainRAM("/data/stageArc.bin", nullptr, EXPAND_SWITCH_DEFAULT, 0, mHeap);"#,
        );
        fixture.write(
            "src/System/MarNameRefGen_NPC.cpp",
            r#"
                if (strcmp(name, "NPCExample") == 0) return new TBaseNPC(1, "?");
                if (strcmp(name, "ExampleManager") == 0) return new TExampleManager;
            "#,
        );
        fixture.write(
            "src/NPC/NpcManager.cpp",
            r#"
                void TExampleManager::createModelData() {
                    static const TModelDataLoadEntry entry[] = {
                        { "example.bmd", 0x10210000, 0 }, { nullptr, 0, 0 },
                    };
                    createModelDataArrayBase(entry, "/scene/example");
                }
            "#,
        );
        fixture.write(
            "src/NPC/NpcInitData.cpp",
            r#"
                static const GXColorS10 sBodyColors[] = { { 1, 2, 3, 255 } };
                static const TColorChangeInfo sBody = {
                    0x00000001, "_body", sBodyColors, nullptr
                };
                static const TNpcInitInfo sExample_InitData = {
                    nullptr, {}, { { &sBody } }, 1.0f, 2.0f, 3.0f, 4.0f,
                };
            "#,
        );
        fixture
    }
}
