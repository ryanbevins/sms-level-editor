use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use sms_formats::{
    mount_scene_archive, read_stage_asset_bytes, BmpFile, J3dFile, J3dRebuildDocument,
    J3dRebuildSectionData, StageAssetKind, YmpDocument, YmpLayer,
};
use sms_scene::{
    generate_floor_depth_map, generate_floor_pollution_model, whole_terrain_region,
    GoopAuthoringDocument, GoopBehavior, GoopLayerAuthoring, GoopLayerOrigin, GoopPlane,
    GoopRenderTriangle, GoopStyleSource, GoopTerrainTriangle, SourceFreeStageArchive,
    StageArchiveEdits, StageResourceDocument, GOOP_AUTHORING_FORMAT_VERSION, GOOP_CELL_SIZE,
    GOOP_DEPTH_WORLD_UNITS_PER_CODE, GOOP_MAX_LAYERS,
};

use crate::camera::CameraProjection;

use super::*;

#[derive(Debug, Clone)]
pub(super) struct RetailGoopTemplate {
    pub(super) stage_id: String,
    pub(super) archive_path: PathBuf,
    pub(super) model_asset_path: PathBuf,
    pub(super) resource_stem: String,
    pub(super) layer_index: usize,
    pub(super) behavior: GoopBehavior,
    pub(super) semantic_type: String,
    pub(super) compatible: bool,
}

const GOOP_TEMPLATE_ANIMATION_EXTENSIONS: [&str; 6] = ["btk", "btp", "bpk", "brk", "bck", "bas"];

