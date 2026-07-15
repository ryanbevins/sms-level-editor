//! Decomp-derived object and parameter registry generation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::WalkDir;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("schema source is missing: {0}")]
    MissingSource(PathBuf),
}

pub type Result<T> = std::result::Result<T, SchemaError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObjectRegistry {
    pub objects: Vec<ObjectDefinition>,
    pub params: Vec<ParamDefinition>,
    pub asset_hints: Vec<AssetHint>,
    #[serde(default)]
    pub map_static_models: Vec<MapStaticModelDefinition>,
    #[serde(default)]
    pub particle_resources: Vec<ParticleResourceDefinition>,
    #[serde(default)]
    pub actor_particle_bindings: Vec<ActorParticleBinding>,
    #[serde(default)]
    pub npc_actors: Vec<NpcActorDefinition>,
    #[serde(default)]
    pub enemy_managers: Vec<EnemyManagerDefinition>,
    #[serde(default)]
    pub enemy_actors: Vec<EnemyActorDefinition>,
    #[serde(default)]
    pub enemy_material_colors: Vec<EnemyMaterialTevColorDefinition>,
    #[serde(default)]
    pub map_obj_flags: Vec<MapObjFlagDefinition>,
}

impl ObjectRegistry {
    pub fn find_object(&self, factory_name: &str) -> Option<&ObjectDefinition> {
        self.objects
            .iter()
            .find(|object| object.factory_name == factory_name)
    }

    pub fn find_npc_actor(&self, factory_name: &str) -> Option<&NpcActorDefinition> {
        let actor_key = factory_name
            .strip_prefix("NPC")
            .or_else(|| factory_name.strip_prefix("npc"))?;
        self.npc_actors
            .iter()
            .filter(|definition| {
                actor_key
                    .to_ascii_lowercase()
                    .starts_with(&definition.actor_key.to_ascii_lowercase())
            })
            .max_by_key(|definition| definition.actor_key.len())
    }

    pub fn find_enemy_manager(&self, factory_name: &str) -> Option<&EnemyManagerDefinition> {
        self.enemy_managers
            .iter()
            .find(|definition| definition.factory_name.eq_ignore_ascii_case(factory_name))
    }

    pub fn find_enemy_actor(&self, factory_name: &str) -> Option<&EnemyActorDefinition> {
        self.enemy_actors
            .iter()
            .find(|definition| definition.factory_name.eq_ignore_ascii_case(factory_name))
    }