fn semantic_goop_type(behavior: GoopBehavior, detail_texture_name: Option<&str>) -> String {
    let detail = detail_texture_name.unwrap_or_default().to_ascii_lowercase();
    match behavior {
        GoopBehavior::Fire => "Fire".to_string(),
        GoopBehavior::Electric => "Electric".to_string(),
        GoopBehavior::Slippery if detail.contains("choco") => "Chocolate".to_string(),
        GoopBehavior::Slippery if detail.contains("pink") => "Pink".to_string(),
        GoopBehavior::Slippery if detail.contains("rico") => "Oil".to_string(),
        _ => behavior.label(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct GoopPixelSpan {
    start: usize,
    before: Vec<u8>,
    after: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum GoopUndoRecord {
    Pixels {
        layer: usize,
        spans: Vec<GoopPixelSpan>,
    },
    Snapshot(Box<GoopSnapshotUndo>),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct GoopSnapshotUndo {
    before: Option<GoopAuthoringDocument>,
    after: Option<GoopAuthoringDocument>,
    before_edits: StageArchiveEdits,
    after_edits: StageArchiveEdits,
}

#[derive(Debug, Clone)]
pub(super) struct GoopStroke {
    layer: usize,
    changed: BTreeMap<usize, (u8, u8)>,
    last_world: Option<[f32; 3]>,
}

pub(super) struct GoopRebuildOutcome {
    base_root: String,
    stage_id: String,
    terrain_fingerprint: u64,
    before: Option<GoopAuthoringDocument>,
    before_edits: StageArchiveEdits,
    document: StageDocument,
}

#[derive(Debug, Clone)]
struct FinalGoopTerrainSnapshot {
    collision_triangles: Vec<GoopTerrainTriangle>,
    render_triangles: Vec<GoopRenderTriangle>,
    fingerprint: u64,
}

pub(super) fn index_retail_goop_templates(
    archives: &[SceneArchiveInfo],
) -> (Vec<RetailGoopTemplate>, Vec<String>) {
    let mut templates = Vec::new();
    let mut warnings = Vec::new();
    for archive in archives.iter().filter(|archive| archive.path.is_file()) {
        let assets = match mount_scene_archive(&archive.path) {
            Ok(assets) => assets,
            Err(error) => {
                warnings.push(format!(
                    "Could not inspect goop templates in '{}': {error}",
                    archive.path.display()
                ));
                continue;
            }
        };
        let Some(ymp_asset) = assets.iter().find(|asset| {
            archive_resource_path(&asset.path)
                .is_some_and(|path| path.eq_ignore_ascii_case("map/ymap.ymp"))
        }) else {
            continue;
        };
        let ymp = match read_stage_asset_bytes(&ymp_asset.path).and_then(YmpDocument::parse) {
            Ok(ymp) => ymp,
            Err(error) => {
                warnings.push(format!(
                    "Could not parse {} ymap.ymp: {error}",
                    archive.stage_id
                ));
                continue;
            }
        };
        for (layer_index, layer) in ymp.layers.iter().enumerate() {
            if GoopPlane::from_runtime_code(layer.flags) != Some(GoopPlane::Floor) {
                continue;
            }
            let expected_stem = source_pollution_stem(layer_index, &archive.stage_id);
            let Some(model_asset) = assets.iter().find(|asset| {
                asset.kind == StageAssetKind::Model
                    && archive_resource_path(&asset.path).is_some_and(|path| {
                        Path::new(&path)
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .is_some_and(|stem| stem.eq_ignore_ascii_case(&expected_stem))
                            && path.to_ascii_lowercase().starts_with("map/pollution/")
                    })
            }) else {
                continue;
            };
            let model = match read_stage_asset_bytes(&model_asset.path)
                .and_then(J3dRebuildDocument::parse)
            {
                Ok(model) => model,
                Err(error) => {
                    warnings.push(format!(
                        "Blocked {} layer {} goop template: {error}",
                        archive.stage_id, layer_index
                    ));
                    continue;
                }
            };
            let has_material = model
                .sections
                .iter()
                .any(|section| matches!(section.data, J3dRebuildSectionData::Materials(_)));
            let first_texture_format = model.sections.iter().find_map(|section| {
                if let J3dRebuildSectionData::Textures(textures) = &section.data {
                    textures.textures.first().map(|texture| texture.format)
                } else {
                    None
                }
            });
            let Some(first_texture_format) = first_texture_format.filter(|_| has_material) else {
                warnings.push(format!(
                    "Blocked {} layer {} goop template because MAT3 or TEX1 texture zero is missing",
                    archive.stage_id, layer_index
                ));
                continue;
            };
            if first_texture_format != 1 {
                warnings.push(format!(
                    "Blocked {} layer {} goop template because texture zero is not mutable I8",
                    archive.stage_id, layer_index
                ));
                continue;
            }
            let preview = match model.to_bytes().and_then(J3dFile::parse).and_then(|file| {
                file.geometry_preview_with_loader_flags(SMS_POLLUTION_MODEL_LOAD_FLAGS)
            }) {
                Ok(preview) => preview,
                Err(error) => {
                    warnings.push(format!(
                        "Blocked {} layer {} goop template because its model preview failed: {error}",
                        archive.stage_id, layer_index
                    ));
                    continue;
                }
            };
            let material_zero_triangles = preview
                .triangles
                .iter()
                .filter(|triangle| triangle.material_index == Some(0))
                .collect::<Vec<_>>();
            let texture_zero_is_bound = material_zero_triangles.iter().any(|triangle| {
                triangle.texture_index == Some(0) || triangle.mask_texture_index == Some(0)
            });
            if !texture_zero_is_bound {
                warnings.push(format!(
                    "Blocked {} layer {} goop template because its material does not bind texture zero",
                    archive.stage_id, layer_index
                ));
                continue;
            }
            let detail_texture_name = material_zero_triangles
                .iter()
                .filter_map(|triangle| triangle.texture_index)
                .find(|index| *index != 0)
                .and_then(|index| preview.textures.get(index))
                .map(|texture| texture.name.as_str());
            let behavior = GoopBehavior::from_runtime_code(layer.layer_type);
            templates.push(RetailGoopTemplate {
                stage_id: archive.stage_id.clone(),
                archive_path: archive.path.clone(),
                model_asset_path: model_asset.path.clone(),
                resource_stem: expected_stem,
                layer_index,
                behavior,
                semantic_type: semantic_goop_type(behavior, detail_texture_name),
                compatible: true,
            });
        }
    }
    templates.sort_by(|left, right| {
        right
            .compatible
            .cmp(&left.compatible)
            .then_with(|| left.stage_id.cmp(&right.stage_id))
            .then_with(|| left.layer_index.cmp(&right.layer_index))
    });
    (templates, warnings)
}

fn archive_resource_path(path: &Path) -> Option<String> {
    path.to_string_lossy()
        .split_once("!/")
        .map(|(_, resource)| resource.replace('\\', "/"))
}

fn source_pollution_stem(index: usize, stage_id: &str) -> String {
    if stage_id.to_ascii_lowercase().starts_with("mare") {
        match index {
            7 => return "pollutionA".to_string(),
            8 => return "pollutionB".to_string(),
            _ => {}
        }
    }
    format!("pollution{index:02}")
}

fn goop_template_choices(
    templates: &[RetailGoopTemplate],
    selected: usize,
    show_retail_sources: bool,
) -> Vec<(usize, RetailGoopTemplate)> {
    if show_retail_sources {
        return templates.iter().cloned().enumerate().collect();
    }

    // The retail archive/file pair is provenance, not a useful authoring
    // concept. Present one safe choice per runtime type while retaining the
    // currently selected source for that type so opening the combo never
    // silently restyles an existing generated layer.
    let mut by_type = BTreeMap::<(u16, String), (usize, RetailGoopTemplate)>::new();
    for (index, template) in templates
        .iter()
        .enumerate()
        .filter(|(_, template)| template.compatible)
    {
        let key = (
            template.behavior.runtime_code(),
            template.semantic_type.clone(),
        );
        let candidate = (index, template.clone());
        match by_type.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) if index == selected => {
                entry.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    by_type.into_values().collect()
}

fn goop_template_label(template: &RetailGoopTemplate, show_retail_source: bool) -> String {
    let semantic_type = &template.semantic_type;
    if show_retail_source {
        let incompatible = if template.compatible {
            ""
        } else {
            " [incompatible]"
        };
        format!(
            "{semantic_type} - {} / {}{incompatible}",
            template.stage_id, template.resource_stem
        )
    } else {
        semantic_type.clone()
    }
}

fn generated_goop_requires_upgrade(authoring: &GoopAuthoringDocument) -> bool {
    authoring.requires_generator_upgrade()
}

fn clear_generated_goop_readiness_if_empty(authoring: &mut GoopAuthoringDocument) {
    if authoring
        .layers
        .iter()
        .any(|layer| layer.origin == GoopLayerOrigin::Generated)
    {
        return;
    }
    authoring.stale = false;
    authoring.terrain_fingerprint = 0;
    authoring.format_version = GOOP_AUTHORING_FORMAT_VERSION;
}

impl SmsEditorApp {
    pub(super) fn ensure_goop_templates_indexed(&mut self) {
        if self.goop_templates_indexed {
            return;
        }
        let (templates, warnings) = index_retail_goop_templates(&self.scene_archives);
        self.retail_goop_templates = templates;
        self.goop_templates_indexed = true;
        self.log.extend(warnings);
        if self.selected_goop_template >= self.retail_goop_templates.len() {
            self.selected_goop_template = 0;
        }
    }

    pub(super) fn goop_inspector_panel(&mut self, ui: &mut egui::Ui) {
        self.ensure_goop_templates_indexed();
        let generator_upgrade_available =
            self.background_receiver.is_none() && self.goop_stroke.is_none();
        let Some(document) = self.document.as_mut() else {
            ui.label("Open a stage to author goop.");
            return;
        };
        let generator_upgrade_pending = match document.ensure_goop_authoring() {
            Ok(authoring) => {
                let pending = generated_goop_requires_upgrade(authoring);
                if pending && generator_upgrade_available {
                    // Claim the migration before scheduling it so a failed
                    // background rebuild falls back to the visible stale-layer
                    // action instead of retrying every frame.
                    authoring.format_version = GOOP_AUTHORING_FORMAT_VERSION;
                }
                pending
            }
            Err(error) => {
                ui.colored_label(egui::Color32::RED, error.to_string());
                return;
            }
        };
        if generator_upgrade_pending && generator_upgrade_available {
            self.rebuild_generated_goop_layers();
        }
        if self.background_label.as_deref() == Some("Rebuilding goopmaps") {
            ui.heading("Goop");
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Rebuilding generated goop for this editor version...");
            });
            ui.small(
                "The painted mask is preserved while the mesh and depth resources are repaired.",
            );
            return;
        }
        let document = self
            .document
            .as_mut()
            .expect("stage document remains open while drawing goop inspector");
        ui.heading("Goop");
        ui.small("Floor layers are editable. Retail wall and wave layers remain read-only.");
        ui.separator();

        let layers = document
            .goop_authoring
            .as_ref()
            .map(|goop| goop.layers.clone())
            .unwrap_or_default();
        for (index, layer) in layers.iter().enumerate() {
            let mut label = format!("{:02}  {}", layer.runtime_index, layer.behavior.label());
            if !layer.editable() {
                label.push_str("  (read-only)");
            }
            if ui
                .selectable_label(self.selected_goop_layer == index, label)
                .clicked()
            {
                self.selected_goop_layer = index;
            }
        }
        self.selected_goop_layer = self.selected_goop_layer.min(layers.len().saturating_sub(1));

        let selected_generated_layer = layers
            .get(self.selected_goop_layer)
            .filter(|layer| layer.origin == GoopLayerOrigin::Generated);
        if let Some(source) = selected_generated_layer.and_then(|layer| layer.style_source.as_ref())
        {
            if let Some((index, _)) =
                self.retail_goop_templates
                    .iter()
                    .enumerate()
                    .find(|(_, template)| {
                        template.stage_id == source.stage_id
                            && template.layer_index == source.layer_index
                    })
            {
                self.selected_goop_template = index;
            }
        }

        ui.separator();
        ui.checkbox(
            &mut self.show_incompatible_goop_templates,
            "Show retail source variants (expert)",
        );
        let selected_behavior = selected_generated_layer.map(|layer| layer.behavior);
        let visible_templates = goop_template_choices(
            &self.retail_goop_templates,
            self.selected_goop_template,
            self.show_incompatible_goop_templates,
        );
        let selected_text = self
            .retail_goop_templates
            .get(self.selected_goop_template)
            .map_or("No compatible goop type".to_string(), |template| {
                goop_template_label(template, self.show_incompatible_goop_templates)
            });
        let previous_template = self.selected_goop_template;
        egui::ComboBox::from_label(if selected_generated_layer.is_some() {
            "Goop type"
        } else {
            "New layer type"
        })
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for (index, template) in visible_templates {
                ui.selectable_value(
                    &mut self.selected_goop_template,
                    index,
                    goop_template_label(&template, self.show_incompatible_goop_templates),
                );
            }
        });
        if self.selected_goop_template != previous_template && selected_generated_layer.is_some() {
            self.set_selected_goop_style(self.selected_goop_template);
            return;
        }
        if self
            .retail_goop_templates
            .get(self.selected_goop_template)
            .is_some_and(|template| {
                !template.compatible
                    || selected_behavior.is_some_and(|behavior| template.behavior != behavior)
            })
        {
            ui.colored_label(
                egui::Color32::from_rgb(245, 180, 70),
                "Expert override: this template is structurally or behavior-incompatible.",
            );
        }
        ui.checkbox(&mut self.goop_use_custom_region, "Use manual region");
        if self.goop_use_custom_region {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.goop_region_min_x)
                        .speed(GOOP_CELL_SIZE)
                        .prefix("Min X "),
                );
                ui.add(
                    egui::DragValue::new(&mut self.goop_region_min_z)
                        .speed(GOOP_CELL_SIZE)
                        .prefix("Min Z "),
                );
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.goop_region_width_cells)
                        .range(8..=1024)
                        .prefix("Cells X "),
                );
                ui.add(
                    egui::DragValue::new(&mut self.goop_region_height_cells)
                        .range(4..=1024)
                        .prefix("Cells Z "),
                );
            });
            ui.small("Origins must align to 40 units; cell dimensions must be powers of two.");
        }
        if ui
            .add_enabled(
                layers.len() < GOOP_MAX_LAYERS
                    && self
                        .retail_goop_templates
                        .get(self.selected_goop_template)
                        .is_some(),
                egui::Button::new("Add generated floor layer"),
            )
            .clicked()
        {
            self.add_generated_goop_layer();
            return;
        }

        ui.separator();
        ui.label("Brush");
        ui.add(egui::Slider::new(&mut self.goop_brush_radius, 20.0..=2000.0).text("Radius"));
        ui.add(egui::Slider::new(&mut self.goop_brush_hardness, 0.0..=1.0).text("Hardness"));
        ui.add(egui::Slider::new(&mut self.goop_brush_opacity, 0.01..=1.0).text("Opacity"));
        ui.checkbox(&mut self.goop_fill_mode, "Connected fill");
        ui.small("Left-drag paints. Shift + left-drag erases.");

        let selected = self.document.as_ref().and_then(|document| {
            document
                .goop_authoring
                .as_ref()
                .and_then(|goop| goop.layers.get(self.selected_goop_layer))
                .cloned()
        });
        if let Some(layer) = selected {
            ui.separator();
            if layer.editable() {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 220, 140),
                    "Brush ready: hold left mouse over a valid floor cell.",
                );
            } else {
                ui.colored_label(
                    egui::Color32::RED,
                    "This layer has no editable pollution BMP mask.",
                );
            }
            let mut visible = layer.visible;
            if ui
                .checkbox(&mut visible, "Visible in authoring overlay")
                .changed()
            {
                self.set_selected_goop_visibility(visible);
            }
            let mut behavior = layer.behavior;
            egui::ComboBox::from_label("Runtime behavior")
                .selected_text(behavior.label())
                .show_ui(ui, |ui| {
                    for preset in [
                        GoopBehavior::Normal,
                        GoopBehavior::Fire,
                        GoopBehavior::Slippery,
                        GoopBehavior::Barrier,
                        GoopBehavior::Electric,
                    ] {
                        ui.selectable_value(&mut behavior, preset, preset.label());
                    }
                    if matches!(layer.behavior, GoopBehavior::Retail(_)) {
                        ui.selectable_value(&mut behavior, layer.behavior, layer.behavior.label());
                    }
                });
            if behavior != layer.behavior {
                self.set_selected_goop_behavior(behavior);
            }
            let (width, height) = layer.dimensions().unwrap_or((0, 0));
            let coverage = layer
                .mask()
                .map(|mask| mask.iter().filter(|value| **value != 0).count())
                .unwrap_or(0);
            let valid = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .filter(|(x, y)| layer.valid_cell(*x, *y))
                .count();
            ui.label(format!("Resolution: {width} x {height}"));
            ui.label(format!("Painted: {coverage} / {valid} valid cells"));
            ui.label(format!(
                "Region: X {:.0}..{:.0}, Z {:.0}..{:.0}",
                layer.region.min_x, layer.region.max_x, layer.region.min_z, layer.region.max_z
            ));
            ui.label(format!("Plane: {:?}", layer.plane));
            if let Some(style) = &layer.style_source {
                if self.show_incompatible_goop_templates {
                    ui.small(format!("Retail source: {}", style.display_name));
                }
                if style.forced_incompatible || style.behavior_code != layer.behavior.runtime_code()
                {
                    ui.colored_label(
                        egui::Color32::from_rgb(245, 180, 70),
                        "Persistent warning: behavior/style compatibility was overridden.",
                    );
                }
            }
            if layer.origin == GoopLayerOrigin::Generated
                && self.selected_goop_layer + 1 == layers.len()
                && ui.button("Delete generated layer").clicked()
            {
                self.delete_selected_goop_layer();
                return;
            }
        }
        if self
            .document
            .as_ref()
            .and_then(|document| document.goop_authoring.as_ref())
            .is_some_and(|goop| goop.stale)
        {
            ui.colored_label(
                egui::Color32::RED,
                "Generated goop resources need rebuilding because the terrain or generator changed.",
            );
            if ui.button("Rebuild generated layers").clicked() {
                self.rebuild_generated_goop_layers();
            }
        }
    }

    pub(super) fn add_generated_goop_layer(&mut self) {
        let Some(template) = self
            .retail_goop_templates
            .get(self.selected_goop_template)
            .cloned()
        else {
            self.log
                .push("No usable retail goop template is selected.".to_string());
            return;
        };
        let terrain = match self.final_goop_terrain_snapshot() {
            Ok(terrain) => terrain,
            Err(error) => {
                self.log.push(format!(
                    "Could not snapshot final terrain for goop generation: {error}"
                ));
                return;
            }
        };
        let custom_region = (
            self.goop_use_custom_region,
            self.goop_region_min_x,
            self.goop_region_min_z,
            self.goop_region_width_cells,
            self.goop_region_height_cells,
        );
        let Some(document) = &mut self.document else {
            return;
        };
        let before = document.goop_authoring.clone();
        let before_edits = document.archive_edits.clone();
        let result = (|| -> Result<(), String> {
            let target_stage_id = document.stage_id.clone();
            let (use_custom, min_x, min_z, width_cells, height_cells) = custom_region;
            let (region, width_log2, height_log2) = if use_custom {
                if (min_x / GOOP_CELL_SIZE).fract().abs() > 0.0001
                    || (min_z / GOOP_CELL_SIZE).fract().abs() > 0.0001
                {
                    return Err(
                        "Manual goop region origins must align to 40-unit cells".to_string()
                    );
                }
                if !width_cells.is_power_of_two()
                    || !height_cells.is_power_of_two()
                    || width_cells < 8
                    || height_cells < 4
                {
                    return Err(
                        "Manual goop region dimensions must be power-of-two cells (minimum 8x4)"
                            .to_string(),
                    );
                }
                let region = sms_scene::GoopRegion {
                    min_x,
                    min_z,
                    max_x: min_x + f32::from(width_cells) * GOOP_CELL_SIZE,
                    max_z: min_z + f32::from(height_cells) * GOOP_CELL_SIZE,
                };
                (
                    region,
                    width_cells.trailing_zeros() as u16,
                    height_cells.trailing_zeros() as u16,
                )
            } else {
                whole_terrain_region(&terrain.collision_triangles)
                    .map_err(|error| error.to_string())?
            };
            let (vertical_offset, depth_map) = generate_floor_depth_map(
                &terrain.collision_triangles,
                region,
                width_log2,
                height_log2,
            )
            .map_err(|error| error.to_string())?;
            let width = 1u16 << width_log2;
            let height = 1u16 << height_log2;
            let runtime = YmpLayer {
                layer_type: template.behavior.runtime_code(),
                subtype: 0,
                flags: GoopPlane::Floor.runtime_code(),
                reserved: 0,
                vertical_offset,
                vertical_scale: GOOP_CELL_SIZE,
                min_x: region.min_x,
                min_z: region.min_z,
                max_x: region.max_x,
                max_z: region.max_z,
                width_log2,
                height_log2,
                user_value: 0,
                map_offset: 0,
                depth_map,
            };
            let template_model = read_stage_asset_bytes(&template.model_asset_path)
                .and_then(J3dRebuildDocument::parse)
                .map_err(|error| error.to_string())?;
            let generated_model = generate_floor_pollution_model(
                &template_model,
                &terrain.render_triangles,
                &runtime,
                !template.compatible,
            )
            .map_err(|error| error.to_string())?;
            let active_material_name = pollution_material_name(&generated_model)?;
            let authoring = document
                .ensure_goop_authoring()
                .map_err(|error| error.to_string())?;
            if authoring.layers.len() >= GOOP_MAX_LAYERS {
                return Err(format!(
                    "Sunshine supports at most {GOOP_MAX_LAYERS} goop layers"
                ));
            }
            if authoring
                .layers
                .iter()
                .any(|layer| layer.plane == GoopPlane::Floor && layer.region.overlaps(region))
            {
                return Err(
                    "The default whole-terrain region overlaps an existing floor layer. Edit or subdivide regions before adding another layer."
                        .to_string(),
                );
            }
            let runtime_index = authoring.layers.len();
            let target_stem = source_pollution_stem(runtime_index, &target_stage_id);
            authoring.layers.push(GoopLayerAuthoring {
                id: format!("generated-goop-{runtime_index:02}"),
                runtime_index,
                origin: GoopLayerOrigin::Generated,
                plane: GoopPlane::Floor,
                behavior: template.behavior,
                visible: true,
                region,
                runtime,
                bitmap: Some(
                    BmpFile::new_pollution_mask(
                        width,
                        height,
                        vec![0; usize::from(width) * usize::from(height)],
                    )
                    .map_err(|error| error.to_string())?,
                ),
                generated_model: Some(generated_model),
                style_source: Some(GoopStyleSource {
                    stage_id: template.stage_id.clone(),
                    layer_index: template.layer_index,
                    display_name: format!("{} / {}", template.stage_id, template.resource_stem),
                    behavior_code: template.behavior.runtime_code(),
                    forced_incompatible: !template.compatible,
                }),
                resource_stem: target_stem.clone(),
                metadata_dirty: true,
            });
            authoring.terrain_fingerprint = terrain.fingerprint;
            authoring.stale = false;
            document
                .compile_goop_authoring()
                .map_err(|error| error.to_string())?;
            copy_template_animations(document, &template, &target_stem, &active_material_name)?;
            sync_runtime_actor_goop_textures(document, &template)?;
            Ok(())
        })();
        if let Err(error) = result {
            document.goop_authoring = before;
            document.archive_edits = before_edits;
            self.log
                .push(format!("Could not generate goop layer: {error}"));
            return;
        }
        let record = GoopUndoRecord::Snapshot(Box::new(GoopSnapshotUndo {
            before,
            after: document.goop_authoring.clone(),
            before_edits,
            after_edits: document.archive_edits.clone(),
        }));
        self.selected_goop_layer = document
            .goop_authoring
            .as_ref()
            .map_or(0, |goop| goop.layers.len().saturating_sub(1));
        self.push_goop_undo(record);
        self.finish_goop_document_change("Generated playable floor goop layer");
    }

    fn finalized_goop_terrain_document(&self) -> Result<StageDocument, String> {
        let mut document = self
            .document
            .clone()
            .ok_or_else(|| "no stage is open".to_string())?;
        let instances = self
            .model_instances
            .iter()
            .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&document.stage_id))
            .cloned()
            .collect::<Vec<_>>();
        let assets = if instances.is_empty() {
            BTreeMap::new()
        } else {
            let content_root = self
                .model_content_root()
                .ok_or_else(|| "project Content root is unavailable".to_string())?;
            Self::load_model_asset_snapshot(&content_root, &instances)?
        };
        let not_cancelled = AtomicBool::new(false);
        document.archive_edits = Self::stage_edits_with_model_instances_from_snapshot_cancellable(
            &assets,
            &instances,
            &document.archive_edits,
            document.stage_archive.as_ref(),
            document.registry.as_ref(),
            &not_cancelled,
        )?;
        Ok(document)
    }

    fn final_goop_terrain_snapshot(&self) -> Result<FinalGoopTerrainSnapshot, String> {
        let document = self.finalized_goop_terrain_document()?;
        let fingerprint = document
            .effective_terrain_fingerprint()
            .map_err(|error| error.to_string())?;

        let collision = match document
            .effective_resource_clone(b"map/map.col")
            .map_err(|error| error.to_string())?
        {
            Some(resource) => Some(resource),
            None => document
                .effective_resource_clone(b"map/map/map.col")
                .map_err(|error| error.to_string())?,
        };
        let collision = match collision {
            Some(StageResourceDocument::Collision(collision)) => collision,
            Some(_) => {
                return Err("effective map collision resource is not COL data".to_string());
            }
            None => {
                return Err(
                    "final terrain has no map/map.col or map/map/map.col resource".to_string(),
                );
            }
        };
        let collision_triangles = goop_collision_triangles(&collision);
        if collision_triangles.is_empty() {
            return Err("final map collision contains no triangles".to_string());
        }

        let model = match document
            .effective_resource_clone(b"map/map/map.bmd")
            .map_err(|error| error.to_string())?
        {
            Some(StageResourceDocument::Model(model)) => model,
            Some(_) => return Err("effective map/map/map.bmd is not model data".to_string()),
            None => return Err("final terrain has no map/map/map.bmd resource".to_string()),
        };
        let model_bytes = model.to_bytes().map_err(|error| error.to_string())?;
        let preview = J3dFile::parse(&model_bytes)
            .and_then(|model| model.geometry_preview_with_loader_flags(SMS_MAP_MODEL_LOAD_FLAGS))
            .map_err(|error| format!("could not decode final map render geometry: {error}"))?;
        let render_triangles = goop_upward_render_triangles(&preview);
        if render_triangles.is_empty() {
            return Err("final map model contains no upward-facing render triangles".to_string());
        }

        Ok(FinalGoopTerrainSnapshot {
            collision_triangles,
            render_triangles,
            fingerprint,
        })
    }

    pub(super) fn final_goop_terrain_fingerprint(&self) -> Result<u64, String> {
        let document = self.finalized_goop_terrain_document()?;
        document
            .effective_terrain_fingerprint()
            .map_err(|error| error.to_string())
    }

    pub(super) fn refresh_goop_stale_from_final_terrain(&mut self) {
        let should_check = self.document.as_ref().is_some_and(|document| {
            document.goop_authoring.as_ref().is_some_and(|goop| {
                goop.layers
                    .iter()
                    .any(|layer| layer.origin == GoopLayerOrigin::Generated)
                    && goop.terrain_fingerprint != 0
            })
        });
        if !should_check {
            return;
        }
        let Ok(fingerprint) = self.final_goop_terrain_fingerprint() else {
            return;
        };
        if let Some(goop) = self
            .document
            .as_mut()
            .and_then(|document| document.goop_authoring.as_mut())
        {
            goop.stale |= goop.terrain_fingerprint != fingerprint;
        }
    }

    pub(super) fn handle_goop_viewport_input(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
    ) -> bool {
        if self.tool != EditorTool::Goop || ui.input(|input| input.modifiers.alt) {
            return false;
        }
        if self.background_label.as_deref() == Some("Rebuilding goopmaps") {
            self.goop_cursor_world = None;
            return false;
        }
        let pointer = ui.input(|input| input.pointer.interact_pos());
        self.goop_cursor_world = pointer.and_then(|position| self.goop_surface_hit(rect, position));
        if !response.hovered() && self.goop_stroke.is_none() {
            return false;
        }
        let down = ui.input(|input| input.pointer.primary_down());
        let released = ui.input(|input| input.pointer.primary_released());
        let had_stroke = self.goop_stroke.is_some();
        if down && self.goop_stroke.is_none() && self.goop_cursor_world.is_some() {
            if self
                .document
                .as_mut()
                .and_then(|document| document.ensure_goop_authoring().ok())
                .is_some_and(|goop| goop.layers.is_empty())
            {
                self.ensure_goop_templates_indexed();
                self.add_generated_goop_layer();
            }
            let editable = self.document.as_ref().is_some_and(|document| {
                document
                    .goop_authoring
                    .as_ref()
                    .and_then(|goop| goop.layers.get(self.selected_goop_layer))
                    .is_some_and(GoopLayerAuthoring::editable)
            });
            if editable {
                self.goop_stroke = Some(GoopStroke {
                    layer: self.selected_goop_layer,
                    changed: BTreeMap::new(),
                    last_world: None,
                });
            }
        }
        if down {
            if let Some(world) = self.goop_cursor_world {
                let erase = ui.input(|input| input.modifiers.shift);
                if self.paint_goop_toward(world, erase) {
                    self.refresh_live_goop_preview();
                }
                ui.ctx().request_repaint();
            }
        }
        if released && self.goop_stroke.is_some() {
            self.commit_goop_stroke();
        }
        self.goop_stroke.is_some() || had_stroke || down
    }

    fn paint_goop_toward(&mut self, world: [f32; 3], erase: bool) -> bool {
        let Some(stroke) = self.goop_stroke.as_ref() else {
            return false;
        };
        let layer_index = stroke.layer;
        let cell_size = self
            .document
            .as_ref()
            .and_then(|document| document.goop_authoring.as_ref())
            .and_then(|goop| goop.layers.get(layer_index))
            .map_or(GOOP_CELL_SIZE, |layer| layer.runtime.vertical_scale);
        let samples = stroke.last_world.map_or_else(
            || vec![world],
            |last| {
                let dx = world[0] - last[0];
                let dz = world[2] - last[2];
                let distance = dx.hypot(dz);
                let spacing = (self.goop_brush_radius * 0.25).max(cell_size * 0.25);
                let count = (distance / spacing).ceil().max(1.0) as usize;
                (1..=count)
                    .map(|step| {
                        let t = step as f32 / count as f32;
                        [last[0] + dx * t, world[1], last[2] + dz * t]
                    })
                    .collect()
            },
        );
        let mut all_changes = Vec::new();
        let Some(document) = &mut self.document else {
            return false;
        };
        let Some(layer) = document
            .goop_authoring
            .as_mut()
            .and_then(|goop| goop.layers.get_mut(layer_index))
        else {
            return false;
        };
        let Ok(mut mask) = layer.mask() else {
            return false;
        };
        if self.goop_fill_mode {
            if let Some((x, y)) = layer.world_to_cell(world[0], world[2]) {
                flood_fill(layer, &mut mask, [x, y], erase, &mut all_changes);
            }
        } else {
            for sample in samples {
                paint_brush_sample(
                    layer,
                    &mut mask,
                    sample,
                    GoopBrush {
                        radius: self.goop_brush_radius,
                        hardness: self.goop_brush_hardness,
                        opacity: self.goop_brush_opacity,
                        erase,
                    },
                    &mut all_changes,
                );
            }
        }
        if layer.set_mask(&mask).is_err() {
            return false;
        }
        let changed = !all_changes.is_empty();
        if let Some(stroke) = &mut self.goop_stroke {
            for (index, before, after) in all_changes {
                stroke
                    .changed
                    .entry(index)
                    .and_modify(|change| change.1 = after)
                    .or_insert((before, after));
            }
            stroke.last_world = Some(world);
        }
        changed
    }

    fn refresh_live_goop_preview(&mut self) {
        self.refresh_live_goop_preview_for_layer(self.selected_goop_layer);
    }

    fn refresh_live_goop_preview_for_layer(&mut self, layer_index: usize) {
        let Some((width, height, mask)) = self.document.as_ref().and_then(|document| {
            let layer = document.goop_authoring.as_ref()?.layers.get(layer_index)?;
            let (width, height) = layer.dimensions().ok()?;
            Some((width, height, layer.mask().ok()?))
        }) else {
            return;
        };
        let Some(texture_indices) = self
            .model_preview
            .as_ref()
            .and_then(|preview| preview.pollution_texture_indices.get(&layer_index))
            .cloned()
        else {
            return;
        };
        let mut rgba = Vec::with_capacity(mask.len() * 4);
        for value in mask {
            rgba.extend_from_slice(&[value, value, value, value]);
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
        let Some(preview) = self.model_preview.as_mut() else {
            return;
        };
        for index in texture_indices.iter().copied() {
            let Some(texture) = preview.textures.get_mut(index) else {
                continue;
            };
            if texture.image.size != [width, height] {
                continue;
            }
            texture.image = image.clone();
            texture.mips.clear();
            texture.mips.push(image.clone());
            texture.mipmap_enabled = false;
            texture.mipmap_count = 1;
            texture.has_alpha = true;
            texture.has_translucent_alpha = rgba
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 12 && pixel[3] < 245);
        }
        if let Some(gpu_viewport) = &self.gpu_viewport {
            gpu_viewport.update_textures(preview, &texture_indices);
        }
        self.clear_viewport_preview_cache();
    }

    fn commit_goop_stroke(&mut self) {
        let Some(stroke) = self.goop_stroke.take() else {
            return;
        };
        if stroke.changed.is_empty() {
            return;
        }
        let spans = coalesce_pixel_changes(stroke.changed);
        if let Some(document) = &mut self.document {
            if let Err(error) = document.compile_goop_layer_mask(stroke.layer) {
                self.log
                    .push(format!("Could not compile painted goop mask: {error}"));
                return;
            }
        }
        self.push_goop_undo(GoopUndoRecord::Pixels {
            layer: stroke.layer,
            spans,
        });
        self.finish_goop_pixel_change("Painted goop mask");
    }

    fn push_goop_undo(&mut self, record: GoopUndoRecord) {
        self.goop_undo_stack.push_back(record);
        if self.goop_undo_stack.len() > 80 {
            self.goop_undo_stack.pop_front();
        }
        self.goop_redo_stack.clear();
    }

    fn set_selected_goop_visibility(&mut self, visible: bool) {
        let Some(document) = &mut self.document else {
            return;
        };
        let before = document.goop_authoring.clone();
        let before_edits = document.archive_edits.clone();
        let Some(layer) = document
            .goop_authoring
            .as_mut()
            .and_then(|goop| goop.layers.get_mut(self.selected_goop_layer))
        else {
            return;
        };
        layer.visible = visible;
        let record = GoopUndoRecord::Snapshot(Box::new(GoopSnapshotUndo {
            before,
            after: document.goop_authoring.clone(),
            before_edits: before_edits.clone(),
            after_edits: document.archive_edits.clone(),
        }));
        self.push_goop_undo(record);
        self.finish_goop_document_change("Changed goop layer visibility");
    }

    fn set_selected_goop_behavior(&mut self, behavior: GoopBehavior) {
        let Some(document) = &mut self.document else {
            return;
        };
        let before = document.goop_authoring.clone();
        let before_edits = document.archive_edits.clone();
        let Some(layer) = document
            .goop_authoring
            .as_mut()
            .and_then(|goop| goop.layers.get_mut(self.selected_goop_layer))
        else {
            return;
        };
        layer.behavior = behavior;
        layer.runtime.layer_type = behavior.runtime_code();
        layer.metadata_dirty = true;
        if let Err(error) = document.compile_goop_authoring() {
            document.goop_authoring = before;
            document.archive_edits = before_edits;
            self.log
                .push(format!("Could not change goop behavior: {error}"));
            return;
        }
        let record = GoopUndoRecord::Snapshot(Box::new(GoopSnapshotUndo {
            before,
            after: document.goop_authoring.clone(),
            before_edits,
            after_edits: document.archive_edits.clone(),
        }));
        self.push_goop_undo(record);
        self.finish_goop_document_change("Changed goop runtime behavior");
    }

    fn set_selected_goop_style(&mut self, template_index: usize) {
        let Some(template) = self.retail_goop_templates.get(template_index).cloned() else {
            return;
        };
        let terrain = match self.final_goop_terrain_snapshot() {
            Ok(terrain) => terrain,
            Err(error) => {
                self.log.push(format!(
                    "Could not snapshot final terrain for the goop style change: {error}"
                ));
                return;
            }
        };
        let template_model = match read_stage_asset_bytes(&template.model_asset_path)
            .and_then(J3dRebuildDocument::parse)
        {
            Ok(model) => model,
            Err(error) => {
                self.log.push(format!(
                    "Could not read the selected retail goop style: {error}"
                ));
                return;
            }
        };
        let Some((runtime, target_stem)) = self
            .document
            .as_ref()
            .and_then(|document| document.goop_authoring.as_ref())
            .and_then(|goop| goop.layers.get(self.selected_goop_layer))
            .filter(|layer| layer.origin == GoopLayerOrigin::Generated)
            .map(|layer| (layer.runtime.clone(), layer.resource_stem.clone()))
        else {
            return;
        };
        let generated_model = match generate_floor_pollution_model(
            &template_model,
            &terrain.render_triangles,
            &runtime,
            !template.compatible,
        ) {
            Ok(model) => model,
            Err(error) => {
                self.log
                    .push(format!("Could not apply the selected goop style: {error}"));
                return;
            }
        };
        let active_material_name = match pollution_material_name(&generated_model) {
            Ok(name) => name,
            Err(error) => {
                self.log
                    .push(format!("Could not apply the selected goop style: {error}"));
                return;
            }
        };

        let Some(document) = &mut self.document else {
            return;
        };
        let before = document.goop_authoring.clone();
        let before_edits = document.archive_edits.clone();
        let result = (|| -> Result<(), String> {
            let layer = document
                .goop_authoring
                .as_mut()
                .and_then(|goop| goop.layers.get_mut(self.selected_goop_layer))
                .ok_or_else(|| "the selected goop layer no longer exists".to_string())?;
            layer.generated_model = Some(generated_model);
            layer.behavior = template.behavior;
            layer.runtime.layer_type = template.behavior.runtime_code();
            layer.style_source = Some(GoopStyleSource {
                stage_id: template.stage_id.clone(),
                layer_index: template.layer_index,
                display_name: format!("{} / {}", template.stage_id, template.resource_stem),
                behavior_code: template.behavior.runtime_code(),
                forced_incompatible: !template.compatible,
            });
            layer.metadata_dirty = true;
            for extension in GOOP_TEMPLATE_ANIMATION_EXTENSIONS {
                document.archive_edits.remove_resource(
                    format!("map/pollution/{target_stem}.{extension}").into_bytes(),
                );
            }
            document
                .compile_goop_authoring()
                .map_err(|error| error.to_string())?;
            copy_template_animations(document, &template, &target_stem, &active_material_name)?;
            sync_runtime_actor_goop_textures(document, &template)?;
            Ok(())
        })();
        if let Err(error) = result {
            document.goop_authoring = before;
            document.archive_edits = before_edits;
            self.log
                .push(format!("Could not change the goop retail style: {error}"));
            return;
        }
        let record = GoopUndoRecord::Snapshot(Box::new(GoopSnapshotUndo {
            before,
            after: document.goop_authoring.clone(),
            before_edits,
            after_edits: document.archive_edits.clone(),
        }));
        self.push_goop_undo(record);
        self.finish_goop_document_change("Changed goop retail style");
    }

    pub(super) fn undo_goop(&mut self) -> bool {
        let Some(record) = self.goop_undo_stack.pop_back() else {
            return false;
        };
        let pixel_layer = match &record {
            GoopUndoRecord::Pixels { layer, .. } => Some(*layer),
            GoopUndoRecord::Snapshot(_) => None,
        };
        self.apply_goop_undo_record(&record, false);
        self.goop_redo_stack.push_back(record);
        if let Some(layer) = pixel_layer {
            self.refresh_live_goop_preview_for_layer(layer);
            self.finish_goop_pixel_change("Undo goop edit");
        } else {
            self.finish_goop_document_change("Undo goop edit");
        }
        true
    }

    pub(super) fn redo_goop(&mut self) -> bool {
        let Some(record) = self.goop_redo_stack.pop_back() else {
            return false;
        };
        let pixel_layer = match &record {
            GoopUndoRecord::Pixels { layer, .. } => Some(*layer),
            GoopUndoRecord::Snapshot(_) => None,
        };
        self.apply_goop_undo_record(&record, true);
        self.goop_undo_stack.push_back(record);
        if let Some(layer) = pixel_layer {
            self.refresh_live_goop_preview_for_layer(layer);
            self.finish_goop_pixel_change("Redo goop edit");
        } else {
            self.finish_goop_document_change("Redo goop edit");
        }
        true
    }

    fn apply_goop_undo_record(&mut self, record: &GoopUndoRecord, forward: bool) {
        let Some(document) = &mut self.document else {
            return;
        };
        match record {
            GoopUndoRecord::Pixels { layer, spans } => {
                let Some(target) = document
                    .goop_authoring
                    .as_mut()
                    .and_then(|goop| goop.layers.get_mut(*layer))
                else {
                    return;
                };
                let Ok(mut mask) = target.mask() else { return };
                for span in spans {
                    let values = if forward { &span.after } else { &span.before };
                    let end = span.start + values.len();
                    if let Some(destination) = mask.get_mut(span.start..end) {
                        destination.copy_from_slice(values);
                    }
                }
                let _ = target.set_mask(&mask);
                let _ = document.compile_goop_layer_mask(*layer);
            }
            GoopUndoRecord::Snapshot(snapshot) => {
                document.goop_authoring = if forward {
                    snapshot.after.clone()
                } else {
                    snapshot.before.clone()
                };
                document.archive_edits = if forward {
                    snapshot.after_edits.clone()
                } else {
                    snapshot.before_edits.clone()
                };
            }
        }
    }

    fn finish_goop_document_change(&mut self, label: &str) {
        self.document_dirty = true;
        self.flush_document_change();
        self.rebuild_model_preview_from_document();
        self.log.push(format!("{label}."));
    }

    fn finish_goop_pixel_change(&mut self, label: &str) {
        self.document_dirty = true;
        // Painting changes only already-validated mask bytes. The live texture
        // upload is authoritative until save, so do not serialize the complete
        // overlay, revalidate every stage resource, or rebuild the full scene
        // preview at the end of each stroke.
        self.log.push(format!("{label}."));
    }

    fn delete_selected_goop_layer(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        let before = document.goop_authoring.clone();
        let before_edits = document.archive_edits.clone();
        let Some(authoring) = &mut document.goop_authoring else {
            return;
        };
        if self.selected_goop_layer + 1 != authoring.layers.len()
            || authoring.layers[self.selected_goop_layer].origin != GoopLayerOrigin::Generated
        {
            self.log
                .push("Only the last generated layer can be deleted safely.".to_string());
            return;
        }
        let layer = authoring
            .layers
            .pop()
            .expect("selected generated layer exists");
        clear_generated_goop_readiness_if_empty(authoring);
        let mut export_authoring = authoring.clone();
        export_authoring.stale = false;
        let compiled_ymp = match document.effective_resource_clone(sms_scene::GOOP_RESOURCE_PATH) {
            Ok(Some(StageResourceDocument::PollutionMap(base))) => {
                export_authoring.compiled_ymp_preserving(&base)
            }
            Ok(_) => export_authoring.compiled_ymp(),
            Err(error) => Err(error),
        };
        let compiled_ymp = match compiled_ymp {
            Ok(ymp) => ymp,
            Err(error) => {
                document.goop_authoring = before;
                document.archive_edits = before_edits;
                self.log
                    .push(format!("Could not delete goop layer: {error}"));
                return;
            }
        };
        let imported_ymp_exists = document
            .stage_archive
            .as_ref()
            .is_some_and(|archive| archive.resource(sms_scene::GOOP_RESOURCE_PATH).is_some());
        if export_authoring.layers.is_empty() && !imported_ymp_exists {
            document
                .archive_edits
                .remove_resource(sms_scene::GOOP_RESOURCE_PATH.to_vec());
        } else {
            document.upsert_authored_resource(
                sms_scene::GOOP_RESOURCE_PATH.to_vec(),
                StageResourceDocument::PollutionMap(compiled_ymp),
            );
        }
        for extension in ["bmp", "bmd", "btk", "btp", "bpk", "brk", "bck", "bas"] {
            document.archive_edits.remove_resource(
                format!("map/pollution/{}.{extension}", layer.resource_stem).into_bytes(),
            );
        }
        if let Err(error) = document.compile_goop_authoring() {
            document.goop_authoring = before;
            document.archive_edits = before_edits;
            self.log
                .push(format!("Could not delete goop layer: {error}"));
            return;
        }
        let record = GoopUndoRecord::Snapshot(Box::new(GoopSnapshotUndo {
            before,
            after: document.goop_authoring.clone(),
            before_edits,
            after_edits: document.archive_edits.clone(),
        }));
        self.push_goop_undo(record);
        self.selected_goop_layer = self.selected_goop_layer.saturating_sub(1);
        self.finish_goop_document_change("Deleted generated goop layer");
    }

    fn rebuild_generated_goop_layers(&mut self) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let terrain = match self.final_goop_terrain_snapshot() {
            Ok(terrain) => terrain,
            Err(error) => {
                self.log.push(format!(
                    "Could not snapshot final terrain for goop rebuild: {error}"
                ));
                return;
            }
        };
        let templates = self.retail_goop_templates.clone();
        let Some(document) = self.document.clone() else {
            return;
        };
        let before = document.goop_authoring.clone();
        let before_edits = document.archive_edits.clone();
        let base_root = self.base_root.trim().to_string();
        let stage_id = document.stage_id.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut document = document;
            let result = rebuild_goop_document(
                &mut document,
                &terrain.collision_triangles,
                &terrain.render_triangles,
                &templates,
                terrain.fingerprint,
                &task_cancel,
            )
            .map(|()| {
                Box::new(GoopRebuildOutcome {
                    base_root,
                    stage_id,
                    terrain_fingerprint: terrain.fingerprint,
                    before,
                    before_edits,
                    document,
                })
            });
            let _ = sender.send(BackgroundResult::GoopRebuild(result));
        });
        self.background_receiver = Some(receiver);
        self.active_build_cancel = Some(cancel);
        self.background_label = Some("Rebuilding goopmaps".to_string());
        self.log
            .push("Rebuilding stale goop layers from the current terrain snapshot...".to_string());
    }

    pub(super) fn apply_goop_rebuild(&mut self, outcome: GoopRebuildOutcome) {
        if self.base_root.trim() != outcome.base_root || self.stage_id != outcome.stage_id {
            self.log
                .push("Discarded rebuilt goopmaps because the open stage changed.".to_string());
            return;
        }
        let current_fingerprint = match self.final_goop_terrain_fingerprint() {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                self.log.push(format!(
                    "Discarded rebuilt goopmaps because the current terrain could not be verified: {error}"
                ));
                return;
            }
        };
        if current_fingerprint != outcome.terrain_fingerprint {
            self.log.push(
                "Discarded rebuilt goopmaps because the terrain or a map-terrain instance changed during the rebuild."
                    .to_string(),
            );
            return;
        }
        let Some(current) = &self.document else {
            return;
        };
        if current.goop_authoring != outcome.before || current.archive_edits != outcome.before_edits
        {
            self.log.push(
                "Discarded rebuilt goopmaps because the stage was edited during the rebuild."
                    .to_string(),
            );
            return;
        }
        let record = GoopUndoRecord::Snapshot(Box::new(GoopSnapshotUndo {
            before: outcome.before,
            after: outcome.document.goop_authoring.clone(),
            before_edits: outcome.before_edits,
            after_edits: outcome.document.archive_edits.clone(),
        }));
        self.document = Some(outcome.document);
        self.push_goop_undo(record);
        self.finish_goop_document_change("Rebuilt stale goop layers");
    }

    fn goop_surface_hit(&self, rect: egui::Rect, position: egui::Pos2) -> Option<[f32; 3]> {
        if !rect.contains(position) {
            return None;
        }
        let frame = self.camera_frame();
        let ray = goop_viewport_ray_direction(
            frame,
            rect,
            self.viewport_pan,
            self.viewport_zoom,
            position,
        );
        let preview = self.model_preview.as_ref()?;
        let active_layer = self
            .document
            .as_ref()
            .and_then(|document| document.goop_authoring.as_ref())
            .and_then(|goop| goop.layers.get(self.selected_goop_layer));

        // Only the effective map terrain may anchor the brush. Scene objects,
        // authored props, and pollution meshes can all be nearer to the
        // camera, but their render triangles do not define the floor goopmap.
        let render_hit = nearest_goop_render_surface_hit(
            active_layer,
            frame.position,
            ray,
            &preview.goop_surface_model_indices,
            &preview.triangles,
        );
        if !preview.goop_surface_model_indices.is_empty() {
            return render_hit;
        }

        // Collision-only stages predate the model provenance metadata. Keep
        // them paintable as a compatibility fallback, while rejecting walls.
        render_hit.or_else(|| {
            nearest_goop_surface_hit(
                active_layer,
                frame.position,
                ray,
                preview
                    .collision_triangles
                    .iter()
                    .filter(|triangle| triangle_has_floor_projection(triangle.vertices))
                    .map(|triangle| triangle.vertices),
            )
        })
    }

    pub(super) fn paint_goop_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.tool != EditorTool::Goop {
            return;
        }
        let Some(document) = &self.document else {
            return;
        };
        let Some(authoring) = &document.goop_authoring else {
            return;
        };
        let Some(layer) = authoring.layers.get(self.selected_goop_layer) else {
            return;
        };
        if !layer.visible {
            return;
        }
        let projection = self.camera_projection(rect);
        if let Ok((width, height)) = layer.dimensions() {
            let cell_size = layer.runtime.vertical_scale;
            let boundary_stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 220, 150));
            for (cell_y, world_z) in [
                (0, layer.region.min_z),
                (height.saturating_sub(1), layer.region.max_z),
            ] {
                let points = (0..width).map(|cell_x| {
                    goop_cell_cleanable_surface_y(layer, cell_x, cell_y).map(|world_y| {
                        [
                            layer.region.min_x + (cell_x as f32 + 0.5) * cell_size,
                            world_y + 8.0,
                            world_z,
                        ]
                    })
                });
                paint_surface_polyline(painter, projection, points, boundary_stroke);
            }
            for (cell_x, world_x) in [
                (0, layer.region.min_x),
                (width.saturating_sub(1), layer.region.max_x),
            ] {
                let points = (0..height).map(|cell_y| {
                    goop_cell_cleanable_surface_y(layer, cell_x, cell_y).map(|world_y| {
                        [
                            world_x,
                            world_y + 8.0,
                            layer.region.min_z + (cell_y as f32 + 0.5) * cell_size,
                        ]
                    })
                });
                paint_surface_polyline(painter, projection, points, boundary_stroke);
            }

            let step = (((width * height) as f32 / 2048.0).sqrt().ceil() as usize).max(1);
            for cell_y in (0..height).step_by(step) {
                for cell_x in (0..width).step_by(step) {
                    if layer.valid_cell(cell_x, cell_y) {
                        continue;
                    }
                    let Some(world_y) = goop_invalid_marker_surface_y(layer, cell_x, cell_y) else {
                        continue;
                    };
                    let world = [
                        layer.region.min_x + (cell_x as f32 + 0.5) * cell_size,
                        world_y + 8.0,
                        layer.region.min_z + (cell_y as f32 + 0.5) * cell_size,
                    ];
                    if let Some((screen, _)) = projection.project_world_to_screen(world) {
                        painter.circle_filled(
                            screen,
                            1.5,
                            egui::Color32::from_rgba_unmultiplied(255, 70, 70, 170),
                        );
                    }
                }
            }
        }
        if let Some(hit) = self.goop_cursor_world {
            if let Some((center, _)) = self.project_world_to_screen(rect, hit) {
                let color = if layer
                    .world_to_cell(hit[0], hit[2])
                    .is_some_and(|(x, y)| layer.valid_cell(x, y))
                {
                    egui::Color32::from_rgb(255, 224, 100)
                } else {
                    egui::Color32::from_rgb(255, 90, 90)
                };
                let outline = goop_brush_outline(layer, hit, self.goop_brush_radius);
                paint_surface_polyline(
                    painter,
                    projection,
                    outline.into_iter().map(Some),
                    egui::Stroke::new(2.0, color),
                );
                painter.circle_filled(center, 2.5, color);
            }
        }
        if authoring.stale {
            painter.text(
                rect.center_top() + egui::vec2(0.0, 18.0),
                egui::Align2::CENTER_TOP,
                "GOOPMAP STALE — rebuild before release",
                egui::FontId::proportional(18.0),
                egui::Color32::RED,
            );
        }
    }
}

fn goop_viewport_ray_direction(
    frame: CameraFrame,
    rect: egui::Rect,
    viewport_pan: egui::Vec2,
    viewport_zoom: f32,
    position: egui::Pos2,
) -> [f32; 3] {
    let focal = perspective_focal_length(rect, viewport_zoom).max(1.0);
    let local = position - rect.center() - viewport_pan;
    vec3_normalize(vec3_add(
        frame.forward,
        vec3_add(
            vec3_scale(frame.right, local.x / focal),
            vec3_scale(frame.up, -local.y / focal),
        ),
    ))
}

fn nearest_goop_surface_hit(
    layer: Option<&GoopLayerAuthoring>,
    origin: [f32; 3],
    direction: [f32; 3],
    triangles: impl IntoIterator<Item = [[f32; 3]; 3]>,
) -> Option<[f32; 3]> {
    triangles
        .into_iter()
        .filter_map(|vertices| {
            let distance = ray_triangle_distance(origin, direction, vertices)?;
            let hit = vec3_add(origin, vec3_scale(direction, distance));
            layer
                .is_none_or(|layer| goop_hit_matches_active_floor(layer, hit))
                .then_some((distance, hit))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, hit)| hit)
}

fn nearest_goop_render_surface_hit<'a>(
    layer: Option<&GoopLayerAuthoring>,
    origin: [f32; 3],
    direction: [f32; 3],
    surface_model_indices: &BTreeSet<usize>,
    triangles: impl IntoIterator<Item = &'a PreviewTriangle>,
) -> Option<[f32; 3]> {
    nearest_goop_surface_hit(
        layer,
        origin,
        direction,
        triangles.into_iter().filter_map(|triangle| {
            (triangle.render_layer == PreviewRenderLayer::Main
                && surface_model_indices.contains(&triangle.model_index)
                && render_triangle_is_upward_facing(triangle.vertices, triangle.normals))
            .then_some(triangle.vertices)
        }),
    )
}