    pub fn apply_overlay(&mut self, overlay: SchemaOverlay) {
        let mut by_name: BTreeMap<String, ObjectOverlay> = overlay
            .objects
            .into_iter()
            .map(|object| (object.factory_name.clone(), object))
            .collect();

        for object in &mut self.objects {
            if let Some(overlay) = by_name.remove(&object.factory_name) {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapStaticModelDefinition {
    pub actor_name: String,
    pub model_path: String,
    pub load_flags: u32,
    pub source_file: String,
    #[serde(default)]
    pub stage_bootstrap_created: bool,
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
    pub models: Vec<EnemyModelDefinition>,
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
        let mut registry = ObjectRegistry::default();
        self.scan_mar_name_ref_gen(&mut registry)?;
        self.scan_enemy_model_data(&mut registry)?;
        self.scan_map_obj_manager(&mut registry)?;
        self.scan_params_and_assets(&mut registry)?;
        self.scan_map_static_models(&mut registry)?;
        self.scan_map_obj_flags(&mut registry)?;
        self.scan_particle_bindings(&mut registry)?;
        self.scan_npc_init_data(&mut registry)?;
        dedup_registry(&mut registry);
        Ok(registry)
    }

    pub fn load_overlay(&self, overlay_path: impl AsRef<Path>) -> Result<SchemaOverlay> {
        let text = fs::read_to_string(overlay_path)?;
        Ok(toml::from_str(&text)?)
    }

    fn scan_mar_name_ref_gen(&self, registry: &mut ObjectRegistry) -> Result<()> {
        for (file_name, category) in [
            ("MarNameRefGen.cpp", "System"),
            ("MarNameRefGen_Enemy.cpp", "Enemy"),
            ("MarNameRefGen_BossEnemy.cpp", "Boss"),
        ] {
            let path = self.repo_root.join("src/System").join(file_name);
            let text = read_required(&path)?;
            extract_string_factory_returns(&text, category, SchemaSource::MarNameRefGen, registry);
            if category != "System" {
                extract_enemy_factory_variants(&text, registry);
            }
        }
        Ok(())
    }

    fn scan_enemy_model_data(&self, registry: &mut ObjectRegistry) -> Result<()> {
        let mut class_models = BTreeMap::<String, Vec<EnemyModelDefinition>>::new();
        let mut actor_models = BTreeMap::<String, Vec<EnemyModelDefinition>>::new();
        let mut actor_primary_models = BTreeMap::<String, String>::new();
        let mut actor_named_models = BTreeMap::<String, Vec<EnemyNamedModelDefinition>>::new();
        let mut actor_indexed_models = BTreeMap::<String, Vec<EnemyIndexedModelDefinition>>::new();
        let mut owned_actor_classes = BTreeMap::<String, Vec<String>>::new();
        let mut actor_root_parts = BTreeMap::<String, String>::new();
        let mut part_model_indices = BTreeMap::<String, usize>::new();
        let mut manager_actor_classes = BTreeMap::<String, String>::new();
        let mut inheritance = BTreeMap::<String, String>::new();
        let mut tev_color_bindings = Vec::new();
        let mut init_tev_colors = BTreeMap::new();
        let flag_symbols = extract_cpp_u32_constants(&read_required(
            &self
                .repo_root
                .join("include/JSystem/J3D/J3DGraphLoader/J3DModelLoaderFlags.hpp"),
        )?);

        for entry in WalkDir::new(self.repo_root.join("src/Enemy"))
            .into_iter()
            .chain(WalkDir::new(self.repo_root.join("src/Animal")))
            .chain(WalkDir::new(self.repo_root.join("src/Strategic")))
            .chain(WalkDir::new(self.repo_root.join("include/Enemy")))
            .chain(WalkDir::new(self.repo_root.join("include/Animal")))
            .chain(WalkDir::new(self.repo_root.join("include/Strategic")))
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            let extension = path.extension().and_then(|extension| extension.to_str());
            if !matches!(extension, Some("cpp" | "hpp" | "h")) {
                continue;
            }
            let text = fs::read_to_string(path)?;
            extract_class_inheritance(&text, &mut inheritance);
            if extension == Some("cpp") {
                let source_file = normalize_source_path(&self.repo_root, path);
                for (class_name, models) in
                    extract_enemy_manager_models(&text, &source_file, &flag_symbols)
                {
                    class_models.entry(class_name).or_default().extend(models);
                }
                for (class_name, models) in extract_enemy_actor_fallback_models(&text, &source_file)
                {
                    actor_models.entry(class_name).or_default().extend(models);
                }
                for (class_name, model_name) in extract_enemy_actor_primary_models(&text) {
                    actor_primary_models.entry(class_name).or_insert(model_name);
                }
                for (class_name, models) in extract_enemy_named_models(&text, &source_file) {
                    actor_named_models
                        .entry(class_name)
                        .or_default()
                        .extend(models);
                }
                for (class_name, models) in extract_enemy_indexed_models(&text, &source_file) {
                    actor_indexed_models
                        .entry(class_name)
                        .or_default()
                        .extend(models);
                }
                extract_owned_actor_classes(&text, &mut owned_actor_classes);
                extract_actor_root_parts(&text, &mut actor_root_parts);
                extract_part_model_indices(&text, &mut part_model_indices);
                extract_enemy_manager_actor_classes(&text, &mut manager_actor_classes);
                tev_color_bindings.extend(extract_enemy_tev_color_bindings(&text, &source_file));
                extract_enemy_init_tev_colors(&text, &mut init_tev_colors);
            }
        }

        // A few retail-only manager subclasses are declared beside their
        // factory registrations rather than in public headers.
        for file_name in ["MarNameRefGen_Enemy.cpp", "MarNameRefGen_BossEnemy.cpp"] {
            let text = read_required(&self.repo_root.join("src/System").join(file_name))?;
            let mut supplemental = BTreeMap::new();
            extract_class_inheritance(&text, &mut supplemental);
            for (class_name, parent) in supplemental {
                inheritance.entry(class_name).or_insert(parent);
            }
        }

        for definition in &mut registry.enemy_actors {
            let Some(object) = registry.objects.iter().find(|object| {
                object
                    .factory_name
                    .eq_ignore_ascii_case(&definition.factory_name)
            }) else {
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
                .find(|definition| {
                    definition
                        .factory_name
                        .eq_ignore_ascii_case(&object.factory_name)
                })
                .and_then(|definition| definition.model_index);
            registry.enemy_managers.retain(|definition| {
                !definition
                    .factory_name
                    .eq_ignore_ascii_case(&object.factory_name)
            });
            registry.enemy_managers.push(EnemyManagerDefinition {
                factory_name: object.factory_name,
                spawned_actor_class: inherited_actor_class(
                    &object.class_name,
                    &manager_actor_classes,
                    &inheritance,
                ),
                class_name: object.class_name,
                model_index,
                models,
            });
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

    fn scan_map_obj_manager(&self, registry: &mut ObjectRegistry) -> Result<()> {
        let path = self.repo_root.join("src/MoveBG/MapObjManager.cpp");
        let text = read_required(&path)?;
        extract_string_factory_returns(&text, "MapObj", SchemaSource::MapObjManager, registry);
        Ok(())
    }

    fn scan_params_and_assets(&self, registry: &mut ObjectRegistry) -> Result<()> {
        let param_re = Regex::new(r"PARAM_INIT\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([^)]+)\)")
            .expect("valid param regex");
        let asset_re =
            Regex::new(r#""(/(?:scene|common|select|game_6|guide|option|subtitle)[^"]+)""#)
                .expect("valid asset regex");

        for entry in WalkDir::new(self.repo_root.join("src"))
            .into_iter()
            .chain(WalkDir::new(self.repo_root.join("include")))
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            let extension = path.extension().and_then(|ext| ext.to_str());
            if !matches!(extension, Some("cpp" | "hpp" | "c" | "h")) {
                continue;
            }

            let text = fs::read_to_string(path)?;
            let source_file = normalize_source_path(&self.repo_root, path);
            let owner_hint = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_string());

            for cap in param_re.captures_iter(&text) {
                registry.params.push(ParamDefinition {
                    owner_hint: owner_hint.clone(),
                    member_name: cap[1].to_string(),
                    default_value: cap[2].trim().to_string(),
                    source_file: source_file.clone(),
                });
            }

            for cap in asset_re.captures_iter(&text) {
                registry.asset_hints.push(AssetHint {
                    path: cap[1].to_string(),
                    source_file: source_file.clone(),
                });
            }
        }

        Ok(())
    }

    fn scan_particle_bindings(&self, registry: &mut ObjectRegistry) -> Result<()> {
        for entry in WalkDir::new(self.repo_root.join("src"))
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("cpp") {
                continue;
            }
            let text = fs::read_to_string(path)?;
            let source_file = normalize_source_path(&self.repo_root, path);
            extract_particle_resources(&text, &source_file, registry);
            extract_calc_particle_bindings(&text, &source_file, registry);
        }
        Ok(())
    }

    fn scan_map_static_models(&self, registry: &mut ObjectRegistry) -> Result<()> {
        let path = self.repo_root.join("src/Map/MapStaticObject.cpp");
        let text = read_required(&path)?;
        let source_file = normalize_source_path(&self.repo_root, &path);
        let map_path = self.repo_root.join("src/Map/Map.cpp");
        let stage_bootstrap_actors =
            extract_stage_bootstrap_map_static_actors(&read_required(&map_path)?);
        registry.map_static_models = extract_map_static_models(&text, &source_file);
        for model in &mut registry.map_static_models {
            model.stage_bootstrap_created = stage_bootstrap_actors
                .iter()
                .any(|actor| actor.eq_ignore_ascii_case(&model.actor_name));
        }
        Ok(())
    }

    fn scan_map_obj_flags(&self, registry: &mut ObjectRegistry) -> Result<()> {
        let source_path = self.repo_root.join("src/MoveBG/MapObjFlag.cpp");
        let source = read_required(&source_path)?;
        let factory_path = self.repo_root.join("src/System/MarNameRefGen_MapObj.cpp");
        let factories = read_required(&factory_path)?;
        let application_path = self.repo_root.join("src/System/Application.cpp");
        let application = read_required(&application_path)?;
        let source_file = normalize_source_path(&self.repo_root, &source_path);
        if let Some(definition) =
            extract_map_obj_flag_definition(&source, &factories, &application, &source_file)
        {
            if !registry.objects.iter().any(|object| {
                object
                    .factory_name
                    .eq_ignore_ascii_case(&definition.factory_name)
            }) {
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
        }
        Ok(())
    }

    fn scan_npc_init_data(&self, registry: &mut ObjectRegistry) -> Result<()> {
        let path = self.repo_root.join("src/NPC/NpcInitData.cpp");
        let text = read_required(&path)?;
        let source_file = normalize_source_path(&self.repo_root, &path);
        registry.npc_actors = extract_npc_actor_definitions(&text, &source_file);
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
        let Some(model_name) = parse_cpp_string(fields[8]) else {
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
        models.push(MapStaticModelDefinition {
            actor_name,
            model_path: format!("{directory}/{model_name}.bmd"),
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
                actor.factory_name.to_ascii_lowercase(),
                binding.material_name.to_ascii_lowercase(),
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

fn factory_without_manager(factory_name: &str) -> String {
    factory_name.replace("Manager", "")
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
            factory_without_manager(&manager.factory_name).eq_ignore_ascii_case(&actor.factory_name)
                || manager_actor_class(manager).is_some_and(|spawned_class| {
                    class_is_or_inherits(&actor.class_name, spawned_class, inheritance)
                })
                || primary_model_manager == Some(manager.factory_name.as_str())
        })
        .map(|manager| {
            let exact_factory = factory_without_manager(&manager.factory_name)
                .eq_ignore_ascii_case(&actor.factory_name);
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
    let factory_return_re = Regex::new(
        r#"strcmp\s*\(\s*name\s*,\s*"([^"]+)"\s*\)\s*==\s*0\s*\)\s*(?:\{[^}]*?)?return\s+(?:[A-Za-z0-9_:]+\s*=\s*)?new\s+([A-Za-z_:][A-Za-z0-9_:]*)"#,
    )
    .expect("valid factory regex");

    for cap in factory_return_re.captures_iter(text) {
        let factory_name = cap[1].to_string();
        let class_name = cap[2].to_string();
        registry.objects.push(ObjectDefinition {
            factory_name,
            class_name,
            category: category.to_string(),
            source: source.clone(),
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        });
    }

    let compare_re = Regex::new(r#"strcmp\s*\(\s*name\s*,\s*"([^"]+)"\s*\)\s*==\s*0"#)
        .expect("valid strcmp regex");
    for cap in compare_re.captures_iter(text) {
        let factory_name = cap[1].to_string();
        if registry
            .objects
            .iter()
            .any(|object| object.factory_name == factory_name)
        {
            continue;
        }

        registry.objects.push(ObjectDefinition {
            factory_name,
            class_name: "Unknown".to_string(),
            category: category.to_string(),
            source: source.clone(),
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        });
    }

    let static_array_re = Regex::new(r#""([A-Za-z0-9_./-]+)""#).expect("valid string regex");
    for cap in static_array_re.captures_iter(text) {
        let factory_name = cap[1].to_string();
        if !looks_like_factory_name(&factory_name)
            || registry
                .objects
                .iter()
                .any(|object| object.factory_name == factory_name)
        {
            continue;
        }

        registry.objects.push(ObjectDefinition {
            factory_name,
            class_name: "Unknown".to_string(),
            category: category.to_string(),
            source: source.clone(),
            display_name: None,
            preview_model: None,
            hidden: false,
            unsafe_to_edit: false,
        });
    }
}

fn looks_like_factory_name(value: &str) -> bool {
    !value.contains('/')
        && !value.contains('.')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        && value.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn dedup_registry(registry: &mut ObjectRegistry) {
    let mut objects = BTreeMap::<String, ObjectDefinition>::new();
    for object in registry.objects.drain(..) {
        objects.entry(object.factory_name.clone()).or_insert(object);
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
        .dedup_by(|a, b| a.factory_name.eq_ignore_ascii_case(&b.factory_name));

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

    registry.enemy_managers.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.class_name.cmp(&b.class_name))
    });
    registry
        .enemy_managers
        .dedup_by(|a, b| a.factory_name.eq_ignore_ascii_case(&b.factory_name));
    registry.enemy_actors.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.class_name.cmp(&b.class_name))
    });
    registry
        .enemy_actors
        .dedup_by(|a, b| a.factory_name.eq_ignore_ascii_case(&b.factory_name));
    registry.enemy_material_colors.sort_by(|a, b| {
        a.factory_name
            .cmp(&b.factory_name)
            .then_with(|| a.material_name.cmp(&b.material_name))
            .then_with(|| a.tev_register.cmp(&b.tev_register))
            .then_with(|| a.source_file.cmp(&b.source_file))
    });
    registry.enemy_material_colors.dedup_by(|a, b| {
        a.factory_name.eq_ignore_ascii_case(&b.factory_name)
            && a.material_name.eq_ignore_ascii_case(&b.material_name)
            && a.tev_register == b.tev_register
    });
}

fn read_required(path: &Path) -> Result<String> {
    if !path.exists() {
        return Err(SchemaError::MissingSource(path.to_path_buf()));
    }
    Ok(fs::read_to_string(path)?)
}

fn normalize_source_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let managers = vec![
            EnemyManagerDefinition {
                factory_name: "HamuKuriManager".to_string(),
                class_name: "THamuKuriManager".to_string(),
                model_index: None,
                spawned_actor_class: Some("THamuKuri".to_string()),
                models: Vec::new(),
            },
            EnemyManagerDefinition {
                factory_name: "HaneHamuKuriManager".to_string(),
                class_name: "THaneHamuKuriManager".to_string(),
                model_index: None,
                spawned_actor_class: Some("THaneHamuKuri".to_string()),
                models: Vec::new(),
            },
        ];

        assert_eq!(
            compatible_enemy_managers(&actor, &managers, &inheritance),
            ["HaneHamuKuriManager", "HamuKuriManager"]
        );
    }

    #[test]
    fn associates_indexed_variants_with_the_manager_class_stem() {
        let manager = EnemyManagerDefinition {
            factory_name: "ButterflyManager".to_string(),
            class_name: "TButterfloidManager".to_string(),
            model_index: None,
            spawned_actor_class: None,
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
        };
        assert_eq!(
            compatible_enemy_managers(&actor, &[manager], &BTreeMap::new()),
            ["EggGenManager"]
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

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].actor_name, "BiancoRiver");
        assert_eq!(models[0].model_path, "/scene/map/map/BiancoRiver.bmd");
        assert_eq!(models[0].load_flags, 0x1021_0000);
        assert_eq!(models[1].model_path, "/common/map/SharedModel.bmd");
        assert_eq!(models[1].load_flags, 0x1122_0000);
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
        assert_eq!(coin.category, "Item");
        assert!(coin.unsafe_to_edit);
    }
}