fn goop_hit_matches_active_floor(layer: &GoopLayerAuthoring, hit: [f32; 3]) -> bool {
    let Some((x, y)) = layer.world_to_cell(hit[0], hit[2]) else {
        // Keep the footprint usable just outside the region so a brush can
        // still overlap and paint valid edge cells.
        return true;
    };
    let Ok(depth) = layer.runtime.depth_at(x, y) else {
        return true;
    };
    if depth == 0xff {
        // Invalid cells are deliberately targetable: the red footprint tells
        // the user why the center will not paint, while valid neighboring
        // cells inside the brush radius can still be affected.
        return true;
    }
    // TPollutionPos::isSame performs this exact comparison: worldToDepth uses
    // the hard-coded 0.025 conversion with C++ truncation toward zero, then
    // accepts the floor layer's inclusive +/-2 encoded-depth window.
    let encoded = ((hit[1] - layer.runtime.vertical_offset) * 0.025).trunc();
    encoded.is_finite() && encoded >= f32::from(depth) - 2.0 && encoded <= f32::from(depth) + 2.0
}

fn triangle_has_floor_projection(vertices: [[f32; 3]; 3]) -> bool {
    const MIN_VERTICAL_NORMAL_COMPONENT: f32 = 0.1;

    let edge_a = vec3_sub(vertices[1], vertices[0]);
    let edge_b = vec3_sub(vertices[2], vertices[0]);
    let normal = vec3_cross(edge_a, edge_b);
    let length = vec3_dot(normal, normal).sqrt();
    length.is_finite()
        && length > f32::EPSILON
        && normal[1].abs() / length > MIN_VERTICAL_NORMAL_COMPONENT
}

fn goop_brush_outline(layer: &GoopLayerAuthoring, center: [f32; 3], radius: f32) -> Vec<[f32; 3]> {
    const SEGMENTS: usize = 64;

    (0..=SEGMENTS)
        .map(|step| {
            let angle = step as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let x = center[0] + angle.cos() * radius;
            let z = center[2] + angle.sin() * radius;
            let surface_y = layer
                .world_to_cell(x, z)
                .and_then(|(cell_x, cell_y)| goop_cell_cleanable_surface_y(layer, cell_x, cell_y))
                .unwrap_or(center[1]);
            [x, surface_y + 8.0, z]
        })
        .collect()
}

/// Reconstructs the world Y which Sunshine accepts for painting and cleaning.
/// `TPollutionPos::worldToDepth` and `isSame` always encode at 0.025, even for
/// imported layers whose `mVerticalScale` is not 40. `getDepthWorld` separately
/// multiplies by `mVerticalScale` for effect placement; the authoring overlay
/// deliberately visualizes the cleanability plane instead.
fn goop_cell_cleanable_surface_y(layer: &GoopLayerAuthoring, x: usize, y: usize) -> Option<f32> {
    let depth = layer.runtime.depth_at(x, y).ok()?;
    (depth != 0xff).then_some(
        layer.runtime.vertical_offset + f32::from(depth) * GOOP_DEPTH_WORLD_UNITS_PER_CODE,
    )
}

fn goop_invalid_marker_surface_y(layer: &GoopLayerAuthoring, x: usize, y: usize) -> Option<f32> {
    let (width, height) = layer.dimensions().ok()?;
    let min_x = x.saturating_sub(1);
    let max_x = (x + 1).min(width.saturating_sub(1));
    let min_y = y.saturating_sub(1);
    let max_y = (y + 1).min(height.saturating_sub(1));
    (min_y..=max_y)
        .flat_map(|near_y| (min_x..=max_x).map(move |near_x| (near_x, near_y)))
        .filter_map(|(near_x, near_y)| goop_cell_cleanable_surface_y(layer, near_x, near_y))
        .max_by(f32::total_cmp)
}

fn paint_surface_polyline(
    painter: &egui::Painter,
    projection: CameraProjection,
    points: impl IntoIterator<Item = Option<[f32; 3]>>,
    stroke: egui::Stroke,
) {
    let mut previous = None;
    for point in points {
        let projected = point.and_then(|point| projection.project_world_to_screen(point));
        if let (Some((from, _)), Some((to, _))) = (previous, projected) {
            painter.line_segment([from, to], stroke);
        }
        previous = projected;
    }
}

fn goop_collision_triangles(collision: &ColFile) -> Vec<GoopTerrainTriangle> {
    collision
        .groups()
        .iter()
        .flat_map(|group| {
            group.triangles.iter().filter_map(|triangle| {
                let [a, b, c] = triangle.vertex_indices;
                let vertices = [a, b, c].map(|index| {
                    collision
                        .vertices()
                        .get(usize::from(index))
                        .map(|vertex| vertex.position)
                });
                let [Some(a), Some(b), Some(c)] = vertices else {
                    return None;
                };
                let vertices = [a, b, c];
                collision_triangle_is_runtime_ground(vertices, group.surface_type)
                    .then_some(GoopTerrainTriangle { vertices })
            })
        })
        .collect()
}

fn collision_triangle_is_runtime_ground(vertices: [[f32; 3]; 3], surface_type: u16) -> bool {
    // TBGCheckData::getPlaneType forces 0x0801 into the ground list. Every
    // other collision triangle is ground only when its normalized Y normal is
    // greater than 0.2. TPollutionObj::getDepthFromMap calls checkGround, so
    // roofs and walls must not participate in generated YMP sampling.
    if surface_type == 0x0801 {
        return true;
    }
    let first = vec3_sub(vertices[1], vertices[0]);
    let second = vec3_sub(vertices[2], vertices[1]);
    let normal = vec3_cross(first, second);
    let length = vec3_dot(normal, normal).sqrt();
    length.is_finite() && length > f32::EPSILON && normal[1] / length > 0.2
}

fn goop_upward_render_triangles(preview: &J3dGeometryPreview) -> Vec<GoopRenderTriangle> {
    preview
        .triangles
        .iter()
        .filter(|triangle| render_triangle_is_upward_facing(triangle.vertices, triangle.normals))
        .map(|triangle| GoopRenderTriangle {
            vertices: triangle.vertices,
            normals: triangle.normals,
        })
        .collect()
}

fn render_triangle_is_upward_facing(
    vertices: [[f32; 3]; 3],
    normals: Option<[[f32; 3]; 3]>,
) -> bool {
    const MIN_UPWARD_COMPONENT: f32 = 0.1;

    if let Some(normals) = normals {
        let average = normals.iter().fold([0.0; 3], |mut average, normal| {
            for axis in 0..3 {
                average[axis] += normal[axis];
            }
            average
        });
        let length = vec3_dot(average, average).sqrt();
        if length.is_finite() && length > f32::EPSILON {
            return average[1] / length > MIN_UPWARD_COMPONENT;
        }
    }

    // GX runtime triangles use clockwise winding. In Sunshine coordinates an
    // upward-facing X/Z triangle therefore has a negative geometric Y normal.
    let edge_a = vec3_sub(vertices[1], vertices[0]);
    let edge_b = vec3_sub(vertices[2], vertices[0]);
    let geometric = vec3_cross(edge_a, edge_b);
    let length = vec3_dot(geometric, geometric).sqrt();
    length.is_finite() && length > f32::EPSILON && -geometric[1] / length > MIN_UPWARD_COMPONENT
}

fn rebuild_goop_document(
    document: &mut StageDocument,
    collision_triangles: &[GoopTerrainTriangle],
    render_triangles: &[GoopRenderTriangle],
    templates: &[RetailGoopTemplate],
    terrain_fingerprint: u64,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let mut animation_jobs = Vec::new();
    {
        let Some(authoring) = &mut document.goop_authoring else {
            return Ok(());
        };
        for layer in authoring
            .layers
            .iter_mut()
            .filter(|layer| layer.origin == GoopLayerOrigin::Generated)
        {
            if cancelled.load(Ordering::Acquire) {
                return Err("goop rebuild cancelled".to_string());
            }
            let old_mask = layer.mask().map_err(|error| error.to_string())?;
            let (width, height) = layer.dimensions().map_err(|error| error.to_string())?;
            let (offset, depth) = generate_floor_depth_map(
                collision_triangles,
                layer.region,
                layer.runtime.width_log2,
                layer.runtime.height_log2,
            )
            .map_err(|error| error.to_string())?;
            layer.runtime.vertical_offset = offset;
            layer.runtime.depth_map = depth;
            let mut reprojected = old_mask;
            for y in 0..height {
                for x in 0..width {
                    if !layer.valid_cell(x, y) {
                        reprojected[y * width + x] = 0;
                    }
                }
            }
            layer
                .set_mask(&reprojected)
                .map_err(|error| error.to_string())?;
            let source = layer
                .style_source
                .as_ref()
                .ok_or_else(|| format!("layer {} has no retail style provenance", layer.id))?;
            let template = templates
                .iter()
                .find(|template| {
                    template.stage_id == source.stage_id
                        && template.layer_index == source.layer_index
                })
                .ok_or_else(|| format!("retail template {} is unavailable", source.display_name))?;
            let model = read_stage_asset_bytes(&template.model_asset_path)
                .and_then(J3dRebuildDocument::parse)
                .map_err(|error| error.to_string())?;
            let generated_model = generate_floor_pollution_model(
                &model,
                render_triangles,
                &layer.runtime,
                source.forced_incompatible,
            )
            .map_err(|error| error.to_string())?;
            let active_material_name = pollution_material_name(&generated_model)?;
            layer.generated_model = Some(generated_model);
            animation_jobs.push((
                template.clone(),
                layer.resource_stem.clone(),
                active_material_name,
            ));
        }
        authoring.terrain_fingerprint = terrain_fingerprint;
        authoring.format_version = GOOP_AUTHORING_FORMAT_VERSION;
        authoring.stale = false;
    }
    document
        .compile_goop_authoring()
        .map_err(|error| error.to_string())?;
    for (template, target_stem, active_material_name) in animation_jobs {
        copy_template_animations(document, &template, &target_stem, &active_material_name)?;
        sync_runtime_actor_goop_textures(document, &template)?;
    }
    Ok(())
}

fn pollution_material_name(model: &J3dRebuildDocument) -> Result<String, String> {
    model
        .sections
        .iter()
        .find_map(|section| match &section.data {
            J3dRebuildSectionData::Materials(materials) => materials
                .names
                .as_ref()
                .and_then(|names| names.entries.first())
                .map(|entry| entry.name.clone()),
            _ => None,
        })
        .ok_or_else(|| "generated goop model has no active material name".to_string())
}

fn copy_template_animations(
    document: &mut StageDocument,
    template: &RetailGoopTemplate,
    target_stem: &str,
    active_material_name: &str,
) -> Result<(), String> {
    for extension in GOOP_TEMPLATE_ANIMATION_EXTENSIONS {
        document
            .archive_edits
            .remove_resource(format!("map/pollution/{target_stem}.{extension}").into_bytes());
    }
    let bytes = fs::read(&template.archive_path).map_err(|error| error.to_string())?;
    let archive = SourceFreeStageArchive::parse(&bytes).map_err(|error| error.to_string())?;
    for resource in archive.resources() {
        let path = String::from_utf8_lossy(&resource.raw_path).replace('\\', "/");
        let candidate = Path::new(&path);
        let extension = candidate
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let stem_matches = candidate
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(&template.resource_stem));
        if stem_matches && GOOP_TEMPLATE_ANIMATION_EXTENSIONS.contains(&extension.as_str()) {
            let mut animation = resource.document.clone();
            if let StageResourceDocument::Animation(value) = &mut animation {
                if value
                    .retain_material_bindings_named(active_material_name)
                    .map_err(|error| error.to_string())?
                    .is_some_and(|retained| retained == 0)
                {
                    continue;
                }
            }
            document.upsert_authored_resource(
                format!("map/pollution/{target_stem}.{extension}").into_bytes(),
                animation,
            );
        }
    }
    Ok(())
}

/// Copies decomp-declared, stage-global actor textures from the retail scene
/// that supplies this document's primary goop style. The declarations decide
/// which resources are needed; no actor or model filename is special-cased.
pub(super) fn sync_runtime_actor_goop_textures(
    document: &mut StageDocument,
    template: &RetailGoopTemplate,
) -> Result<usize, String> {
    let Some(primary_style) = document
        .goop_authoring
        .as_ref()
        .and_then(|authoring| {
            authoring
                .layers
                .iter()
                .filter_map(|layer| layer.style_source.as_ref())
                .next()
        })
        .cloned()
    else {
        return Ok(0);
    };
    if primary_style.stage_id != template.stage_id
        || primary_style.layer_index != template.layer_index
    {
        return Ok(0);
    }
    sync_runtime_actor_goop_textures_from_source(document, &primary_style, &template.archive_path)
}

pub(super) fn sync_runtime_actor_goop_textures_from_source(
    document: &mut StageDocument,
    primary_style: &GoopStyleSource,
    source_archive_path: &Path,
) -> Result<usize, String> {
    let Some(registry) = document.registry.as_ref() else {
        return Ok(0);
    };
    let factories = document
        .objects
        .iter()
        .map(|object| object.factory_name.as_str())
        .collect::<BTreeSet<_>>();
    let resource_paths = factories
        .into_iter()
        .flat_map(|factory| registry.runtime_texture_replacements_for(factory))
        .map(|replacement| replacement.resource_path.clone())
        .collect::<BTreeSet<_>>();
    if resource_paths.is_empty() {
        return Ok(0);
    }

    let bytes = fs::read(source_archive_path).map_err(|error| error.to_string())?;
    let archive = SourceFreeStageArchive::parse(&bytes).map_err(|error| error.to_string())?;
    let mut writes = 0;
    for resource_path in resource_paths {
        let archive_path = runtime_texture_archive_path(&resource_path);
        let source = archive
            .resources()
            .iter()
            .find(|resource| {
                String::from_utf8_lossy(&resource.raw_path)
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(&archive_path)
            })
            .ok_or_else(|| {
                format!(
                    "retail goop style {} does not provide declared runtime texture {resource_path}",
                    primary_style.display_name
                )
            })?;
        if !matches!(source.document, StageResourceDocument::Texture(_)) {
            return Err(format!(
                "retail goop style {} provides {resource_path}, but it is not a BTI texture",
                primary_style.display_name
            ));
        }
        let raw_path = archive_path.into_bytes();
        let aliases = document
            .stage_archive
            .iter()
            .flat_map(|archive| archive.resources())
            .map(|resource| resource.raw_path.clone())
            .chain(
                document
                    .archive_edits
                    .resources
                    .iter()
                    .map(|edit| edit.raw_resource_path.clone()),
            )
            .filter(|candidate| {
                candidate != &raw_path
                    && String::from_utf8_lossy(candidate)
                        .replace('\\', "/")
                        .eq_ignore_ascii_case(&String::from_utf8_lossy(&raw_path))
            })
            .collect::<BTreeSet<_>>();
        for alias in aliases {
            document.archive_edits.remove_resource(alias);
            writes += 1;
        }
        if document
            .effective_resource_clone(&raw_path)
            .map_err(|error| error.to_string())?
            .as_ref()
            == Some(&source.document)
        {
            continue;
        }
        document.upsert_authored_resource(raw_path, source.document.clone());
        writes += 1;
    }
    Ok(writes)
}

fn runtime_texture_archive_path(resource_path: &str) -> String {
    let normalized = resource_path.trim_start_matches('/').replace('\\', "/");
    normalized
        .strip_prefix("scene/")
        .unwrap_or(&normalized)
        .to_string()
}

#[derive(Debug, Clone, Copy)]
struct GoopBrush {
    radius: f32,
    hardness: f32,
    opacity: f32,
    erase: bool,
}

fn paint_brush_sample(
    layer: &GoopLayerAuthoring,
    mask: &mut [u8],
    sample: [f32; 3],
    brush: GoopBrush,
    changes: &mut Vec<(usize, u8, u8)>,
) {
    let Ok((width, height)) = layer.dimensions() else {
        return;
    };
    let GoopBrush {
        radius,
        hardness,
        opacity,
        erase,
    } = brush;
    let cell_size = layer.runtime.vertical_scale;
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return;
    }
    let min_x = (((sample[0] - radius - layer.region.min_x) / cell_size).floor() as isize)
        .clamp(0, width.saturating_sub(1) as isize) as usize;
    let max_x = (((sample[0] + radius - layer.region.min_x) / cell_size).ceil() as isize)
        .clamp(0, width.saturating_sub(1) as isize) as usize;
    let min_y = (((sample[2] - radius - layer.region.min_z) / cell_size).floor() as isize)
        .clamp(0, height.saturating_sub(1) as isize) as usize;
    let max_y = (((sample[2] + radius - layer.region.min_z) / cell_size).ceil() as isize)
        .clamp(0, height.saturating_sub(1) as isize) as usize;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if !layer.valid_cell(x, y) {
                continue;
            }
            let world_x = layer.region.min_x + (x as f32 + 0.5) * cell_size;
            let world_z = layer.region.min_z + (y as f32 + 0.5) * cell_size;
            let normalized = (world_x - sample[0]).hypot(world_z - sample[2]) / radius.max(1.0);
            if normalized > 1.0 {
                continue;
            }
            let feather = if normalized <= hardness {
                1.0
            } else {
                1.0 - (normalized - hardness) / (1.0 - hardness).max(0.0001)
            };
            let strength = (feather * opacity).clamp(0.0, 1.0);
            let index = y * width + x;
            let before = mask[index];
            let after = if erase {
                (f32::from(before) * (1.0 - strength)).round() as u8
            } else {
                (f32::from(before) + (255.0 - f32::from(before)) * strength).round() as u8
            };
            if before != after {
                mask[index] = after;
                changes.push((index, before, after));
            }
        }
    }
}

fn flood_fill(
    layer: &GoopLayerAuthoring,
    mask: &mut [u8],
    start: [usize; 2],
    erase: bool,
    changes: &mut Vec<(usize, u8, u8)>,
) {
    let Ok((width, height)) = layer.dimensions() else {
        return;
    };
    let [start_x, start_y] = start;
    if !layer.valid_cell(start_x, start_y) {
        return;
    }
    let mut pending = VecDeque::from([(start_x, start_y)]);
    let mut visited = vec![false; width * height];
    let value = if erase { 0 } else { 255 };
    while let Some((x, y)) = pending.pop_front() {
        let index = y * width + x;
        if visited[index] || !layer.valid_cell(x, y) {
            continue;
        }
        visited[index] = true;
        let before = mask[index];
        if before != value {
            mask[index] = value;
            changes.push((index, before, value));
        }
        if x > 0 {
            pending.push_back((x - 1, y));
        }
        if x + 1 < width {
            pending.push_back((x + 1, y));
        }
        if y > 0 {
            pending.push_back((x, y - 1));
        }
        if y + 1 < height {
            pending.push_back((x, y + 1));
        }
    }
}

fn coalesce_pixel_changes(changes: BTreeMap<usize, (u8, u8)>) -> Vec<GoopPixelSpan> {
    let mut spans: Vec<GoopPixelSpan> = Vec::new();
    for (index, (before, after)) in changes {
        if let Some(span) = spans.last_mut() {
            if span.start + span.before.len() == index {
                span.before.push(before);
                span.after.push(after);
                continue;
            }
        }
        spans.push(GoopPixelSpan {
            start: index,
            before: vec![before],
            after: vec![after],
        });
    }
    spans
}

fn ray_triangle_distance(
    origin: [f32; 3],
    direction: [f32; 3],
    vertices: [[f32; 3]; 3],
) -> Option<f32> {
    let edge1 = vec3_sub(vertices[1], vertices[0]);
    let edge2 = vec3_sub(vertices[2], vertices[0]);
    let p = vec3_cross(direction, edge2);
    let determinant = vec3_dot(edge1, p);
    if determinant.abs() < 0.000001 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let t = vec3_sub(origin, vertices[0]);
    let u = vec3_dot(t, p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = vec3_cross(t, edge1);
    let v = vec3_dot(direction, q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = vec3_dot(edge2, q) * inverse;
    (distance > 0.0).then_some(distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editable_layer() -> GoopLayerAuthoring {
        GoopLayerAuthoring {
            id: "test".to_string(),
            runtime_index: 0,
            origin: GoopLayerOrigin::Generated,
            plane: GoopPlane::Floor,
            behavior: GoopBehavior::Normal,
            visible: true,
            region: sms_scene::GoopRegion {
                min_x: 0.0,
                min_z: 0.0,
                max_x: 320.0,
                max_z: 160.0,
            },
            runtime: YmpLayer {
                layer_type: 0,
                subtype: 0,
                flags: 0,
                reserved: 0,
                vertical_offset: 0.0,
                vertical_scale: 40.0,
                min_x: 0.0,
                min_z: 0.0,
                max_x: 320.0,
                max_z: 160.0,
                width_log2: 3,
                height_log2: 2,
                user_value: 0,
                map_offset: 0,
                depth_map: vec![0; 32],
            },
            bitmap: Some(BmpFile::new_pollution_mask(8, 4, vec![0; 32]).unwrap()),
            generated_model: None,
            style_source: None,
            resource_stem: "pollution00".to_string(),
            metadata_dirty: false,
        }
    }

    fn retail_template(
        stage_id: &str,
        layer_index: usize,
        behavior: GoopBehavior,
        semantic_type: &str,
    ) -> RetailGoopTemplate {
        RetailGoopTemplate {
            stage_id: stage_id.to_string(),
            archive_path: PathBuf::from(format!("{stage_id}.szs")),
            model_asset_path: PathBuf::from(format!("pollution{layer_index:02}.bmd")),
            resource_stem: format!("pollution{layer_index:02}"),
            layer_index,
            behavior,
            semantic_type: semantic_type.to_string(),
            compatible: true,
        }
    }

    fn synthetic_runtime_texture(fill: u8) -> sms_formats::BtiFile {
        sms_formats::BtiFile {
            allocation_size: 0xa0,
            format: 0,
            transparency: 1,
            width: 16,
            height: 16,
            wrap_s: 1,
            wrap_t: 1,
            palette_enabled: 0,
            palette_format: 0,
            palette_entries: Vec::new(),
            palette_offset: 0,
            mipmap_enabled: 0,
            edge_lod: 0,
            bias_clamp: 0,
            max_anisotropy: 0,
            min_filter: 1,
            mag_filter: 1,
            min_lod: 0,
            max_lod: 0,
            mipmap_count: 1,
            reserved_19: 0,
            lod_bias: 0,
            image_offset: 0x20,
            encoded_mip_levels: vec![vec![fill; 128]],
        }
    }

    fn write_runtime_texture_template(
        directory: &Path,
        stage_id: &str,
        fill: u8,
    ) -> RetailGoopTemplate {
        let archive_path = directory.join(format!("{stage_id}.szs"));
        let mut archive = SourceFreeStageArchive::new_for_blank(stage_id, 1).unwrap();
        archive
            .insert_resource(
                b"map/pollution/H_ma_rak.bti".to_vec(),
                StageResourceDocument::Texture(synthetic_runtime_texture(fill)),
            )
            .unwrap();
        fs::write(&archive_path, archive.encode().unwrap()).unwrap();
        RetailGoopTemplate {
            archive_path,
            ..retail_template(stage_id, 0, GoopBehavior::Normal, "Normal")
        }
    }

    #[test]
    fn primary_goop_style_supplies_declared_actor_texture_without_model_tables() {
        let temp = tempfile::tempdir().unwrap();
        let primary = write_runtime_texture_template(temp.path(), "monte0", 0x44);
        let secondary = write_runtime_texture_template(temp.path(), "bianco0", 0x99);
        let mut layer = editable_layer();
        layer.style_source = Some(GoopStyleSource {
            stage_id: primary.stage_id.clone(),
            layer_index: primary.layer_index,
            display_name: "Monte / pollution00".to_string(),
            behavior_code: 0,
            forced_incompatible: false,
        });
        let registry = ObjectRegistry {
            runtime_texture_replacements: vec![sms_schema::RuntimeTextureReplacementDefinition {
                factory_name: "Mario".to_string(),
                dummy_texture_name: "H_ma_rak_dummy".to_string(),
                resource_path: "/scene/map/pollution/H_ma_rak.bti".to_string(),
                source_file: "src/Player/MarioDraw.cpp".to_string(),
            }],
            ..ObjectRegistry::default()
        };
        let mut document = StageDocument {
            stage_id: "custom0".to_string(),
            base_root: temp.path().to_path_buf(),
            assets: Vec::new(),
            objects: vec![SceneObject::new("mario", "Mario")],
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: StageArchiveEdits::default(),
            registry: Some(registry),
            route_authoring: None,
            goop_authoring: Some(GoopAuthoringDocument {
                layers: vec![layer],
                ..Default::default()
            }),
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };
        document.insert_authored_resource(
            b"map/pollution/h_ma_rak.bti".to_vec(),
            StageResourceDocument::Texture(synthetic_runtime_texture(0x11)),
        );

        assert_eq!(
            sync_runtime_actor_goop_textures(&mut document, &primary).unwrap(),
            2
        );
        let remaining_paths = document
            .archive_edits
            .resources
            .iter()
            .filter(|edit| {
                String::from_utf8_lossy(&edit.raw_resource_path)
                    .eq_ignore_ascii_case("map/pollution/H_ma_rak.bti")
            })
            .map(|edit| edit.raw_resource_path.clone())
            .collect::<Vec<_>>();
        assert_eq!(remaining_paths, [b"map/pollution/H_ma_rak.bti".to_vec()]);
        let Some(StageResourceDocument::Texture(texture)) = document
            .effective_resource_clone(b"map/pollution/H_ma_rak.bti")
            .unwrap()
        else {
            panic!("primary style did not install the declared runtime texture");
        };
        assert_eq!(texture.encoded_mip_levels[0], vec![0x44; 128]);

        assert_eq!(
            sync_runtime_actor_goop_textures(&mut document, &secondary).unwrap(),
            0
        );
        let Some(StageResourceDocument::Texture(texture)) = document
            .effective_resource_clone(b"map/pollution/H_ma_rak.bti")
            .unwrap()
        else {
            panic!("secondary style removed the primary runtime texture");
        };
        assert_eq!(texture.encoded_mip_levels[0], vec![0x44; 128]);
    }

    #[test]
    fn normal_goop_type_choices_hide_retail_provenance_and_keep_selected_variant() {
        assert_eq!(
            semantic_goop_type(GoopBehavior::Slippery, Some("TestChoco2")),
            "Chocolate"
        );
        assert_eq!(
            semantic_goop_type(GoopBehavior::Slippery, Some("B_RAKenogu_pink")),
            "Pink"
        );
        assert_eq!(
            semantic_goop_type(GoopBehavior::Slippery, Some("B_ricoDrDr")),
            "Oil"
        );
        assert!(GOOP_TEMPLATE_ANIMATION_EXTENSIONS.contains(&"bpk"));

        let templates = vec![
            retail_template("airport0", 0, GoopBehavior::Slippery, "Pink"),
            retail_template("bianco0", 0, GoopBehavior::Slippery, "Chocolate"),
            retail_template("monte0", 0, GoopBehavior::Fire, "Fire"),
            retail_template("sirena0", 0, GoopBehavior::Electric, "Electric"),
        ];

        let choices = goop_template_choices(&templates, 1, false);
        assert_eq!(choices.len(), 4);
        assert_eq!(choices[0].1.behavior, GoopBehavior::Fire);
        assert_eq!(choices[1].0, 1);
        assert_eq!(choices[1].1.behavior, GoopBehavior::Slippery);
        assert_eq!(choices[2].1.semantic_type, "Pink");
        assert_eq!(choices[3].1.behavior, GoopBehavior::Electric);
        assert_eq!(goop_template_label(&choices[1].1, false), "Chocolate");
        assert!(!goop_template_label(&choices[1].1, false).contains("bianco"));

        let expert = goop_template_choices(&templates, 1, true);
        assert_eq!(expert.len(), templates.len());
        assert!(goop_template_label(&expert[1].1, true).contains("bianco0 / pollution00"));
    }

    #[test]
    fn legacy_generated_layers_request_exactly_one_automatic_rebuild() {
        let mut authoring = GoopAuthoringDocument {
            format_version: GOOP_AUTHORING_FORMAT_VERSION - 1,
            layers: vec![editable_layer()],
            terrain_fingerprint: 0,
            stale: false,
        };
        assert!(generated_goop_requires_upgrade(&authoring));
        assert!(authoring
            .validate()
            .unwrap_err()
            .to_string()
            .contains("authoring format version"));

        authoring.format_version = GOOP_AUTHORING_FORMAT_VERSION;
        assert!(!generated_goop_requires_upgrade(&authoring));

        authoring.format_version = GOOP_AUTHORING_FORMAT_VERSION - 1;
        authoring.layers[0].origin = GoopLayerOrigin::Imported;
        assert!(!generated_goop_requires_upgrade(&authoring));
    }

    #[test]
    fn deleting_the_last_generated_layer_clears_stale_release_state() {
        let mut authoring = GoopAuthoringDocument {
            format_version: GOOP_AUTHORING_FORMAT_VERSION - 1,
            layers: vec![editable_layer()],
            terrain_fingerprint: 123,
            stale: true,
        };
        clear_generated_goop_readiness_if_empty(&mut authoring);
        assert!(authoring.stale);

        authoring.layers.clear();
        clear_generated_goop_readiness_if_empty(&mut authoring);
        assert!(!authoring.stale);
        assert_eq!(authoring.terrain_fingerprint, 0);
        assert_eq!(authoring.format_version, GOOP_AUTHORING_FORMAT_VERSION);
    }

    #[test]
    fn pixel_changes_are_coalesced_into_contiguous_spans() {
        let changes = BTreeMap::from([(2, (0, 1)), (3, (2, 3)), (7, (4, 5))]);
        let spans = coalesce_pixel_changes(changes);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].start, 2);
        assert_eq!(spans[0].before, vec![0, 2]);
        assert_eq!(spans[0].after, vec![1, 3]);
    }

    #[test]
    fn collision_raycast_hits_triangle() {
        let distance = ray_triangle_distance(
            [0.25, 1.0, 0.25],
            [0.0, -1.0, 0.0],
            [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
        );
        assert_eq!(distance, Some(1.0));
    }

    #[test]
    fn generated_depth_uses_only_the_runtime_collision_ground_list() {
        let vertices = vec![
            sms_formats::ColVertex::new(0.0, 0.0, 0.0),
            sms_formats::ColVertex::new(0.0, 0.0, 100.0),
            sms_formats::ColVertex::new(100.0, 0.0, 0.0),
            sms_formats::ColVertex::new(0.0, 100.0, 0.0),
        ];
        let triangle = |vertex_indices| sms_formats::ColTriangle {
            vertex_indices,
            attribute_0: 0,
            attribute_1: 0,
            data: None,
        };
        let collision = sms_formats::ColFile::new(
            vertices,
            vec![
                sms_formats::ColGroup {
                    surface_type: 0,
                    has_per_triangle_data: false,
                    triangles: vec![
                        triangle([0, 1, 2]),
                        triangle([0, 2, 1]),
                        triangle([0, 3, 1]),
                    ],
                },
                sms_formats::ColGroup {
                    surface_type: 0x0801,
                    has_per_triangle_data: false,
                    triangles: vec![triangle([0, 3, 1])],
                },
            ],
        );

        assert!(collision_triangle_is_runtime_ground(
            [[0.0, 0.0, 0.0], [0.0, 0.0, 100.0], [100.0, 0.0, 0.0]],
            0
        ));
        assert!(!collision_triangle_is_runtime_ground(
            [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0]],
            0
        ));
        assert!(!collision_triangle_is_runtime_ground(
            [[0.0, 0.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 100.0]],
            0
        ));
        assert!(collision_triangle_is_runtime_ground(
            [[0.0, 0.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 100.0]],
            0x0801
        ));
        assert_eq!(goop_collision_triangles(&collision).len(), 2);
    }

    #[test]
    fn viewport_ray_is_the_inverse_of_zoomed_and_panned_projection() {
        let mut app = SmsEditorApp::default();
        {
            let camera = app.renderer.camera_mut();
            camera.focus = [120.0, 80.0, 300.0];
            camera.yaw_degrees = 37.0;
            camera.pitch_degrees = -28.0;
            camera.distance = 4_800.0;
        }
        app.viewport_pan = egui::vec2(83.0, -47.0);
        app.viewport_zoom = 0.35;
        let rect = egui::Rect::from_min_size(egui::pos2(25.0, 40.0), egui::vec2(1_300.0, 620.0));
        let frame = app.camera_frame();
        let target = vec3_add(
            frame.position,
            vec3_add(
                vec3_scale(frame.forward, 6_000.0),
                vec3_add(
                    vec3_scale(frame.right, 1_150.0),
                    vec3_scale(frame.up, -620.0),
                ),
            ),
        );
        let screen = app
            .project_world_to_screen(rect, target)
            .expect("target remains in front of camera")
            .0;
        let ray =
            goop_viewport_ray_direction(frame, rect, app.viewport_pan, app.viewport_zoom, screen);
        let expected = vec3_normalize(vec3_sub(target, frame.position));
        for axis in 0..3 {
            assert!((ray[axis] - expected[axis]).abs() < 0.000_01);
        }
    }

    #[test]
    fn collision_wall_cannot_steal_the_brush_from_the_active_floor() {
        let layer = editable_layer();
        let wall = [[0.0, 0.0, 0.0], [320.0, 0.0, 0.0], [0.0, 100.0, 0.0]];
        let floor = [[0.0, 0.0, 0.0], [0.0, 0.0, 160.0], [320.0, 0.0, 0.0]];
        let origin = [60.0, 100.0, -100.0];
        let direction = vec3_normalize([0.0, -1.0, 2.0]);

        assert!(!triangle_has_floor_projection(wall));
        assert!(triangle_has_floor_projection(floor));
        assert!(ray_triangle_distance(origin, direction, wall).is_some());
        let hit = nearest_goop_surface_hit(
            Some(&layer),
            origin,
            direction,
            [wall, floor]
                .into_iter()
                .filter(|vertices| triangle_has_floor_projection(*vertices)),
        )
        .expect("floor remains targetable behind collision wall");
        assert!((hit[0] - 60.0).abs() < 0.001);
        assert!(hit[1].abs() < 0.001);
        assert!((hit[2] - 100.0).abs() < 0.001);
    }

    #[test]
    fn nearer_upward_prop_cannot_steal_brush_from_authoritative_map_floor() {
        let triangle = |model_index, height| PreviewTriangle {
            vertices: [
                [0.0, height, 0.0],
                [100.0, height, 0.0],
                [0.0, height, 100.0],
            ],
            normals: Some([[0.0, 1.0, 0.0]; 3]),
            color_channels: [None; 2],
            tex_coord_sets: [None; 8],
            material_index: None,
            packet_index: 0,
            model_index,
            render_layer: PreviewRenderLayer::Main,
            color: None,
            vertex_colors: None,
            combine_mode: J3dPreviewCombineMode::VertexOnly,
            tex_coords: None,
            texture_index: None,
            mask_tex_coords: None,
            mask_texture_index: None,
            cull_mode: None,
            alpha_compare: None,
            blend_mode: None,
            z_mode: None,
            billboard: None,
            particle_type: None,
            particle_pivot: None,
            particle_direction: None,
            particle_color_mode: None,
            particle_environment_color: None,
        };
        let prop = triangle(2, 50.0);
        let floor = triangle(1, 0.0);
        let origin = [40.0, 100.0, 40.0];
        let direction = [0.0, -1.0, 0.0];
        let layer = editable_layer();

        let unfiltered = nearest_goop_surface_hit(
            Some(&layer),
            origin,
            direction,
            [prop.vertices, floor.vertices],
        )
        .expect("nearer prop is a geometric hit");
        assert!((unfiltered[1] - 50.0).abs() < 0.001);

        let hit = nearest_goop_render_surface_hit(
            Some(&layer),
            origin,
            direction,
            &BTreeSet::from([1]),
            [&prop, &floor],
        )
        .expect("authoritative map floor remains targetable behind the prop");
        assert!(hit[1].abs() < 0.001);
    }

    #[test]
    fn invalid_and_edge_cells_keep_a_targetable_red_brush_footprint() {
        let mut layer = editable_layer();
        layer.runtime.set_depth(2, 1, 0xff).unwrap();
        assert!(goop_hit_matches_active_floor(&layer, [100.0, 20.0, 60.0]));
        assert!(goop_hit_matches_active_floor(&layer, [-5.0, 0.0, 60.0]));

        assert!(!goop_hit_matches_active_floor(&layer, [60.0, 200.0, 60.0]));
    }

    #[test]
    fn active_floor_hit_uses_runtime_depth_truncation_and_inclusive_window() {
        let mut layer = editable_layer();
        layer.origin = GoopLayerOrigin::Imported;
        layer.runtime.vertical_offset = 40.0;
        layer.runtime.vertical_scale = 32.0;

        // Cell (1, 1) stores depth zero. isSame accepts encoded -2..=2.
        // Values just inside the next 40-unit interval still truncate toward
        // zero, while crossing it produces +/-3 and must reject the surface.
        assert!(goop_hit_matches_active_floor(&layer, [60.0, -79.0, 60.0]));
        assert!(!goop_hit_matches_active_floor(&layer, [60.0, -81.0, 60.0]));
        assert!(goop_hit_matches_active_floor(&layer, [60.0, 159.0, 60.0]));
        assert!(!goop_hit_matches_active_floor(&layer, [60.0, 161.0, 60.0]));
    }

    #[test]
    fn brush_outline_is_a_world_xz_circle_that_follows_layer_height() {
        let mut layer = editable_layer();
        layer.runtime.vertical_offset = 40.0;
        layer.runtime.set_depth(5, 2, 2).unwrap();
        let center = [160.0, 40.0, 80.0];
        let outline = goop_brush_outline(&layer, center, 60.0);
        assert_eq!(outline.len(), 65);
        for point in &outline {
            assert!(((point[0] - center[0]).hypot(point[2] - center[2]) - 60.0).abs() < 0.001);
        }
        assert!(outline.iter().any(|point| (point[1] - 128.0).abs() < 0.001));
    }

    #[test]
    fn render_surface_filter_keeps_floors_and_rejects_walls_and_undersides() {
        let floor = [[0.0, 25.0, 0.0], [100.0, 25.0, 0.0], [0.0, 25.0, 100.0]];
        assert!(render_triangle_is_upward_facing(
            floor,
            Some([[0.0, 1.0, 0.0]; 3])
        ));
        assert!(render_triangle_is_upward_facing(floor, None));

        let wall = [[0.0, 0.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 100.0]];
        assert!(!render_triangle_is_upward_facing(
            wall,
            Some([[1.0, 0.0, 0.0]; 3])
        ));
        assert!(!render_triangle_is_upward_facing(wall, None));

        assert!(!render_triangle_is_upward_facing(
            floor,
            Some([[0.0, -1.0, 0.0]; 3])
        ));
        let reversed_floor = [floor[0], floor[2], floor[1]];
        assert!(!render_triangle_is_upward_facing(reversed_floor, None));
    }

    #[test]
    fn brush_combines_opacity_and_rejects_invalid_cells() {
        let mut layer = editable_layer();
        layer.runtime.set_depth(2, 1, 0xff).unwrap();
        let mut mask = vec![0; 32];
        let mut changes = Vec::new();
        paint_brush_sample(
            &layer,
            &mut mask,
            [60.0, 0.0, 60.0],
            GoopBrush {
                radius: 100.0,
                hardness: 1.0,
                opacity: 0.5,
                erase: false,
            },
            &mut changes,
        );
        assert_eq!(mask[1 + 8], 128);
        assert_eq!(mask[2 + 8], 0);
        assert!(!changes.is_empty());
    }

    #[test]
    fn retail_scale_controls_world_cell_mapping_and_brush_centers() {
        let mut layer = editable_layer();
        layer.runtime.vertical_scale = 32.0;
        layer.region.max_x = 256.0;
        layer.region.max_z = 128.0;
        assert_eq!(layer.world_to_cell(80.0, 48.0), Some((2, 1)));

        let mut mask = vec![0; 32];
        let mut changes = Vec::new();
        paint_brush_sample(
            &layer,
            &mut mask,
            [80.0, 0.0, 48.0],
            GoopBrush {
                radius: 10.0,
                hardness: 1.0,
                opacity: 1.0,
                erase: false,
            },
            &mut changes,
        );
        assert_eq!(mask[10], 255);
        assert_eq!(changes, vec![(10, 0, 255)]);
    }

    #[test]
    fn connected_fill_stops_at_invalid_depth_cells() {
        let mut layer = editable_layer();
        for y in 0..4 {
            layer.runtime.set_depth(4, y, 0xff).unwrap();
        }
        let mut mask = vec![0; 32];
        let mut changes = Vec::new();
        flood_fill(&layer, &mut mask, [1, 1], false, &mut changes);
        assert!(mask
            .chunks_exact(8)
            .all(|row| row[..4].iter().all(|value| *value == 255)));
        assert!(mask
            .chunks_exact(8)
            .all(|row| row[4..].iter().all(|value| *value == 0)));
    }

    #[test]
    fn imported_overlay_uses_cleanability_depth_and_retail_horizontal_scale() {
        let mut layer = editable_layer();
        layer.origin = GoopLayerOrigin::Imported;
        layer.region.max_x = 256.0;
        layer.region.max_z = 128.0;
        layer.runtime.max_x = 256.0;
        layer.runtime.max_z = 128.0;
        layer.runtime.vertical_offset = -80.0;
        layer.runtime.vertical_scale = 32.0;
        assert_eq!(layer.world_to_cell(80.0, 48.0), Some((2, 1)));

        layer.runtime.set_depth(2, 1, 3).unwrap();
        assert_eq!(goop_cell_cleanable_surface_y(&layer, 2, 1), Some(40.0));

        layer.runtime.set_depth(3, 1, 0xff).unwrap();
        assert_eq!(goop_invalid_marker_surface_y(&layer, 3, 1), Some(40.0));

        for y in 0..4 {
            for x in 0..8 {
                layer.runtime.set_depth(x, y, 0xff).unwrap();
            }
        }
        assert_eq!(goop_invalid_marker_surface_y(&layer, 3, 1), None);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn every_retail_goop_animation_survives_material_filter_and_reparse() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives =
            sms_formats::discover_scene_archives(root).expect("discover retail scene archives");
        let (templates, warnings) = index_retail_goop_templates(&archives);
        assert!(!templates.is_empty(), "no goop templates: {warnings:#?}");
        let mut checked = 0;
        for template in templates {
            let model = read_stage_asset_bytes(&template.model_asset_path)
                .and_then(J3dRebuildDocument::parse)
                .expect("parse retail goop model");
            let material_name = pollution_material_name(&model).expect("material-zero name");
            let bytes = fs::read(&template.archive_path).expect("read retail stage archive");
            let archive = SourceFreeStageArchive::parse(&bytes).unwrap_or_else(|error| {
                panic!("parse {}: {error}", template.archive_path.display())
            });
            for resource in archive.resources() {
                let path = String::from_utf8_lossy(&resource.raw_path).replace('\\', "/");
                let candidate = Path::new(&path);
                let is_template_animation = candidate
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(&template.resource_stem))
                    && candidate
                        .extension()
                        .and_then(|value| value.to_str())
                        .is_some_and(|value| {
                            GOOP_TEMPLATE_ANIMATION_EXTENSIONS
                                .iter()
                                .any(|extension| value.eq_ignore_ascii_case(extension))
                        });
                let StageResourceDocument::Animation(animation) = &resource.document else {
                    continue;
                };
                if !is_template_animation {
                    continue;
                }
                let mut filtered = (**animation).clone();
                if filtered
                    .retain_material_bindings_named(&material_name)
                    .expect("filter retail goop animation")
                    .is_some_and(|retained| retained == 0)
                {
                    continue;
                }
                let encoded = filtered.encode().expect("encode filtered goop animation");
                sms_formats::J3dAnimationRebuildDocument::parse(&encoded).unwrap_or_else(|error| {
                    panic!("reparse {}!/{}: {error}", template.stage_id, path)
                });
                checked += 1;
            }
        }
        assert!(checked > 0, "retail goop census found no bound animations");
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT and SMS_PROJECT_ROOT with authored goopmap0"]
    fn authored_project_migrates_case_alias_and_bakes_primary_actor_goop_texture() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT");
        let project_root = std::env::var_os("SMS_PROJECT_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_PROJECT_ROOT");
        let mut document =
            StageDocument::open_authored_project_stage(&base_root, "goopmap0", &project_root)
                .expect("open authored goopmap0");
        let style = document
            .goop_authoring
            .as_ref()
            .and_then(|goop| {
                goop.layers
                    .iter()
                    .find_map(|layer| layer.style_source.clone())
            })
            .expect("primary goop style");
        let archive = sms_formats::discover_scene_archives(&base_root)
            .expect("discover retail stages")
            .into_iter()
            .find(|archive| archive.stage_id == style.stage_id)
            .expect("primary style archive");
        document.registry = Some(ObjectRegistry {
            runtime_texture_replacements: ["Mario", "StayPakkun"]
                .into_iter()
                .map(
                    |factory_name| sms_schema::RuntimeTextureReplacementDefinition {
                        factory_name: factory_name.to_string(),
                        dummy_texture_name: "H_ma_rak_dummy".to_string(),
                        resource_path: "/scene/map/pollution/H_ma_rak.bti".to_string(),
                        source_file: "regression".to_string(),
                    },
                )
                .collect(),
            ..ObjectRegistry::default()
        });

        sync_runtime_actor_goop_textures_from_source(&mut document, &style, &archive.path)
            .expect("migrate runtime texture aliases");
        let aliases = document
            .archive_edits
            .resources
            .iter()
            .filter(|edit| {
                String::from_utf8_lossy(&edit.raw_resource_path)
                    .eq_ignore_ascii_case("map/pollution/H_ma_rak.bti")
            })
            .collect::<Vec<_>>();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].raw_resource_path, b"map/pollution/H_ma_rak.bti");

        let rebuilt = SourceFreeStageArchive::parse(
            &document
                .build_stage_archive()
                .expect("build migrated authored stage"),
        )
        .expect("reopen migrated authored stage");
        let StageResourceDocument::Texture(texture) = rebuilt
            .resource(b"map/pollution/H_ma_rak.bti")
            .expect("canonical actor goop texture")
        else {
            panic!("actor goop resource is not a texture");
        };
        let expected = sms_formats::decode_bti_texture(texture.encode().unwrap()).unwrap();
        let StageResourceDocument::Model(model) =
            rebuilt.resource(b"pakkun/pakun.bmd").expect("Pakkun model")
        else {
            panic!("Pakkun resource is not a model");
        };
        let baked = J3dFile::parse(model.to_bytes().unwrap())
            .unwrap()
            .texture_previews()
            .unwrap()
            .into_iter()
            .find(|texture| texture.name == "H_ma_rak_dummy")
            .expect("baked Pakkun actor goop texture");
        assert_eq!(baked.width, expected.width);
        assert_eq!(baked.height, expected.height);
        assert_eq!(baked.rgba, expected.rgba);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn retail_floor_template_census_finds_safe_templates() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives =
            sms_formats::discover_scene_archives(root).expect("discover retail scene archives");
        let (templates, warnings) = index_retail_goop_templates(&archives);
        let compatible = templates
            .iter()
            .filter(|template| template.compatible)
            .collect::<Vec<_>>();
        assert!(
            !compatible.is_empty(),
            "retail census found no structurally safe floor-goop templates; warnings: {warnings:#?}"
        );
        for template in compatible {
            assert!(template.archive_path.is_file());
            assert!(!template.resource_stem.is_empty());
            let model = read_stage_asset_bytes(&template.model_asset_path)
                .and_then(J3dRebuildDocument::parse)
                .expect("reparse compatible retail goop template");
            let template_preview =
                sms_formats::J3dFile::parse(model.to_bytes().expect("encode retail goop template"))
                    .expect("parse retail goop template")
                    .geometry_preview()
                    .expect("preview retail goop template");
            let expects_detail = template_preview
                .materials
                .iter()
                .find(|material| material.material_index == 0)
                .is_some_and(|material| {
                    material.tev_stages.iter().any(|stage| {
                        stage.order.tex_map.is_some_and(|texture| texture > 0)
                            && stage.order.tex_coord.is_some_and(|coord| coord > 0)
                    })
                });
            assert!(model
                .sections
                .iter()
                .any(|section| { matches!(section.data, J3dRebuildSectionData::Materials(_)) }));
            assert_eq!(
                model.sections.iter().find_map(|section| {
                    if let J3dRebuildSectionData::Textures(textures) = &section.data {
                        textures.textures.first().map(|texture| texture.format)
                    } else {
                        None
                    }
                }),
                Some(1)
            );

            let region = sms_scene::GoopRegion {
                min_x: 0.0,
                min_z: 0.0,
                max_x: 320.0,
                max_z: 160.0,
            };
            // Feed the clockwise GX winding and the upward vertex normals
            // decoded from the finalized map-render BMD.
            let terrain = vec![GoopRenderTriangle {
                vertices: [[0.0, 0.0, 0.0], [320.0, 0.0, 0.0], [0.0, 0.0, 160.0]],
                normals: Some([[0.0, 1.0, 0.0]; 3]),
            }];
            let runtime = YmpLayer {
                layer_type: 0,
                subtype: 0,
                flags: GoopPlane::Floor.runtime_code(),
                reserved: 0,
                vertical_offset: 0.0,
                vertical_scale: GOOP_CELL_SIZE,
                min_x: region.min_x,
                min_z: region.min_z,
                max_x: region.max_x,
                max_z: region.max_z,
                width_log2: 3,
                height_log2: 2,
                user_value: 0,
                map_offset: 0,
                depth_map: vec![0; 32],
            };
            let generated = generate_floor_pollution_model(&model, &terrain, &runtime, false)
                .expect("generate with compatible retail material template");
            assert!(generated
                .sections
                .iter()
                .all(|section| section.declared_size.is_multiple_of(0x20)));
            let preview = sms_formats::J3dFile::parse(
                generated
                    .to_bytes()
                    .expect("encode generated pollution BMD"),
            )
            .expect("parse generated pollution BMD")
            .geometry_preview()
            .expect("preview generated pollution BMD");
            assert!(!preview.triangles.is_empty());
            assert!(preview.triangles.iter().all(|triangle| {
                let [a, b, c] = triangle.vertices;
                let normal_y = (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]);
                normal_y < 0.0 && triangle.cull_mode == Some(2)
            }));
            assert!(
                preview.triangles.iter().any(|triangle| {
                    triangle.texture_index == Some(0) || triangle.mask_texture_index == Some(0)
                }),
                "compatible template {} / {} does not bind mutable texture zero",
                template.stage_id,
                template.resource_stem
            );
            if expects_detail {
                let material = preview
                    .materials
                    .iter()
                    .find(|material| material.material_index == 0)
                    .expect("generated pollution BMD material zero");
                let usable_detail = material.tev_stages.iter().any(|stage| {
                    let (Some(texture_slot), Some(tex_gen_slot)) =
                        (stage.order.tex_map, stage.order.tex_coord)
                    else {
                        return false;
                    };
                    if texture_slot == 0
                        || tex_gen_slot == 0
                        || material.texture_indices[usize::from(texture_slot)].is_none()
                    {
                        return false;
                    }
                    let source = material.tex_gens[usize::from(tex_gen_slot)].source;
                    let Some(vertex_slot) = source
                        .checked_sub(4)
                        .map(usize::from)
                        .filter(|slot| *slot < 8)
                    else {
                        return false;
                    };
                    let coords = preview
                        .triangles
                        .iter()
                        .filter_map(|triangle| triangle.tex_coord_sets[vertex_slot])
                        .flatten()
                        .collect::<Vec<_>>();
                    coords.first().is_some_and(|first| {
                        coords.iter().skip(1).any(|coord| {
                            (coord[0] - first[0]).abs() > 0.000001
                                || (coord[1] - first[1]).abs() > 0.000001
                        })
                    })
                });
                assert!(
                    usable_detail,
                    "compatible template {} / {} lost its nonzero detail texture coordinates",
                    template.stage_id, template.resource_stem
                );
            }
        }
    }
}
