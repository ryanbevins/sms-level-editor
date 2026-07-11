use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use eframe::egui;
use sms_formats::{
    discover_scene_archives, read_stage_asset_bytes, J3dAlphaCompare, J3dBlendMode, J3dFile,
    J3dGeometryPreview, J3dMaterial, J3dPreviewCombineMode, J3dZMode, SceneArchiveInfo,
    StageAssetKind, SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS, SMS_MAP_MODEL_LOAD_FLAGS,
    SMS_POLLUTION_MODEL_LOAD_FLAGS,
};
use sms_render::{RenderScene, RendererConfig, ViewportRenderer};
use sms_scene::{
    AssetRef, AssetRole, SceneObject, StageDocument, Transform, ValidationIssue, ValidationSeverity,
};
use sms_schema::{ObjectDefinition, ObjectRegistry, SchemaGenerator};

mod gpu_viewport;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SMS Editor")
            .with_inner_size([1560.0, 940.0]),
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 24,
        ..Default::default()
    };

    eframe::run_native(
        "SMS Editor",
        options,
        Box::new(|cc| {
            install_style(&cc.egui_ctx);
            Ok(Box::new(SmsEditorApp::new(cc)))
        }),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTool {
    Select,
    Move,
    Rotate,
    Scale,
    Place,
}

impl EditorTool {
    fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Move => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
            Self::Place => "Place",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Lit,
    Collision,
    Objects,
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Lit => "Lit",
            Self::Collision => "Collision",
            Self::Objects => "Objects",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftTab {
    Project,
    Content,
    Palette,
    Outliner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightTab {
    Inspector,
    Assets,
    Issues,
}

#[derive(Clone)]
struct ModelPreview {
    points: Vec<PreviewPoint>,
    triangles: Vec<PreviewTriangle>,
    textures: Vec<PreviewTexture>,
    materials: Vec<J3dMaterial>,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    camera_bounds_min: [f32; 3],
    camera_bounds_max: [f32; 3],
    sky_radius: f32,
    loaded_models: usize,
    failed_models: usize,
    source_vertices: usize,
    source_triangles: usize,
    source_textures: usize,
    object_model_indices: BTreeMap<String, usize>,
}

impl ModelPreview {
    fn center(&self) -> [f32; 3] {
        [
            (self.camera_bounds_min[0] + self.camera_bounds_max[0]) * 0.5,
            (self.camera_bounds_min[1] + self.camera_bounds_max[1]) * 0.5,
            (self.camera_bounds_min[2] + self.camera_bounds_max[2]) * 0.5,
        ]
    }

    fn radius(&self) -> f32 {
        let dx = self.camera_bounds_max[0] - self.camera_bounds_min[0];
        let dy = self.camera_bounds_max[1] - self.camera_bounds_min[1];
        let dz = self.camera_bounds_max[2] - self.camera_bounds_min[2];
        ((dx * dx + dy * dy + dz * dz).sqrt() * 0.5).max(1000.0)
    }

    fn far_clip(&self, camera_distance: f32) -> f32 {
        (camera_distance + self.radius() * 5.0)
            .max(self.sky_radius * 1.05)
            .max(20_000.0)
    }
}

#[derive(Debug, Clone, Copy)]
struct PreviewPoint {
    position: [f32; 3],
    model_index: usize,
}

#[derive(Debug, Clone, Copy)]
struct PreviewTriangle {
    vertices: [[f32; 3]; 3],
    normals: Option<[[f32; 3]; 3]>,
    color_channels: [Option<[[u8; 4]; 3]>; 2],
    tex_coord_sets: [Option<[[f32; 2]; 3]>; 8],
    material_index: Option<usize>,
    packet_index: usize,
    model_index: usize,
    render_layer: PreviewRenderLayer,
    color: Option<[u8; 4]>,
    vertex_colors: Option<[[u8; 4]; 3]>,
    combine_mode: J3dPreviewCombineMode,
    tex_coords: Option<[[f32; 2]; 3]>,
    texture_index: Option<usize>,
    mask_tex_coords: Option<[[f32; 2]; 3]>,
    mask_texture_index: Option<usize>,
    cull_mode: Option<u8>,
    alpha_compare: Option<J3dAlphaCompare>,
    blend_mode: Option<J3dBlendMode>,
    z_mode: Option<J3dZMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewRenderLayer {
    Sky,
    Main,
    Water,
    Goop,
}

#[derive(Debug, Clone, Copy)]
struct ProjectedVertex {
    x: f32,
    y: f32,
    depth: f32,
    inv_depth: f32,
}

#[derive(Debug, Clone, Copy)]
struct CameraFrame {
    position: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    forward: [f32; 3],
}

#[derive(Clone, Copy)]
struct ProjectedPreviewTriangle<'a> {
    triangle: &'a PreviewTriangle,
    screen: [ProjectedVertex; 3],
    average_depth: f32,
}

#[derive(Clone)]
struct PreviewTexture {
    image: egui::ColorImage,
    mips: Vec<egui::ColorImage>,
    format: u8,
    wrap_s: u8,
    wrap_t: u8,
    min_filter: u8,
    mag_filter: u8,
    mipmap_count: u8,
    has_alpha: bool,
    has_translucent_alpha: bool,
}

#[derive(Clone)]
struct CachedObjectModelPreview {
    preview: J3dGeometryPreview,
    texture_base: usize,
    material_base: usize,
}

#[derive(Clone, PartialEq)]
struct ModelFramebufferKey {
    stage_id: String,
    size: [usize; 2],
    camera_focus: [u32; 3],
    camera_yaw: u32,
    camera_pitch: u32,
    camera_distance: u32,
    viewport_pan: [u32; 2],
    viewport_zoom: u32,
    triangle_count: usize,
    texture_count: usize,
    source_triangles: usize,
}

struct SmsEditorApp {
    repo_root: String,
    base_root: String,
    mod_root: String,
    stage_id: String,
    dolphin_path: String,
    game_path: String,
    dolphin_user_dir: String,
    registry: Option<ObjectRegistry>,
    document: Option<StageDocument>,
    scene_archives: Vec<SceneArchiveInfo>,
    model_preview: Option<ModelPreview>,
    gpu_viewport: Option<gpu_viewport::GpuViewportScene>,
    gpu_target_format: Option<eframe::wgpu::TextureFormat>,
    model_framebuffer: Option<egui::TextureHandle>,
    model_framebuffer_key: Option<ModelFramebufferKey>,
    issues: Vec<ValidationIssue>,
    log: Vec<String>,
    renderer: ViewportRenderer,
    selected_object_id: Option<String>,
    palette_factory: Option<String>,
    object_filter: String,
    scene_filter: String,
    last_scanned_base_root: String,
    tool: EditorTool,
    view_mode: ViewMode,
    left_tab: LeftTab,
    right_tab: RightTab,
    snap_enabled: bool,
    snap_translation: f32,
    snap_rotation: f32,
    snap_scale: f32,
    show_environment_meshes: bool,
    show_goop_meshes: bool,
    show_effect_meshes: bool,
    startup_camera_focus: Option<[f32; 3]>,
    startup_focus_object: Option<String>,
    startup_camera_distance: Option<f32>,
    startup_camera_yaw: Option<f32>,
    startup_camera_pitch: Option<f32>,
    viewport_pan: egui::Vec2,
    viewport_zoom: f32,
    camera_speed: f32,
    next_object_serial: u32,
    undo_stack: Vec<Vec<SceneObject>>,
    redo_stack: Vec<Vec<SceneObject>>,
}

impl Default for SmsEditorApp {
    fn default() -> Self {
        let args = editor_startup_args();
        let base_root = args.base_root.unwrap_or_else(default_base_root);
        Self {
            repo_root: args.repo_root.unwrap_or_else(default_repo_root),
            base_root,
            mod_root: "sms-mod".to_string(),
            stage_id: args.stage_id.unwrap_or_else(|| "dolpic0".to_string()),
            dolphin_path: String::new(),
            game_path: String::new(),
            dolphin_user_dir: String::new(),
            registry: None,
            document: None,
            scene_archives: Vec::new(),
            model_preview: None,
            gpu_viewport: None,
            gpu_target_format: None,
            model_framebuffer: None,
            model_framebuffer_key: None,
            issues: Vec::new(),
            log: vec!["Ready.".to_string()],
            renderer: ViewportRenderer::new(RendererConfig::default()),
            selected_object_id: None,
            palette_factory: None,
            object_filter: String::new(),
            scene_filter: String::new(),
            last_scanned_base_root: String::new(),
            tool: EditorTool::Select,
            view_mode: ViewMode::Lit,
            left_tab: LeftTab::Content,
            right_tab: RightTab::Inspector,
            snap_enabled: true,
            snap_translation: 50.0,
            snap_rotation: 15.0,
            snap_scale: 0.1,
            show_environment_meshes: true,
            show_goop_meshes: true,
            show_effect_meshes: false,
            startup_camera_focus: args.camera_focus,
            startup_focus_object: args.focus_object,
            startup_camera_distance: args.camera_distance,
            startup_camera_yaw: args.camera_yaw,
            startup_camera_pitch: args.camera_pitch,
            viewport_pan: egui::Vec2::ZERO,
            viewport_zoom: 1.0,
            camera_speed: 1.0,
            next_object_serial: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }
}

impl SmsEditorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            gpu_target_format: cc
                .wgpu_render_state
                .as_ref()
                .map(|render_state| render_state.target_format),
            ..Self::default()
        }
    }
}

impl eframe::App for SmsEditorApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Z)) {
            self.undo();
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Y)) {
            self.redo();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
            self.delete_selected();
        }
        if ctx.input(|i| i.pointer.secondary_down()) {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::W)) {
            self.tool = EditorTool::Move;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::E)) {
            self.tool = EditorTool::Rotate;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            self.tool = EditorTool::Scale;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Q)) {
            self.tool = EditorTool::Select;
        }
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.refresh_scene_browser_if_needed();

        egui::Panel::top("toolbar")
            .default_size(48.0)
            .show(root_ui, |ui| self.toolbar(ui));

        egui::Panel::left("left_dock")
            .resizable(true)
            .default_size(350.0)
            .show(root_ui, |ui| self.left_dock(ui));

        egui::Panel::right("right_dock")
            .resizable(true)
            .default_size(390.0)
            .show(root_ui, |ui| self.right_dock(ui));

        egui::Panel::bottom("console")
            .resizable(true)
            .default_size(150.0)
            .show(root_ui, |ui| self.console(ui));

        egui::CentralPanel::default().show(root_ui, |ui| self.viewport(ui));
    }
}

impl SmsEditorApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);

            let open_enabled = !self.base_root.trim().is_empty();
            if command_button(ui, "Schema", true).clicked() {
                self.generate_schema();
            }
            if command_button(ui, "Open", open_enabled).clicked() {
                self.open_stage();
            }
            if command_button(ui, "Validate", self.document.is_some()).clicked() {
                self.validate();
            }
            if command_button(ui, "Export", self.document.is_some()).clicked() {
                self.export_mod();
            }
            if command_button(ui, "Launch", true).clicked() {
                self.launch_dolphin();
            }

            ui.separator();
            for tool in [
                EditorTool::Select,
                EditorTool::Move,
                EditorTool::Rotate,
                EditorTool::Scale,
                EditorTool::Place,
            ] {
                if ui
                    .selectable_label(self.tool == tool, tool.label())
                    .on_hover_text(format!("{} tool", tool.label()))
                    .clicked()
                {
                    self.tool = tool;
                }
            }

            ui.separator();
            for mode in [ViewMode::Lit, ViewMode::Collision, ViewMode::Objects] {
                if ui
                    .selectable_label(self.view_mode == mode, mode.label())
                    .clicked()
                {
                    self.view_mode = mode;
                }
            }

            ui.separator();
            if ui
                .add_enabled(self.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.redo();
            }

            ui.separator();
            let (warnings, errors) = self.issue_counts();
            ui.colored_label(
                if errors > 0 {
                    egui::Color32::from_rgb(255, 116, 104)
                } else if warnings > 0 {
                    egui::Color32::from_rgb(235, 190, 92)
                } else {
                    egui::Color32::from_rgb(111, 220, 168)
                },
                format!("{} warnings  {} errors", warnings, errors),
            );
        });
    }

    fn left_dock(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.left_tab, LeftTab::Project, "Project");
            ui.selectable_value(&mut self.left_tab, LeftTab::Content, "Content");
            ui.selectable_value(&mut self.left_tab, LeftTab::Palette, "Palette");
            ui.selectable_value(&mut self.left_tab, LeftTab::Outliner, "Outliner");
        });
        ui.separator();

        match self.left_tab {
            LeftTab::Project => self.project_panel(ui),
            LeftTab::Content => self.content_browser_panel(ui),
            LeftTab::Palette => self.palette_panel(ui),
            LeftTab::Outliner => self.outliner_panel(ui),
        }
    }

    fn right_dock(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.right_tab, RightTab::Inspector, "Inspector");
            ui.selectable_value(&mut self.right_tab, RightTab::Assets, "Assets");
            ui.selectable_value(&mut self.right_tab, RightTab::Issues, "Issues");
        });
        ui.separator();

        match self.right_tab {
            RightTab::Inspector => self.inspector_panel(ui),
            RightTab::Assets => self.assets_panel(ui),
            RightTab::Issues => self.issues_panel(ui),
        }
    }

    fn project_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("SMS Level Editor");
        ui.add_space(4.0);
        labeled_text(ui, "Repo Root", &mut self.repo_root);
        labeled_text(ui, "Base Root", &mut self.base_root);
        labeled_text(ui, "Mod Folder", &mut self.mod_root);
        labeled_text(ui, "Stage", &mut self.stage_id);

        ui.separator();
        ui.heading("Viewport");
        {
            let config = self.renderer.config_mut();
            ui.checkbox(&mut config.show_grid, "Grid");
            ui.checkbox(&mut config.show_collision, "Collision");
            ui.checkbox(&mut config.show_object_bounds, "Object bounds");
        }
        let environment_changed = ui
            .checkbox(&mut self.show_environment_meshes, "Water")
            .changed();
        let goop_changed = ui.checkbox(&mut self.show_goop_meshes, "Goop").changed();
        let effects_changed = ui
            .checkbox(&mut self.show_effect_meshes, "Effects")
            .changed();
        if environment_changed || goop_changed || effects_changed {
            let model_preview = self
                .document
                .as_ref()
                .and_then(|document| self.build_model_preview(document));
            self.model_preview = model_preview;
            self.rebuild_gpu_viewport_scene();
            self.clear_viewport_preview_cache();
            self.reset_camera();
        }
        ui.add(egui::Slider::new(&mut self.viewport_zoom, 0.35..=2.5).text("Zoom"));
        ui.add(egui::Slider::new(&mut self.camera_speed, 0.1..=8.0).text("Speed"));
        if ui.button("Frame Selection").clicked() {
            self.frame_selected();
        }
        if ui.button("Reset Camera").clicked() {
            self.reset_camera();
        }

        ui.separator();
        ui.heading("Snap");
        ui.checkbox(&mut self.snap_enabled, "Enabled");
        ui.add(
            egui::DragValue::new(&mut self.snap_translation)
                .speed(5.0)
                .prefix("Move "),
        );
        ui.add(
            egui::DragValue::new(&mut self.snap_rotation)
                .speed(1.0)
                .prefix("Rotate "),
        );
        ui.add(
            egui::DragValue::new(&mut self.snap_scale)
                .speed(0.01)
                .prefix("Scale "),
        );

        ui.separator();
        ui.heading("Dolphin");
        labeled_text(ui, "Executable", &mut self.dolphin_path);
        labeled_text(ui, "Game", &mut self.game_path);
        labeled_text(ui, "User Dir", &mut self.dolphin_user_dir);

        ui.separator();
        if let Some(registry) = &self.registry {
            ui.label(format!("{} object schema entries", registry.objects.len()));
            ui.label(format!("{} parameter hints", registry.params.len()));
            ui.label(format!("{} asset hints", registry.asset_hints.len()));
        } else {
            ui.label("Schema not generated.");
        }
    }

    fn content_browser_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Content Browser");
        ui.add_space(4.0);
        labeled_text(ui, "Base Root", &mut self.base_root);

        ui.horizontal(|ui| {
            let can_scan = PathBuf::from(self.base_root.trim()).exists();
            if command_button(ui, "Scan", can_scan).clicked() {
                self.scan_scenes();
            }
            if command_button(ui, "Open", !self.stage_id.trim().is_empty()).clicked() {
                self.open_stage();
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Scenes");
            ui.text_edit_singleline(&mut self.scene_filter);
        });
        ui.small(format!(
            "{} scene archive(s)  current: {}",
            self.scene_archives.len(),
            if self.stage_id.trim().is_empty() {
                "none"
            } else {
                self.stage_id.as_str()
            }
        ));
        ui.separator();

        let filter = self.scene_filter.to_ascii_lowercase();
        let archives: Vec<SceneArchiveInfo> = self
            .scene_archives
            .iter()
            .filter(|archive| {
                filter.is_empty()
                    || archive.stage_id.to_ascii_lowercase().contains(&filter)
                    || archive.group.to_ascii_lowercase().contains(&filter)
                    || archive
                        .relative_path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .cloned()
            .collect();

        let mut open_archive = None;
        let mut current_group = String::new();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for archive in archives {
                if archive.group != current_group {
                    current_group = archive.group.clone();
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(if current_group.is_empty() {
                            "Ungrouped"
                        } else {
                            current_group.as_str()
                        })
                        .strong()
                        .color(egui::Color32::from_rgb(159, 208, 201)),
                    );
                }

                let selected = self.stage_id.eq_ignore_ascii_case(&archive.stage_id);
                let label = format!(
                    "{}    {}",
                    archive.stage_id,
                    format_bytes_short(archive.size_bytes)
                );
                let response = ui
                    .selectable_label(selected, label)
                    .on_hover_text(archive.path.display().to_string());
                ui.small(archive.relative_path.display().to_string());
                ui.separator();

                if response.clicked() {
                    open_archive = Some(archive);
                }
            }
        });

        if let Some(archive) = open_archive {
            self.open_scene_archive(archive);
        }
    }

    fn palette_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Object Palette");
        ui.text_edit_singleline(&mut self.object_filter);
        ui.add_space(4.0);

        let mut chosen: Option<String> = None;
        let mut spawn_now: Option<String> = None;
        let filter = self.object_filter.to_ascii_lowercase();
        let entries: Vec<ObjectDefinition> = self
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .objects
                    .iter()
                    .filter(|object| !object.hidden)
                    .filter(|object| {
                        filter.is_empty()
                            || object.factory_name.to_ascii_lowercase().contains(&filter)
                            || object.class_name.to_ascii_lowercase().contains(&filter)
                            || object.category.to_ascii_lowercase().contains(&filter)
                    })
                    .take(160)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for object in entries {
                ui.horizontal(|ui| {
                    let selected = self.palette_factory.as_deref() == Some(&object.factory_name);
                    if ui
                        .selectable_label(selected, &object.factory_name)
                        .on_hover_text(format!("{} / {}", object.category, object.class_name))
                        .clicked()
                    {
                        chosen = Some(object.factory_name.clone());
                    }
                    if ui
                        .add_enabled(self.document.is_some(), egui::Button::new("Add"))
                        .clicked()
                    {
                        spawn_now = Some(object.factory_name.clone());
                    }
                });
                ui.small(format!("{}  {}", object.category, object.class_name));
                ui.separator();
            }
        });

        if let Some(factory) = chosen {
            self.palette_factory = Some(factory);
            self.tool = EditorTool::Place;
        }
        if let Some(factory) = spawn_now {
            self.spawn_object_at(factory, self.default_spawn_position());
        }
    }

    fn outliner_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hierarchy");
        let objects: Vec<(String, String, Option<String>)> = self
            .document
            .as_ref()
            .map(|document| {
                document
                    .objects
                    .iter()
                    .map(|object| {
                        (
                            object.id.clone(),
                            object.factory_name.clone(),
                            object.class_name.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.selected_object_id.is_some(),
                    egui::Button::new("Duplicate"),
                )
                .clicked()
            {
                self.duplicate_selected();
            }
            if ui
                .add_enabled(
                    self.selected_object_id.is_some(),
                    egui::Button::new("Delete"),
                )
                .clicked()
            {
                self.delete_selected();
            }
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (id, factory, class_name) in objects {
                let selected = self.selected_object_id.as_deref() == Some(&id);
                if ui.selectable_label(selected, factory).clicked() {
                    self.selected_object_id = Some(id.clone());
                    self.right_tab = RightTab::Inspector;
                }
                ui.small(format!(
                    "{}  {}",
                    id,
                    class_name.unwrap_or_else(|| "Unknown".to_string())
                ));
                ui.separator();
            }
        });
    }

    fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_object().cloned();
        if let Some(object) = selected {
            ui.heading(&object.factory_name);
            ui.label(format!("Id: {}", object.id));
            ui.label(format!(
                "Class: {}",
                object.class_name.as_deref().unwrap_or("Unknown")
            ));
            ui.separator();

            let mut transform = object.transform;
            let mut changed = false;

            ui.label("Translation");
            changed |= vector_drag(ui, &mut transform.translation, 1.0);
            ui.label("Rotation");
            changed |= vector_drag(ui, &mut transform.rotation_degrees, 0.5);
            ui.label("Scale");
            changed |= vector_drag(ui, &mut transform.scale, 0.01);

            if changed {
                if self.snap_enabled {
                    snap_transform(
                        &mut transform,
                        self.snap_translation,
                        self.snap_rotation,
                        self.snap_scale,
                    );
                }
                self.update_selected_transform(transform);
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Frame").clicked() {
                    self.frame_selected();
                }
                if ui.button("Duplicate").clicked() {
                    self.duplicate_selected();
                }
                if ui.button("Delete").clicked() {
                    self.delete_selected();
                }
            });

            ui.separator();
            ui.heading("Params");
            if object.raw_params.is_empty() && object.decoded_params.is_empty() {
                ui.label("No decoded params yet.");
            } else {
                for (key, value) in object.raw_params {
                    ui.label(format!("{key}: {value}"));
                }
                for (key, value) in object.decoded_params {
                    ui.label(format!("{key}: {value:?}"));
                }
            }
        } else {
            ui.heading("Inspector");
            if self.document.is_some() {
                ui.label("No object selected.");
            } else {
                ui.label("No stage open.");
            }
        }
    }

    fn assets_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Assets");
        if let Some(document) = &self.document {
            let scene = RenderScene::from_document(document);
            ui.label(format!(
                "{} scanned assets  {} models  {} collision",
                document.assets.len(),
                scene.model_paths.len(),
                scene.collision_paths.len()
            ));
            if let Some(preview) = &self.model_preview {
                ui.label(format!(
                    "Preview: {} model(s), {} shown point(s), {} source vertex/vertices, {} failed",
                    preview.loaded_models,
                    preview.points.len(),
                    preview.source_vertices,
                    preview.failed_models
                ));
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for asset in document.assets.iter().take(400) {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("{:?}", asset.kind));
                        ui.label(asset.path.display().to_string());
                    });
                }
            });
        } else {
            ui.label("No stage open.");
        }
    }

    fn issues_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Validation");
        if self.issues.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(111, 220, 168), "Clean");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for issue in &self.issues {
                let color = match issue.severity {
                    ValidationSeverity::Info => egui::Color32::from_rgb(150, 180, 220),
                    ValidationSeverity::Warning => egui::Color32::from_rgb(235, 190, 92),
                    ValidationSeverity::Error => egui::Color32::from_rgb(255, 116, 104),
                };
                ui.colored_label(color, format!("{:?} [{}]", issue.severity, issue.code));
                ui.label(&issue.message);
                ui.separator();
            }
        });
    }

    fn console(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Console");
            if ui.button("Clear").clicked() {
                self.log.clear();
            }
        });
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.label(line);
                }
            });
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let size = egui::vec2(available.x.max(240.0), available.y.max(240.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        self.handle_viewport_input(ui, rect, &response);
        self.paint_viewport(ui, &painter, rect);
    }

    fn handle_viewport_input(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
    ) {
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta().y);
            if scroll.abs() > f32::EPSILON {
                let amount = self.renderer.camera().distance * scroll * 0.0015;
                self.dolly_camera(amount);
                self.mark_viewport_interaction(ui);
            }
        }

        let pointer_delta = ui.input(|input| input.pointer.delta());
        let modifiers = ui.input(|input| input.modifiers);
        let secondary_down = ui.input(|input| input.pointer.secondary_down());

        if response.hovered() && ui.input(|input| input.key_pressed(egui::Key::F)) {
            self.frame_selected();
            self.mark_viewport_interaction(ui);
        }

        if secondary_down
            && (response.hovered() || response.dragged_by(egui::PointerButton::Secondary))
            && self.handle_viewport_keyboard_fly(ui)
        {
            self.mark_viewport_interaction(ui);
        }

        if modifiers.alt && response.dragged_by(egui::PointerButton::Primary) {
            self.orbit_camera(pointer_delta);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if modifiers.alt && response.dragged_by(egui::PointerButton::Secondary) {
            let amount =
                self.renderer.camera().distance * (pointer_delta.x - pointer_delta.y) * 0.006;
            self.dolly_camera(amount);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if response.dragged_by(egui::PointerButton::Secondary) {
            self.rotate_camera_in_place(pointer_delta);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if response.dragged_by(egui::PointerButton::Middle)
            || (modifiers.alt && response.dragged_by(egui::PointerButton::Middle))
        {
            self.pan_camera_pixels(rect, pointer_delta);
            if pointer_delta != egui::Vec2::ZERO {
                self.mark_viewport_interaction(ui);
            }
        } else if response.dragged_by(egui::PointerButton::Primary)
            && self.tool == EditorTool::Move
            && pointer_delta != egui::Vec2::ZERO
        {
            self.nudge_selected(self.viewport_drag_move_delta(rect, pointer_delta));
            self.mark_viewport_interaction(ui);
        }

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if self.tool == EditorTool::Place {
                    if let Some(factory) = self.palette_factory.clone() {
                        let world = self.screen_to_world_floor(rect, pos);
                        self.spawn_object_at(factory, world);
                        return;
                    }
                }

                let nearest = self
                    .object_screen_positions(rect)
                    .into_iter()
                    .filter_map(|(id, object_pos, _)| {
                        let dist = object_pos.distance(pos);
                        (dist < 24.0).then_some((dist, id))
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, id)| id);
                self.selected_object_id = nearest;
            }
        }
    }

    fn handle_viewport_keyboard_fly(&mut self, ui: &egui::Ui) -> bool {
        let (dt, forward_key, back_key, left_key, right_key, up_key, down_key, shift, ctrl) = ui
            .input(|input| {
                (
                    input.stable_dt.clamp(1.0 / 240.0, 1.0 / 15.0),
                    input.key_down(egui::Key::W),
                    input.key_down(egui::Key::S),
                    input.key_down(egui::Key::A),
                    input.key_down(egui::Key::D),
                    input.key_down(egui::Key::E),
                    input.key_down(egui::Key::Q),
                    input.modifiers.shift,
                    input.modifiers.ctrl,
                )
            });
        let frame = self.camera_frame();
        let mut move_axis = [0.0, 0.0, 0.0];
        if forward_key {
            move_axis = vec3_add(move_axis, frame.forward);
        }
        if back_key {
            move_axis = vec3_sub(move_axis, frame.forward);
        }
        if right_key {
            move_axis = vec3_add(move_axis, frame.right);
        }
        if left_key {
            move_axis = vec3_sub(move_axis, frame.right);
        }
        if up_key {
            move_axis = vec3_add(move_axis, [0.0, 1.0, 0.0]);
        }
        if down_key {
            move_axis = vec3_sub(move_axis, [0.0, 1.0, 0.0]);
        }

        if vec3_dot(move_axis, move_axis) <= 0.0001 {
            return false;
        }

        let mut speed = self.viewport_fly_speed();
        if shift {
            speed *= 4.0;
        }
        if ctrl {
            speed *= 0.25;
        }
        self.translate_camera(vec3_scale(vec3_normalize(move_axis), speed * dt));
        true
    }

    fn viewport_fly_speed(&self) -> f32 {
        (self.renderer.camera().distance * 0.8).clamp(300.0, 80_000.0) * self.camera_speed
    }

    fn rotate_camera_in_place(&mut self, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let old_position = self.camera_frame().position;
        {
            let camera = self.renderer.camera_mut();
            camera.yaw_degrees -= delta.x * 0.14;
            camera.pitch_degrees = (camera.pitch_degrees - delta.y * 0.12).clamp(-89.0, 89.0);
        }
        let frame = self.camera_frame();
        let distance = self.renderer.camera().distance;
        self.renderer.camera_mut().focus =
            vec3_add(old_position, vec3_scale(frame.forward, distance));
    }

    fn orbit_camera(&mut self, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let camera = self.renderer.camera_mut();
        camera.yaw_degrees -= delta.x * 0.18;
        camera.pitch_degrees = (camera.pitch_degrees - delta.y * 0.14).clamp(-89.0, 89.0);
    }

    fn pan_camera_pixels(&mut self, rect: egui::Rect, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom).max(1.0);
        let units_per_pixel = (self.renderer.camera().distance / focal).max(0.01);
        let world_delta = vec3_add(
            vec3_scale(frame.right, -delta.x * units_per_pixel),
            vec3_scale(frame.up, delta.y * units_per_pixel),
        );
        self.translate_camera(world_delta);
    }

    fn viewport_drag_move_delta(&self, rect: egui::Rect, delta: egui::Vec2) -> [f32; 3] {
        if delta == egui::Vec2::ZERO {
            return [0.0, 0.0, 0.0];
        }
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom).max(1.0);
        let units_per_pixel = (self.renderer.camera().distance / focal).max(0.01);
        let right = vec3_normalize([frame.right[0], 0.0, frame.right[2]]);
        let forward = vec3_normalize([frame.forward[0], 0.0, frame.forward[2]]);
        let forward = if vec3_dot(forward, forward) <= 0.0001 {
            [0.0, 0.0, 1.0]
        } else {
            forward
        };
        vec3_add(
            vec3_scale(right, delta.x * units_per_pixel),
            vec3_scale(forward, -delta.y * units_per_pixel),
        )
    }

    fn dolly_camera(&mut self, amount: f32) {
        if amount.abs() <= f32::EPSILON || !amount.is_finite() {
            return;
        }
        let frame = self.camera_frame();
        self.translate_camera(vec3_scale(frame.forward, amount));
    }

    fn translate_camera(&mut self, delta: [f32; 3]) {
        if !delta.iter().all(|value| value.is_finite()) {
            return;
        }
        let camera = self.renderer.camera_mut();
        camera.focus = vec3_add(camera.focus, delta);
    }

    fn mark_viewport_interaction(&mut self, ui: &egui::Ui) {
        ui.ctx().request_repaint();
    }

    fn paint_viewport(&mut self, ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect) {
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(21, 23, 25));

        let sky = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.center().y));
        painter.rect_filled(sky, 0.0, egui::Color32::from_rgb(30, 42, 48));
        let lower = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.center().y), rect.max);
        painter.rect_filled(lower, 0.0, egui::Color32::from_rgb(18, 24, 26));

        if self.renderer.config().show_grid {
            self.paint_grid(painter, rect);
        }
        if self.model_preview.is_some() {
            self.paint_model_preview(ui.ctx(), painter, rect);
        } else {
            self.paint_stage_silhouette(painter, rect);
        }

        let object_positions = self.object_screen_positions(rect);
        for (id, pos, label) in object_positions {
            let selected = self.selected_object_id.as_deref() == Some(&id);
            if self.view_mode != ViewMode::Objects && !selected {
                continue;
            }
            let color = if selected {
                egui::Color32::from_rgb(255, 214, 102)
            } else {
                egui::Color32::from_rgb(93, 205, 184)
            };
            painter.circle_filled(
                pos + egui::vec2(3.0, 4.0),
                9.0,
                egui::Color32::from_black_alpha(80),
            );
            painter.circle_filled(pos, 8.0, color);
            painter.circle_stroke(
                pos,
                11.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(240, 244, 246)),
            );
            painter.text(
                pos + egui::vec2(14.0, -8.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(232, 236, 238),
            );

            if selected {
                self.paint_gizmo(painter, rect, pos);
            }
        }

        self.paint_viewport_overlays(ui, painter, rect);
    }

    fn paint_grid(&self, painter: &egui::Painter, rect: egui::Rect) {
        let minor = egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(178, 186, 178, 32),
        );
        let major = egui::Stroke::new(
            1.5,
            egui::Color32::from_rgba_unmultiplied(213, 200, 160, 58),
        );

        for i in -10..=10 {
            let v = i as f32 * 500.0;
            let stroke = if i % 5 == 0 { major } else { minor };
            painter.line_segment(
                [
                    self.world_to_screen(rect, [v, 0.0, -5000.0]),
                    self.world_to_screen(rect, [v, 0.0, 5000.0]),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    self.world_to_screen(rect, [-5000.0, 0.0, v]),
                    self.world_to_screen(rect, [5000.0, 0.0, v]),
                ],
                stroke,
            );
        }

        painter.line_segment(
            [
                self.world_to_screen(rect, [-5200.0, 0.0, 0.0]),
                self.world_to_screen(rect, [5200.0, 0.0, 0.0]),
            ],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(206, 82, 82)),
        );
        painter.line_segment(
            [
                self.world_to_screen(rect, [0.0, 0.0, -5200.0]),
                self.world_to_screen(rect, [0.0, 0.0, 5200.0]),
            ],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(82, 168, 110)),
        );
    }

    fn paint_stage_silhouette(&self, painter: &egui::Painter, rect: egui::Rect) {
        let island = vec![
            self.world_to_screen(rect, [-3200.0, -80.0, -2400.0]),
            self.world_to_screen(rect, [1200.0, -80.0, -3000.0]),
            self.world_to_screen(rect, [3400.0, -80.0, -200.0]),
            self.world_to_screen(rect, [2200.0, -80.0, 2600.0]),
            self.world_to_screen(rect, [-2600.0, -80.0, 2800.0]),
            self.world_to_screen(rect, [-3900.0, -80.0, 600.0]),
        ];
        painter.add(egui::Shape::convex_polygon(
            island,
            egui::Color32::from_rgb(76, 84, 66),
            egui::Stroke::new(1.0, egui::Color32::from_rgb(148, 162, 123)),
        ));

        let plaza = vec![
            self.world_to_screen(rect, [-1200.0, 0.0, -900.0]),
            self.world_to_screen(rect, [900.0, 0.0, -900.0]),
            self.world_to_screen(rect, [1200.0, 0.0, 900.0]),
            self.world_to_screen(rect, [-1000.0, 0.0, 1100.0]),
        ];
        painter.add(egui::Shape::convex_polygon(
            plaza,
            egui::Color32::from_rgb(126, 111, 82),
            egui::Stroke::new(1.5, egui::Color32::from_rgb(198, 170, 112)),
        ));
    }

    fn paint_model_preview(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: egui::Rect,
    ) {
        let has_triangles = self
            .model_preview
            .as_ref()
            .is_some_and(|preview| !preview.triangles.is_empty());

        if has_triangles {
            if let (Some(gpu_viewport), Some(frame)) =
                (self.gpu_viewport.as_ref(), self.gpu_viewport_frame(rect))
            {
                gpu_viewport.set_frame(frame);
                painter.add(gpu_viewport.paint_callback(rect));
            } else {
                let key = self.model_framebuffer_key(rect);
                let needs_render =
                    self.model_framebuffer.is_none() || self.model_framebuffer_key != key;
                if needs_render {
                    if let Some(image) = self.render_model_framebuffer(rect) {
                        if let Some(handle) = &mut self.model_framebuffer {
                            handle.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.model_framebuffer = Some(ctx.load_texture(
                                "sms-model-framebuffer",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        self.model_framebuffer_key = key;
                    }
                }

                if let Some(handle) = &self.model_framebuffer {
                    painter.image(
                        handle.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
            }
        } else if let Some(preview) = &self.model_preview {
            self.paint_point_model_preview(painter, rect, preview);
        }

        if self.renderer.config().show_object_bounds {
            if let Some(preview) = &self.model_preview {
                self.paint_preview_bounds(painter, rect, preview);
            }
        }
    }

    fn gpu_viewport_frame(&self, rect: egui::Rect) -> Option<gpu_viewport::GpuViewportFrame> {
        let preview = self.model_preview.as_ref()?;
        let frame = self.camera_frame();
        let focal = perspective_focal_length(rect, self.viewport_zoom);
        let far = preview.far_clip(self.renderer.camera().distance);
        Some(gpu_viewport::GpuViewportFrame {
            camera_position: frame.position,
            right: frame.right,
            up: frame.up,
            forward: frame.forward,
            focal,
            viewport_size: [rect.width().max(1.0), rect.height().max(1.0)],
            viewport_pan: [self.viewport_pan.x, self.viewport_pan.y],
            near: 8.0,
            far,
        })
    }

    fn paint_point_model_preview(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        preview: &ModelPreview,
    ) {
        let stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(91, 190, 173, 95));
        for pair in preview.points.windows(2) {
            if pair[0].model_index != pair[1].model_index {
                continue;
            }

            let a = self.world_to_screen(rect, pair[0].position);
            let b = self.world_to_screen(rect, pair[1].position);
            if rect.expand(80.0).contains(a)
                && rect.expand(80.0).contains(b)
                && a.distance(b) < 90.0
            {
                painter.line_segment([a, b], stroke);
            }
        }

        for point in &preview.points {
            let screen = self.world_to_screen(rect, point.position);
            if rect.expand(4.0).contains(screen) {
                painter.circle_filled(
                    screen,
                    1.4,
                    egui::Color32::from_rgba_unmultiplied(224, 243, 229, 155),
                );
            }
        }
    }

    fn render_model_framebuffer(&self, rect: egui::Rect) -> Option<egui::ColorImage> {
        let preview = self.model_preview.as_ref()?;
        if preview.triangles.is_empty() || rect.width() < 2.0 || rect.height() < 2.0 {
            return None;
        }

        let size = framebuffer_size_for_rect(rect);
        let mut image = viewport_framebuffer_background(size);
        let mut depth = vec![f32::INFINITY; size[0] * size[1]];

        for triangle in preview
            .triangles
            .iter()
            .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Sky)
        {
            if let Some(projected) = self.project_preview_triangle(rect, size, triangle) {
                rasterize_projected_preview_triangle(
                    preview, &mut image, &mut depth, projected, false,
                );
            }
        }

        for triangle in preview.triangles.iter().filter(|triangle| {
            triangle.render_layer != PreviewRenderLayer::Sky
                && !preview_triangle_is_translucent(preview, triangle)
        }) {
            if let Some(projected) = self.project_preview_triangle(rect, size, triangle) {
                rasterize_projected_preview_triangle(
                    preview, &mut image, &mut depth, projected, true,
                );
            }
        }

        let mut translucent: Vec<_> = preview
            .triangles
            .iter()
            .filter(|triangle| {
                triangle.render_layer != PreviewRenderLayer::Sky
                    && preview_triangle_is_translucent(preview, triangle)
            })
            .filter_map(|triangle| self.project_preview_triangle(rect, size, triangle))
            .collect();
        translucent.sort_by(|a, b| b.average_depth.total_cmp(&a.average_depth));
        for projected in translucent {
            rasterize_projected_preview_triangle(preview, &mut image, &mut depth, projected, false);
        }

        Some(image)
    }

    fn model_framebuffer_key(&self, rect: egui::Rect) -> Option<ModelFramebufferKey> {
        let preview = self.model_preview.as_ref()?;
        let camera = self.renderer.camera();
        Some(ModelFramebufferKey {
            stage_id: self.stage_id.clone(),
            size: framebuffer_size_for_rect(rect),
            camera_focus: camera.focus.map(f32::to_bits),
            camera_yaw: camera.yaw_degrees.to_bits(),
            camera_pitch: camera.pitch_degrees.to_bits(),
            camera_distance: camera.distance.to_bits(),
            viewport_pan: [self.viewport_pan.x.to_bits(), self.viewport_pan.y.to_bits()],
            viewport_zoom: self.viewport_zoom.to_bits(),
            triangle_count: preview.triangles.len(),
            texture_count: preview.textures.len(),
            source_triangles: preview.source_triangles,
        })
    }

    fn project_preview_triangle<'a>(
        &self,
        rect: egui::Rect,
        size: [usize; 2],
        triangle: &'a PreviewTriangle,
    ) -> Option<ProjectedPreviewTriangle<'a>> {
        let vertices = preview_triangle_world_vertices(
            triangle.vertices,
            triangle.render_layer,
            self.camera_frame().position,
        );
        let screen = project_triangle_to_framebuffer(self, rect, size, vertices)?;
        if !projected_triangle_overlaps_frame(screen, size) {
            return None;
        }
        if projected_triangle_is_culled(screen, triangle.cull_mode) {
            return None;
        }
        Some(ProjectedPreviewTriangle {
            triangle,
            screen,
            average_depth: (screen[0].depth + screen[1].depth + screen[2].depth) / 3.0,
        })
    }

    fn world_to_framebuffer(
        &self,
        rect: egui::Rect,
        size: [usize; 2],
        point: [f32; 3],
    ) -> Option<ProjectedVertex> {
        let (screen, depth) = self.project_world_to_screen(rect, point)?;
        let x = (screen.x - rect.left()) * size[0] as f32 / rect.width().max(1.0);
        let y = (screen.y - rect.top()) * size[1] as f32 / rect.height().max(1.0);
        Some(ProjectedVertex {
            x,
            y,
            depth,
            inv_depth: 1.0 / depth,
        })
    }

    fn paint_preview_bounds(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        preview: &ModelPreview,
    ) {
        let min = preview.bounds_min;
        let max = preview.bounds_max;
        let corners = [
            [min[0], min[1], min[2]],
            [max[0], min[1], min[2]],
            [max[0], min[1], max[2]],
            [min[0], min[1], max[2]],
            [min[0], max[1], min[2]],
            [max[0], max[1], min[2]],
            [max[0], max[1], max[2]],
            [min[0], max[1], max[2]],
        ];
        let edges = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        let stroke = egui::Stroke::new(
            1.4,
            egui::Color32::from_rgba_unmultiplied(255, 214, 102, 115),
        );
        for (a, b) in edges {
            painter.line_segment(
                [
                    self.world_to_screen(rect, corners[a]),
                    self.world_to_screen(rect, corners[b]),
                ],
                stroke,
            );
        }
    }

    fn paint_gizmo(&self, painter: &egui::Painter, _rect: egui::Rect, origin: egui::Pos2) {
        painter.arrow(
            origin,
            egui::vec2(54.0, 0.0),
            egui::Stroke::new(3.0, egui::Color32::from_rgb(230, 82, 82)),
        );
        painter.arrow(
            origin,
            egui::vec2(-32.0, -34.0),
            egui::Stroke::new(3.0, egui::Color32::from_rgb(82, 176, 116)),
        );
        painter.arrow(
            origin,
            egui::vec2(24.0, -46.0),
            egui::Stroke::new(3.0, egui::Color32::from_rgb(93, 158, 236)),
        );
    }

    fn paint_viewport_overlays(&self, _ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect) {
        let overlay =
            egui::Rect::from_min_size(rect.min + egui::vec2(16.0, 16.0), egui::vec2(280.0, 118.0));
        painter.rect_filled(
            overlay,
            6.0,
            egui::Color32::from_rgba_unmultiplied(12, 15, 16, 210),
        );
        painter.rect_stroke(
            overlay,
            6.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
            ),
            egui::StrokeKind::Inside,
        );

        let document_summary = self
            .document
            .as_ref()
            .map(|document| {
                let scene = RenderScene::from_document(document);
                format!(
                    "{}  models:{}  col:{}  obj:{}",
                    scene.stage_id,
                    scene.model_paths.len(),
                    scene.collision_paths.len(),
                    scene.object_count
                )
            })
            .unwrap_or_else(|| "No stage loaded".to_string());

        painter.text(
            overlay.min + egui::vec2(14.0, 12.0),
            egui::Align2::LEFT_TOP,
            document_summary,
            egui::FontId::proportional(14.0),
            egui::Color32::from_rgb(238, 241, 242),
        );
        painter.text(
            overlay.min + egui::vec2(14.0, 40.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} / {} / snap {}",
                self.tool.label(),
                self.view_mode.label(),
                if self.snap_enabled { "on" } else { "off" }
            ),
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(190, 202, 204),
        );
        let camera = self.renderer.camera();
        painter.text(
            overlay.min + egui::vec2(14.0, 66.0),
            egui::Align2::LEFT_TOP,
            if let Some(preview) = &self.model_preview {
                format!(
                    "verts {}  tris {}  tex {}  pts {}  dist {:.0}",
                    preview.source_vertices,
                    preview.source_triangles,
                    preview.source_textures,
                    preview.points.len(),
                    camera.distance
                )
            } else {
                format!(
                    "yaw {:.0}  pitch {:.0}  dist {:.0}",
                    camera.yaw_degrees, camera.pitch_degrees, camera.distance
                )
            },
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(190, 202, 204),
        );

        let compass_center = egui::pos2(rect.right() - 70.0, rect.top() + 74.0);
        painter.circle_filled(
            compass_center,
            42.0,
            egui::Color32::from_rgba_unmultiplied(10, 13, 14, 190),
        );
        painter.circle_stroke(
            compass_center,
            42.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
            ),
        );
        painter.text(
            compass_center + egui::vec2(0.0, -31.0),
            egui::Align2::CENTER_CENTER,
            "N",
            egui::FontId::monospace(12.0),
            egui::Color32::from_rgb(235, 220, 160),
        );
        painter.arrow(
            compass_center,
            egui::vec2(0.0, -24.0),
            egui::Stroke::new(2.0, egui::Color32::from_rgb(235, 220, 160)),
        );
        painter.arrow(
            compass_center,
            egui::vec2(23.0, 10.0),
            egui::Stroke::new(2.0, egui::Color32::from_rgb(93, 158, 236)),
        );

        if self.palette_factory.is_some() && self.tool == EditorTool::Place {
            let text = self.palette_factory.as_deref().unwrap_or_default();
            let rect = egui::Rect::from_min_size(
                rect.left_bottom() + egui::vec2(16.0, -48.0),
                egui::vec2(260.0, 34.0),
            );
            painter.rect_filled(
                rect,
                5.0,
                egui::Color32::from_rgba_unmultiplied(24, 36, 34, 220),
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("Placing {text}"),
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(215, 238, 231),
            );
        }
    }

    fn generate_schema(&mut self) {
        match SchemaGenerator::new(&self.repo_root).generate() {
            Ok(registry) => {
                self.log.push(format!(
                    "Generated {} object entries.",
                    registry.objects.len()
                ));
                self.registry = Some(registry);
            }
            Err(err) => self.log.push(format!("Schema generation failed: {err}")),
        }
    }

    fn refresh_scene_browser_if_needed(&mut self) {
        let base_root = self.base_root.trim();
        if base_root.is_empty() || self.last_scanned_base_root == base_root {
            return;
        }

        if PathBuf::from(base_root).exists() {
            let should_open = self.document.is_none();
            self.scan_scenes();
            if should_open && !self.stage_id.trim().is_empty() {
                self.open_stage();
            }
        }
    }

    fn scan_scenes(&mut self) {
        let base_root = self.base_root.trim().to_string();
        if base_root.is_empty() {
            self.log
                .push("Base root is required for scene scan.".to_string());
            return;
        }

        match discover_scene_archives(PathBuf::from(&base_root)) {
            Ok(archives) => {
                let count = archives.len();
                self.scene_archives = archives;
                self.last_scanned_base_root = base_root;
                self.log
                    .push(format!("Discovered {count} scene archive(s)."));

                if self.stage_id.trim().is_empty() {
                    if let Some(first) = self.scene_archives.first() {
                        self.stage_id = first.stage_id.clone();
                    }
                }
            }
            Err(err) => self.log.push(format!("Scene scan failed: {err}")),
        }
    }

    fn open_scene_archive(&mut self, archive: SceneArchiveInfo) {
        self.stage_id = archive.stage_id.clone();
        self.log
            .push(format!("Selected scene '{}'.", archive.stage_id));
        self.open_stage();
    }

    fn open_stage(&mut self) {
        if self.registry.is_none() {
            self.generate_schema();
        }
        if self.scene_archives.is_empty() && !self.base_root.trim().is_empty() {
            self.scan_scenes();
        }

        match StageDocument::open(PathBuf::from(&self.base_root), self.stage_id.clone()) {
            Ok(document) => {
                let document = if let Some(registry) = self.registry.clone() {
                    document.with_registry(registry)
                } else {
                    document
                };
                let scene = RenderScene::from_document(&document);
                let model_preview = self.build_model_preview(&document);
                self.log.push(format!(
                    "Opened stage '{}' with {} asset(s), {} model(s), {} collision file(s).",
                    document.stage_id,
                    document.assets.len(),
                    scene.model_paths.len(),
                    scene.collision_paths.len()
                ));
                if let Some(preview) = &model_preview {
                    self.log.push(format!(
                        "Viewport preview loaded {} model(s), {} sampled point(s), {} triangle(s), {} texture(s), {} source vertex/vertices.",
                        preview.loaded_models,
                        preview.points.len(),
                        preview.triangles.len(),
                        preview.textures.len(),
                        preview.source_vertices
                    ));
                } else if !scene.model_paths.is_empty() {
                    self.log
                        .push("Viewport preview could not decode BMD vertex data.".to_string());
                }
                self.issues = document.validate();
                self.document = Some(document);
                self.model_preview = model_preview;
                self.rebuild_gpu_viewport_scene();
                self.clear_viewport_preview_cache();
                self.selected_object_id = None;
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.reset_camera();
                self.apply_startup_camera_focus();
            }
            Err(err) => self.log.push(format!("Open stage failed: {err}")),
        }
    }

    fn build_model_preview(&self, document: &StageDocument) -> Option<ModelPreview> {
        const POINT_BUDGET: usize = 16_000;
        const POINTS_PER_MODEL: usize = 1_200;
        const POINTS_PER_OBJECT_INSTANCE: usize = 500;

        let models: Vec<_> = document
            .assets
            .iter()
            .filter(|asset| asset.kind == StageAssetKind::Model)
            .collect();
        let preferred: Vec<_> = models
            .iter()
            .copied()
            .filter(|asset| {
                let path = asset.path.to_string_lossy().replace('\\', "/");
                is_default_preview_model_path(
                    &path,
                    self.show_environment_meshes,
                    self.show_goop_meshes,
                    self.show_effect_meshes,
                )
            })
            .collect();
        let models = if preferred.is_empty() {
            models
        } else {
            preferred
        };

        let mut points = Vec::new();
        let mut triangles = Vec::new();
        let mut textures = Vec::new();
        let mut materials = Vec::new();
        let mut next_packet_index = 0usize;
        let mut bounds_min = [f32::INFINITY; 3];
        let mut bounds_max = [f32::NEG_INFINITY; 3];
        let mut camera_bounds_min = [f32::INFINITY; 3];
        let mut camera_bounds_max = [f32::NEG_INFINITY; 3];
        let mut sky_radius = 0.0f32;
        let mut camera_bound_points = Vec::new();
        let mut loaded_models = 0;
        let mut failed_models = 0;
        let mut source_vertices = 0;
        let mut source_triangles = 0;
        let mut source_textures = 0;
        let mut object_model_indices = BTreeMap::new();

        for asset in models {
            let asset_path = asset.path.to_string_lossy().replace('\\', "/");
            let include_in_camera_bounds = is_camera_bounds_model_path(&asset_path);
            let model_render_layer = preview_render_layer_for_model_path(&asset_path);
            let is_sky = model_render_layer == PreviewRenderLayer::Sky;
            let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
                failed_models += 1;
                continue;
            };
            let Ok(file) = J3dFile::parse(&bytes) else {
                failed_models += 1;
                continue;
            };

            let loader_flags = model_loader_flags_for_path(&asset_path);
            match file.geometry_preview_with_loader_flags(loader_flags) {
                Ok(mut preview) => {
                    apply_model_material_table(document, &asset_path, loader_flags, &mut preview);
                    loaded_models += 1;
                    let model_index = loaded_models;
                    let texture_base = push_preview_textures(&mut textures, &preview);
                    let material_base =
                        push_preview_materials(&mut materials, &preview, texture_base);
                    let packet_base = next_packet_index;
                    next_packet_index += preview
                        .triangles
                        .iter()
                        .map(|triangle| triangle.packet_index)
                        .max()
                        .map(|index| index + 1)
                        .unwrap_or(1);
                    source_vertices += preview.positions.len();
                    source_triangles += preview.triangles.len();
                    source_textures += preview.textures.len();
                    if is_sky {
                        sky_radius = sky_radius.max(max_distance_from_origin(&preview.positions));
                    } else {
                        merge_bounds(
                            &mut bounds_min,
                            &mut bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                    }
                    if include_in_camera_bounds {
                        merge_bounds(
                            &mut camera_bounds_min,
                            &mut camera_bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                        camera_bound_points.extend(preview.positions.iter().copied());
                    }

                    if !is_sky {
                        let point_stride = (preview.positions.len() / POINTS_PER_MODEL).max(1);
                        for position in preview.positions.iter().step_by(point_stride) {
                            points.push(PreviewPoint {
                                position: *position,
                                model_index,
                            });
                        }
                    }

                    for triangle in &preview.triangles {
                        if triangle_vertices_are_finite(triangle.vertices) {
                            triangles.push(PreviewTriangle {
                                vertices: triangle.vertices,
                                normals: triangle.normals,
                                color_channels: triangle.color_channels,
                                tex_coord_sets: triangle.tex_coord_sets,
                                material_index: triangle.material_index.and_then(|index| {
                                    let global_index = material_base + index;
                                    (global_index < materials.len()).then_some(global_index)
                                }),
                                packet_index: packet_base + triangle.packet_index,
                                model_index,
                                render_layer: model_render_layer,
                                color: triangle.color,
                                vertex_colors: triangle.vertex_colors,
                                combine_mode: triangle.combine_mode,
                                tex_coords: triangle.tex_coords,
                                texture_index: triangle.texture_index.and_then(|index| {
                                    let global_index = texture_base + index;
                                    (global_index < textures.len()).then_some(global_index)
                                }),
                                mask_tex_coords: triangle.mask_tex_coords,
                                mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                                    let global_index = texture_base + index;
                                    (global_index < textures.len()).then_some(global_index)
                                }),
                                cull_mode: triangle.cull_mode,
                                alpha_compare: triangle.alpha_compare,
                                blend_mode: triangle.blend_mode,
                                z_mode: triangle.z_mode,
                            });
                        }
                    }
                }
                Err(_) => {
                    let Ok(preview) = file.vertex_preview() else {
                        failed_models += 1;
                        continue;
                    };

                    loaded_models += 1;
                    let model_index = loaded_models;
                    source_vertices += preview.positions.len();
                    if is_sky {
                        sky_radius = sky_radius.max(max_distance_from_origin(&preview.positions));
                    } else {
                        merge_bounds(
                            &mut bounds_min,
                            &mut bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                    }
                    if include_in_camera_bounds {
                        merge_bounds(
                            &mut camera_bounds_min,
                            &mut camera_bounds_max,
                            preview.bounds_min,
                            preview.bounds_max,
                        );
                        camera_bound_points.extend(preview.positions.iter().copied());
                    }

                    if !is_sky {
                        let stride = (preview.positions.len() / POINTS_PER_MODEL).max(1);
                        for position in preview.positions.iter().step_by(stride) {
                            points.push(PreviewPoint {
                                position: *position,
                                model_index,
                            });
                        }
                    }
                }
            }
        }

        let mut object_model_cache = BTreeMap::<String, CachedObjectModelPreview>::new();
        for object in &document.objects {
            if !should_render_object_model(object) {
                continue;
            }
            let Some(model_path) = object
                .asset_hints
                .iter()
                .find(|hint| hint.role == AssetRole::PreviewModel)
                .map(|hint| hint.path.clone())
            else {
                continue;
            };
            if !is_supported_object_preview_model_path(&model_path) {
                continue;
            }

            if !object_model_cache.contains_key(&model_path) {
                let Ok(bytes) = read_stage_asset_bytes(&model_path) else {
                    failed_models += 1;
                    continue;
                };
                let Ok(file) = J3dFile::parse(&bytes) else {
                    failed_models += 1;
                    continue;
                };
                let loader_flags = model_loader_flags_for_path(&model_path);
                let Ok(mut preview) = file.geometry_preview_with_loader_flags(loader_flags) else {
                    failed_models += 1;
                    continue;
                };
                apply_model_material_table(document, &model_path, loader_flags, &mut preview);
                let texture_base = push_preview_textures(&mut textures, &preview);
                let material_base = push_preview_materials(&mut materials, &preview, texture_base);
                object_model_cache.insert(
                    model_path.clone(),
                    CachedObjectModelPreview {
                        preview,
                        texture_base,
                        material_base,
                    },
                );
            }

            let Some(cached) = object_model_cache.get(&model_path) else {
                continue;
            };
            let object_material_base =
                push_object_preview_materials(&mut materials, cached, object);
            loaded_models += 1;
            let model_index = loaded_models;
            object_model_indices.insert(object.id.clone(), model_index);
            source_vertices += cached.preview.positions.len();
            source_triangles += cached.preview.triangles.len();
            source_textures += cached.preview.textures.len();
            let packet_base = next_packet_index;
            next_packet_index += cached
                .preview
                .triangles
                .iter()
                .map(|triangle| triangle.packet_index)
                .max()
                .map(|index| index + 1)
                .unwrap_or(1);

            let transformed_bounds_min =
                transform_preview_point(cached.preview.bounds_min, object.transform);
            let transformed_bounds_max =
                transform_preview_point(cached.preview.bounds_max, object.transform);
            merge_bounds(
                &mut bounds_min,
                &mut bounds_max,
                [
                    transformed_bounds_min[0].min(transformed_bounds_max[0]),
                    transformed_bounds_min[1].min(transformed_bounds_max[1]),
                    transformed_bounds_min[2].min(transformed_bounds_max[2]),
                ],
                [
                    transformed_bounds_min[0].max(transformed_bounds_max[0]),
                    transformed_bounds_min[1].max(transformed_bounds_max[1]),
                    transformed_bounds_min[2].max(transformed_bounds_max[2]),
                ],
            );

            let point_stride = (cached.preview.positions.len() / POINTS_PER_OBJECT_INSTANCE).max(1);
            for position in cached.preview.positions.iter().step_by(point_stride) {
                points.push(PreviewPoint {
                    position: transform_preview_point(*position, object.transform),
                    model_index,
                });
            }

            for triangle in &cached.preview.triangles {
                let vertices = transform_preview_vertices(triangle.vertices, object.transform);
                if triangle_vertices_are_finite(vertices) {
                    triangles.push(PreviewTriangle {
                        vertices,
                        normals: triangle
                            .normals
                            .map(|normals| transform_preview_normals(normals, object.transform)),
                        color_channels: triangle.color_channels,
                        tex_coord_sets: triangle.tex_coord_sets,
                        material_index: triangle.material_index.and_then(|index| {
                            let global_index = object_material_base + index;
                            (global_index < materials.len()).then_some(global_index)
                        }),
                        packet_index: packet_base + triangle.packet_index,
                        model_index,
                        render_layer: PreviewRenderLayer::Main,
                        color: triangle.color,
                        vertex_colors: triangle.vertex_colors,
                        combine_mode: triangle.combine_mode,
                        tex_coords: triangle.tex_coords,
                        texture_index: triangle.texture_index.and_then(|index| {
                            let global_index = cached.texture_base + index;
                            (global_index < textures.len()).then_some(global_index)
                        }),
                        mask_tex_coords: triangle.mask_tex_coords,
                        mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                            let global_index = cached.texture_base + index;
                            (global_index < textures.len()).then_some(global_index)
                        }),
                        cull_mode: triangle.cull_mode,
                        alpha_compare: triangle.alpha_compare,
                        blend_mode: triangle.blend_mode,
                        z_mode: triangle.z_mode,
                    });
                }
            }
        }

        if points.len() > POINT_BUDGET {
            let stride = (points.len() / POINT_BUDGET).max(1);
            points = points
                .into_iter()
                .step_by(stride)
                .take(POINT_BUDGET)
                .collect();
        }

        if loaded_models == 0 || (points.is_empty() && triangles.is_empty()) {
            return None;
        }

        if !bounds_are_finite(bounds_min, bounds_max) {
            if let Some((robust_min, robust_max)) = robust_preview_bounds(&triangles, &points) {
                bounds_min = robust_min;
                bounds_max = robust_max;
            }
        }

        if !bounds_are_finite(camera_bounds_min, camera_bounds_max) {
            camera_bounds_min = bounds_min;
            camera_bounds_max = bounds_max;
        }
        if let Some((robust_min, robust_max)) = robust_position_bounds(&camera_bound_points) {
            camera_bounds_min = robust_min;
            camera_bounds_max = robust_max;
        }

        Some(ModelPreview {
            points,
            triangles,
            textures,
            materials,
            bounds_min,
            bounds_max,
            camera_bounds_min,
            camera_bounds_max,
            sky_radius,
            loaded_models,
            failed_models,
            source_vertices,
            source_triangles,
            source_textures,
            object_model_indices,
        })
    }

    fn validate(&mut self) {
        if let Some(document) = &self.document {
            self.issues = document.validate();
            self.log.push(format!(
                "Validation produced {} issue(s).",
                self.issues.len()
            ));
            for issue in &self.issues {
                self.log.push(format!(
                    "{:?} [{}] {}",
                    issue.severity, issue.code, issue.message
                ));
            }
        } else {
            self.log.push("No stage open.".to_string());
        }
    }

    fn export_mod(&mut self) {
        if let Some(document) = &mut self.document {
            if let Err(err) = document.queue_editor_overlay_change() {
                self.log.push(format!("Scene overlay export failed: {err}"));
                return;
            }
        }

        match &self.document {
            Some(document) => match document.save_to_mod_folder(PathBuf::from(&self.mod_root)) {
                Ok(manifest) => self.log.push(format!(
                    "Exported manifest with {} changed file(s).",
                    manifest.changed_files.len()
                )),
                Err(err) => self.log.push(format!("Export failed: {err}")),
            },
            None => self.log.push("No stage open.".to_string()),
        }
    }

    fn launch_dolphin(&mut self) {
        if self.dolphin_path.trim().is_empty() || self.game_path.trim().is_empty() {
            self.log
                .push("Dolphin executable and game path are required.".to_string());
            return;
        }

        let mut command = Command::new(&self.dolphin_path);
        if !self.dolphin_user_dir.trim().is_empty() {
            command.arg("-u").arg(&self.dolphin_user_dir);
        }
        command.arg("-b").arg("-e").arg(&self.game_path);

        match command.spawn() {
            Ok(_) => self.log.push("Launched Dolphin.".to_string()),
            Err(err) => self.log.push(format!("Failed to launch Dolphin: {err}")),
        }
    }

    fn spawn_object_at(&mut self, factory_name: String, translation: [f32; 3]) {
        let id = format!(
            "{}-obj-{:04}",
            sanitize_id(&self.stage_id),
            self.next_object_serial
        );
        self.next_object_serial += 1;

        let mut object = SceneObject::new(id.clone(), factory_name.clone());
        object.transform.translation = translation;
        if let Some(schema) = self
            .registry
            .as_ref()
            .and_then(|registry| registry.find_object(&factory_name))
        {
            object.class_name = Some(schema.class_name.clone());
            if let Some(model) = &schema.preview_model {
                object.asset_hints.push(AssetRef {
                    path: model.clone(),
                    role: AssetRole::PreviewModel,
                });
            }
        }

        self.mutate_document("Added object", |document| document.add_object(object));
        self.selected_object_id = Some(id);
        self.rebuild_model_preview_from_document();
    }

    fn duplicate_selected(&mut self) {
        let Some(source) = self.selected_object().cloned() else {
            return;
        };
        let id = format!(
            "{}-obj-{:04}",
            sanitize_id(&self.stage_id),
            self.next_object_serial
        );
        self.next_object_serial += 1;

        let mut clone = source;
        clone.id = id.clone();
        clone.transform.translation[0] += self.snap_translation.max(25.0);
        clone.transform.translation[2] += self.snap_translation.max(25.0);
        self.mutate_document("Duplicated object", |document| document.add_object(clone));
        self.selected_object_id = Some(id);
        self.rebuild_model_preview_from_document();
    }

    fn delete_selected(&mut self) {
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        self.mutate_document("Deleted object", |document| {
            document.objects.retain(|object| object.id != selected_id);
        });
        self.selected_object_id = None;
        self.rebuild_model_preview_from_document();
    }

    fn update_selected_transform(&mut self, transform: Transform) {
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        let Some(old_transform) = self.selected_object().map(|object| object.transform) else {
            return;
        };
        if old_transform == transform {
            return;
        }
        self.mutate_document("Updated transform", |document| {
            if let Some(object) = document
                .objects
                .iter_mut()
                .find(|object| object.id == selected_id)
            {
                object.transform = transform;
            }
        });
        if self.update_object_preview_transform(&selected_id, old_transform, transform) {
            self.rebuild_gpu_viewport_scene();
            self.clear_viewport_preview_cache();
        } else {
            self.rebuild_model_preview_from_document();
        }
    }

    fn nudge_selected(&mut self, delta: [f32; 3]) {
        let Some(mut object) = self.selected_object().cloned() else {
            return;
        };
        for (value, add) in object.transform.translation.iter_mut().zip(delta) {
            *value += add;
        }
        if self.snap_enabled {
            snap_transform(
                &mut object.transform,
                self.snap_translation,
                self.snap_rotation,
                self.snap_scale,
            );
        }
        self.update_selected_transform(object.transform);
    }

    fn mutate_document(&mut self, label: &str, mutate: impl FnOnce(&mut StageDocument)) {
        self.push_undo_snapshot();
        if let Some(document) = &mut self.document {
            mutate(document);
            if let Err(err) = document.queue_editor_overlay_change() {
                self.log.push(format!("Scene overlay update failed: {err}"));
            }
            self.issues = document.validate();
            self.log.push(format!("{label}."));
        }
    }

    fn push_undo_snapshot(&mut self) {
        if let Some(document) = &self.document {
            self.undo_stack.push(document.objects.clone());
            if self.undo_stack.len() > 80 {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
    }

    fn undo(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(document.objects.clone());
        document.objects = previous;
        if let Err(err) = document.queue_editor_overlay_change() {
            self.log.push(format!("Scene overlay update failed: {err}"));
        }
        self.issues = document.validate();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Undo.".to_string());
    }

    fn redo(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(document.objects.clone());
        document.objects = next;
        if let Err(err) = document.queue_editor_overlay_change() {
            self.log.push(format!("Scene overlay update failed: {err}"));
        }
        self.issues = document.validate();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Redo.".to_string());
    }

    fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn selected_object(&self) -> Option<&SceneObject> {
        let selected_id = self.selected_object_id.as_ref()?;
        self.document
            .as_ref()?
            .objects
            .iter()
            .find(|object| &object.id == selected_id)
    }

    fn clear_viewport_preview_cache(&mut self) {
        self.model_framebuffer = None;
        self.model_framebuffer_key = None;
    }

    fn rebuild_model_preview_from_document(&mut self) {
        self.model_preview = self
            .document
            .as_ref()
            .and_then(|document| self.build_model_preview(document));
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
    }

    fn update_object_preview_transform(
        &mut self,
        object_id: &str,
        old_transform: Transform,
        new_transform: Transform,
    ) -> bool {
        if !transform_has_invertible_scale(old_transform) {
            return false;
        }
        let Some(preview) = self.model_preview.as_mut() else {
            return false;
        };
        let Some(model_index) = preview.object_model_indices.get(object_id).copied() else {
            return false;
        };

        let mut changed = false;
        for point in &mut preview.points {
            if point.model_index == model_index {
                point.position =
                    retransform_preview_point(point.position, old_transform, new_transform);
                changed = true;
            }
        }
        for triangle in &mut preview.triangles {
            if triangle.model_index == model_index {
                triangle.vertices = triangle
                    .vertices
                    .map(|vertex| retransform_preview_point(vertex, old_transform, new_transform));
                triangle.normals = triangle.normals.map(|normals| {
                    normals.map(|normal| {
                        retransform_preview_normal(normal, old_transform, new_transform)
                    })
                });
                changed = true;
            }
        }
        if changed {
            recompute_model_preview_bounds(preview);
        }
        changed
    }

    fn rebuild_gpu_viewport_scene(&mut self) {
        let Some(target_format) = self.gpu_target_format else {
            self.gpu_viewport = None;
            return;
        };
        let Some(preview) = self.model_preview.as_ref() else {
            self.gpu_viewport = None;
            return;
        };
        self.gpu_viewport = Some(gpu_viewport::GpuViewportScene::from_preview(
            preview,
            target_format,
        ));
    }

    fn ensure_selection_exists(&mut self) {
        let exists = self.selected_object_id.as_ref().is_some_and(|id| {
            self.document
                .as_ref()
                .is_some_and(|document| document.objects.iter().any(|object| &object.id == id))
        });
        if !exists {
            self.selected_object_id = None;
        }
    }

    fn default_spawn_position(&self) -> [f32; 3] {
        self.renderer.camera().focus
    }

    fn frame_selected(&mut self) {
        if let Some(object) = self.selected_object() {
            self.renderer.camera_mut().focus = object.transform.translation;
            self.viewport_pan = egui::Vec2::ZERO;
        }
    }

    fn reset_camera(&mut self) {
        self.viewport_pan = egui::Vec2::ZERO;
        self.viewport_zoom = 1.0;
        if let Some(preview) = &self.model_preview {
            let camera = self.renderer.camera_mut();
            camera.focus = preview.center();
            camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
            camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
            camera.distance = (preview.radius() * 4.2).clamp(2500.0, 600_000.0);
            return;
        }

        let camera = self.renderer.camera_mut();
        camera.focus = [0.0, 0.0, 0.0];
        camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
        camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
        camera.distance = 7000.0;
    }

    fn apply_startup_camera_focus(&mut self) {
        if let Some(focus) = self.startup_camera_focus {
            let camera = self.renderer.camera_mut();
            camera.focus = focus;
            if let Some(distance) = self.startup_camera_distance {
                camera.distance = distance.max(50.0);
            }
            self.viewport_pan = egui::Vec2::ZERO;
            self.viewport_zoom = 1.0;
            self.log.push(format!(
                "Focused startup camera on {:.1}, {:.1}, {:.1}.",
                focus[0], focus[1], focus[2]
            ));
            return;
        }
        let Some(needle) = self.startup_focus_object.as_deref() else {
            return;
        };
        let Some(document) = &self.document else {
            return;
        };
        let needle = needle.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return;
        }
        if let Some(object) = document
            .objects
            .iter()
            .find(|object| object_matches_focus(object, &needle))
        {
            let camera = self.renderer.camera_mut();
            camera.focus = object.transform.translation;
            camera.distance = self.startup_camera_distance.unwrap_or(2200.0).max(50.0);
            self.viewport_pan = egui::Vec2::ZERO;
            self.viewport_zoom = 1.0;
            self.selected_object_id = Some(object.id.clone());
            self.log.push(format!(
                "Focused startup camera on '{}'.",
                object_display_name(object)
            ));
        }
    }

    fn issue_counts(&self) -> (usize, usize) {
        let warnings = self
            .issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Warning)
            .count();
        let errors = self
            .issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
            .count();
        (warnings, errors)
    }

    fn object_screen_positions(&self, rect: egui::Rect) -> Vec<(String, egui::Pos2, String)> {
        self.document
            .as_ref()
            .map(|document| {
                document
                    .objects
                    .iter()
                    .map(|object| {
                        (
                            object.id.clone(),
                            self.world_to_screen(rect, object.transform.translation),
                            object.factory_name.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn world_to_screen(&self, rect: egui::Rect, point: [f32; 3]) -> egui::Pos2 {
        self.project_world_to_screen(rect, point)
            .map(|(screen, _)| screen)
            .unwrap_or(rect.center() + self.viewport_pan)
    }

    fn project_world_to_screen(
        &self,
        rect: egui::Rect,
        point: [f32; 3],
    ) -> Option<(egui::Pos2, f32)> {
        let frame = self.camera_frame();
        let rel = vec3_sub(point, frame.position);
        let depth = vec3_dot(rel, frame.forward);
        if depth <= 8.0 || !depth.is_finite() {
            return None;
        }

        let focal = perspective_focal_length(rect, self.viewport_zoom);
        let x = vec3_dot(rel, frame.right) / depth * focal;
        let y = vec3_dot(rel, frame.up) / depth * focal;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }

        Some((rect.center() + self.viewport_pan + egui::vec2(x, -y), depth))
    }

    fn camera_frame(&self) -> CameraFrame {
        let camera = self.renderer.camera();
        let yaw = camera.yaw_degrees.to_radians();
        let pitch = camera.pitch_degrees.to_radians();
        let forward = vec3_normalize([
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        ]);
        let right = vec3_normalize([-yaw.cos(), 0.0, yaw.sin()]);
        let up = vec3_normalize(vec3_cross(right, forward));
        let position = vec3_sub(camera.focus, vec3_scale(forward, camera.distance));
        CameraFrame {
            position,
            right,
            up,
            forward,
        }
    }

    fn screen_to_world_floor(&self, rect: egui::Rect, pos: egui::Pos2) -> [f32; 3] {
        let frame = self.camera_frame();
        let floor_y = self.renderer.camera().focus[1];
        let focal = perspective_focal_length(rect, self.viewport_zoom);
        let local = pos - rect.center() - self.viewport_pan;
        let ray = vec3_normalize(vec3_add(
            frame.forward,
            vec3_add(
                vec3_scale(frame.right, local.x / focal),
                vec3_scale(frame.up, -local.y / focal),
            ),
        ));
        if ray[1].abs() < 0.0001 {
            return self.renderer.camera().focus;
        }
        let t = (floor_y - frame.position[1]) / ray[1];
        if !t.is_finite() || t <= 0.0 {
            return self.renderer.camera().focus;
        }

        vec3_add(frame.position, vec3_scale(ray, t))
    }
}

fn install_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(28, 30, 31);
    visuals.window_fill = egui::Color32::from_rgb(32, 34, 35);
    visuals.faint_bg_color = egui::Color32::from_rgb(38, 41, 42);
    visuals.extreme_bg_color = egui::Color32::from_rgb(18, 20, 21);
    visuals.selection.bg_fill = egui::Color32::from_rgb(54, 124, 116);
    visuals.hyperlink_color = egui::Color32::from_rgb(104, 186, 214);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.indent = 16.0;
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn command_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).strong())
            .fill(egui::Color32::from_rgb(48, 56, 54)),
    )
}

fn push_preview_textures(
    textures: &mut Vec<PreviewTexture>,
    preview: &J3dGeometryPreview,
) -> usize {
    let texture_base = textures.len();
    for texture in &preview.textures {
        let expected_len = texture.width as usize * texture.height as usize * 4;
        if texture.rgba.len() == expected_len && expected_len > 0 {
            let has_alpha = texture.rgba.chunks_exact(4).any(|pixel| pixel[3] < 245);
            let has_translucent_alpha = texture
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 12 && pixel[3] < 245);
            let mut mips = texture
                .mips
                .iter()
                .filter_map(|mip| {
                    let expected_len = mip.width as usize * mip.height as usize * 4;
                    (mip.rgba.len() == expected_len && expected_len > 0).then(|| {
                        egui::ColorImage::from_rgba_unmultiplied(
                            [mip.width as usize, mip.height as usize],
                            &mip.rgba,
                        )
                    })
                })
                .collect::<Vec<_>>();
            if mips.is_empty() {
                mips.push(egui::ColorImage::from_rgba_unmultiplied(
                    [texture.width as usize, texture.height as usize],
                    &texture.rgba,
                ));
            }
            textures.push(PreviewTexture {
                image: mips[0].clone(),
                mips,
                format: texture.format,
                wrap_s: texture.wrap_s,
                wrap_t: texture.wrap_t,
                min_filter: texture.min_filter,
                mag_filter: texture.mag_filter,
                mipmap_count: texture.mipmap_count,
                has_alpha,
                has_translucent_alpha,
            });
        } else {
            let image = egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
            textures.push(PreviewTexture {
                image: image.clone(),
                mips: vec![image],
                format: texture.format,
                wrap_s: texture.wrap_s,
                wrap_t: texture.wrap_t,
                min_filter: texture.min_filter,
                mag_filter: texture.mag_filter,
                mipmap_count: texture.mipmap_count,
                has_alpha: false,
                has_translucent_alpha: false,
            });
        }
    }
    texture_base
}

fn push_preview_materials(
    materials: &mut Vec<J3dMaterial>,
    preview: &J3dGeometryPreview,
    texture_base: usize,
) -> usize {
    let material_base = materials.len();
    for material in &preview.materials {
        let mut material = material.clone();
        material.material_index = materials.len();
        for index in material.texture_indices.iter_mut().flatten() {
            *index += texture_base;
        }
        materials.push(material);
    }
    material_base
}

fn apply_model_material_table(
    document: &StageDocument,
    model_path: &str,
    loader_flags: u32,
    preview: &mut J3dGeometryPreview,
) {
    let candidates = material_table_candidates_for_model(model_path);
    let Some(table_path) = document.assets.iter().find_map(|asset| {
        if asset.kind != StageAssetKind::MaterialTable {
            return None;
        }
        let normalized = asset
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        candidates
            .iter()
            .any(|candidate| candidate == &normalized)
            .then_some(asset.path.clone())
    }) else {
        return;
    };
    let Ok(bytes) = read_stage_asset_bytes(&table_path) else {
        return;
    };
    let Ok(table) = J3dFile::parse(&bytes) else {
        return;
    };
    let Ok(materials) = table.material_programs_with_loader_flags(loader_flags) else {
        return;
    };
    let Ok(textures) = table.texture_previews() else {
        return;
    };
    let required_materials = preview
        .triangles
        .iter()
        .filter_map(|triangle| triangle.material_index)
        .max()
        .map(|index| index + 1)
        .unwrap_or(0);
    if materials.len() < required_materials
        || materials
            .iter()
            .flat_map(|material| material.texture_indices)
            .flatten()
            .any(|index| index >= textures.len())
    {
        return;
    }

    preview.materials = materials;
    preview.textures = textures;
    for triangle in &mut preview.triangles {
        let Some(material) = triangle
            .material_index
            .and_then(|index| preview.materials.get(index))
        else {
            continue;
        };
        triangle.texture_index = material.texture_indices.into_iter().flatten().next();
        triangle.cull_mode = Some(material.cull_mode);
        triangle.alpha_compare = Some(material.alpha_compare);
        triangle.blend_mode = Some(material.blend_mode);
        triangle.z_mode = Some(material.z_mode);
        triangle.z_comp_loc = Some(material.z_comp_loc);
    }
}

fn material_table_candidates_for_model(model_path: &str) -> Vec<String> {
    let normalized = model_path.replace('\\', "/").to_ascii_lowercase();
    let Some(base) = normalized.rsplit_once('.').map(|(base, _)| base) else {
        return Vec::new();
    };
    let mut candidates = vec![format!("{base}.bmt")];
    let (directory, stem) = base
        .rsplit_once('/')
        .map(|(directory, stem)| (format!("{directory}/"), stem))
        .unwrap_or_else(|| (String::new(), base));
    for suffix in ["_crash", "crash", "_alpha", "alpha"] {
        if let Some(base_stem) = stem.strip_suffix(suffix) {
            if !base_stem.is_empty() {
                candidates.push(format!("{directory}{base_stem}.bmt"));
            }
            break;
        }
    }
    candidates
}

fn push_object_preview_materials(
    materials: &mut Vec<J3dMaterial>,
    cached: &CachedObjectModelPreview,
    object: &SceneObject,
) -> usize {
    let Some(tev_reg1) = nozzle_box_tev_reg1_color(object) else {
        return cached.material_base;
    };
    let source_end = cached.material_base + cached.preview.materials.len();
    let source_materials = materials[cached.material_base..source_end].to_vec();
    let material_base = materials.len();
    for mut material in source_materials {
        material.material_index = materials.len();
        material.tev_colors[1] = tev_reg1;
        materials.push(material);
    }
    material_base
}

fn nozzle_box_tev_reg1_color(object: &SceneObject) -> Option<[i16; 4]> {
    let is_nozzle_box = object.factory_name.eq_ignore_ascii_case("NozzleBox")
        || object
            .class_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("NozzleBox"));
    if !is_nozzle_box {
        return None;
    }

    let nozzle_item = object
        .raw_params
        .values()
        .map(|value| value.to_ascii_lowercase())
        .find(|value| value.ends_with("_nozzle_item"));
    Some(match nozzle_item.as_deref() {
        Some("normal_nozzle_item") => [0, 0, 255, 100],
        Some("rocket_nozzle_item") => [255, 0, 0, 100],
        Some("back_nozzle_item") => [90, 90, 120, 100],
        _ => [255, 255, 255, 100],
    })
}

fn transform_preview_vertices(vertices: [[f32; 3]; 3], transform: Transform) -> [[f32; 3]; 3] {
    vertices.map(|vertex| transform_preview_point(vertex, transform))
}

fn transform_preview_normals(normals: [[f32; 3]; 3], transform: Transform) -> [[f32; 3]; 3] {
    normals.map(|normal| transform_preview_normal(normal, transform))
}

fn transform_preview_normal(mut normal: [f32; 3], transform: Transform) -> [f32; 3] {
    for (component, scale) in normal.iter_mut().zip(transform.scale) {
        if scale.abs() > 0.00001 {
            *component /= scale;
        }
    }
    normal = rotate_x_degrees(normal, transform.rotation_degrees[0]);
    normal = rotate_y_degrees(normal, transform.rotation_degrees[1]);
    normal = rotate_z_degrees(normal, transform.rotation_degrees[2]);
    vec3_normalize(normal)
}

fn transform_preview_point(mut point: [f32; 3], transform: Transform) -> [f32; 3] {
    point[0] *= transform.scale[0];
    point[1] *= transform.scale[1];
    point[2] *= transform.scale[2];

    point = rotate_x_degrees(point, transform.rotation_degrees[0]);
    point = rotate_y_degrees(point, transform.rotation_degrees[1]);
    point = rotate_z_degrees(point, transform.rotation_degrees[2]);

    [
        point[0] + transform.translation[0],
        point[1] + transform.translation[1],
        point[2] + transform.translation[2],
    ]
}

fn retransform_preview_point(
    point: [f32; 3],
    old_transform: Transform,
    new_transform: Transform,
) -> [f32; 3] {
    transform_preview_point(
        inverse_transform_preview_point(point, old_transform),
        new_transform,
    )
}

fn retransform_preview_normal(
    normal: [f32; 3],
    old_transform: Transform,
    new_transform: Transform,
) -> [f32; 3] {
    transform_preview_normal(
        inverse_transform_preview_normal(normal, old_transform),
        new_transform,
    )
}

fn inverse_transform_preview_normal(mut normal: [f32; 3], transform: Transform) -> [f32; 3] {
    normal = rotate_z_degrees(normal, -transform.rotation_degrees[2]);
    normal = rotate_y_degrees(normal, -transform.rotation_degrees[1]);
    normal = rotate_x_degrees(normal, -transform.rotation_degrees[0]);
    for (component, scale) in normal.iter_mut().zip(transform.scale) {
        *component *= scale;
    }
    vec3_normalize(normal)
}

fn inverse_transform_preview_point(mut point: [f32; 3], transform: Transform) -> [f32; 3] {
    point[0] -= transform.translation[0];
    point[1] -= transform.translation[1];
    point[2] -= transform.translation[2];

    point = rotate_z_degrees(point, -transform.rotation_degrees[2]);
    point = rotate_y_degrees(point, -transform.rotation_degrees[1]);
    point = rotate_x_degrees(point, -transform.rotation_degrees[0]);

    [
        point[0] / transform.scale[0],
        point[1] / transform.scale[1],
        point[2] / transform.scale[2],
    ]
}

fn transform_has_invertible_scale(transform: Transform) -> bool {
    transform
        .scale
        .iter()
        .all(|value| value.is_finite() && value.abs() > 0.00001)
}

fn rotate_x_degrees(point: [f32; 3], degrees: f32) -> [f32; 3] {
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        point[0],
        point[1] * cos - point[2] * sin,
        point[1] * sin + point[2] * cos,
    ]
}

fn rotate_y_degrees(point: [f32; 3], degrees: f32) -> [f32; 3] {
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        point[0] * cos + point[2] * sin,
        point[1],
        -point[0] * sin + point[2] * cos,
    ]
}

fn rotate_z_degrees(point: [f32; 3], degrees: f32) -> [f32; 3] {
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        point[0] * cos - point[1] * sin,
        point[0] * sin + point[1] * cos,
        point[2],
    ]
}

fn is_supported_object_preview_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("!/mapobj/") || path.contains("/scene/mapobj/")
}

fn should_render_object_model(object: &SceneObject) -> bool {
    let factory = object.factory_name.to_ascii_lowercase();
    let class_name = object
        .class_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let placement_name = object
        .raw_params
        .get("name")
        .map(String::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    !(factory == "palmleaf" || class_name == "palmleaf" || placement_name.starts_with("palmleaf"))
}

fn object_matches_focus(object: &SceneObject, needle: &str) -> bool {
    object.id.to_ascii_lowercase().contains(needle)
        || object.factory_name.to_ascii_lowercase().contains(needle)
        || object
            .class_name
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(needle)
        || object.raw_params.values().any(|value| {
            value
                .trim_matches('"')
                .to_ascii_lowercase()
                .contains(needle)
        })
}

fn object_display_name(object: &SceneObject) -> String {
    object
        .raw_params
        .get("name")
        .cloned()
        .unwrap_or_else(|| object.factory_name.clone())
}

fn labeled_text(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
    });
}

fn default_base_root() -> String {
    if let Ok(path) = std::env::var("SMS_BASE_ROOT") {
        if PathBuf::from(&path).exists() {
            return path;
        }
    }

    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let candidate = PathBuf::from(user_profile)
            .join("Downloads")
            .join("SunshineJPExtract");
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    String::new()
}

fn default_repo_root() -> String {
    if let Ok(path) = std::env::var("SMS_REPO_ROOT") {
        if sms_repo_marker_exists(&PathBuf::from(&path)) {
            return path;
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        if let Some(root) = find_sms_repo_root(&current_dir) {
            return root;
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            if let Some(root) = find_sms_repo_root(parent) {
                return root;
            }
        }
    }

    "..".to_string()
}

fn find_sms_repo_root(start: &std::path::Path) -> Option<String> {
    start
        .ancestors()
        .find(|candidate| sms_repo_marker_exists(candidate))
        .map(|candidate| candidate.to_string_lossy().to_string())
}

fn sms_repo_marker_exists(path: &std::path::Path) -> bool {
    path.join("src")
        .join("System")
        .join("MarNameRefGen.cpp")
        .exists()
}

#[derive(Debug, Default)]
struct EditorStartupArgs {
    repo_root: Option<String>,
    base_root: Option<String>,
    stage_id: Option<String>,
    focus_object: Option<String>,
    camera_focus: Option<[f32; 3]>,
    camera_distance: Option<f32>,
    camera_yaw: Option<f32>,
    camera_pitch: Option<f32>,
}

fn editor_startup_args() -> EditorStartupArgs {
    let mut parsed = EditorStartupArgs::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo-root" => parsed.repo_root = args.next(),
            "--base-root" => parsed.base_root = args.next(),
            "--stage" | "--stage-id" => parsed.stage_id = args.next(),
            "--focus-object" => parsed.focus_object = args.next(),
            "--camera-focus" => {
                if let Some(value) = args.next() {
                    parsed.camera_focus = parse_vec3_arg(&value).or_else(|| {
                        let x = value.parse().ok()?;
                        let y = args.next()?.parse().ok()?;
                        let z = args.next()?.parse().ok()?;
                        Some([x, y, z])
                    });
                }
            }
            "--camera-distance" => {
                parsed.camera_distance = args.next().and_then(|value| value.parse().ok())
            }
            "--camera-yaw" => parsed.camera_yaw = args.next().and_then(|value| value.parse().ok()),
            "--camera-pitch" => {
                parsed.camera_pitch = args.next().and_then(|value| value.parse().ok())
            }
            _ => {}
        }
    }

    parsed
}

fn parse_vec3_arg(value: &str) -> Option<[f32; 3]> {
    let mut parts = value.split(',').map(str::trim);
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let z = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some([x, y, z])
}

fn format_bytes_short(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn merge_bounds(
    bounds_min: &mut [f32; 3],
    bounds_max: &mut [f32; 3],
    next_min: [f32; 3],
    next_max: [f32; 3],
) {
    for axis in 0..3 {
        bounds_min[axis] = bounds_min[axis].min(next_min[axis]);
        bounds_max[axis] = bounds_max[axis].max(next_max[axis]);
    }
}

fn max_distance_from_origin(points: &[[f32; 3]]) -> f32 {
    points
        .iter()
        .filter(|point| point.iter().all(|value| value.is_finite()))
        .map(|point| vec3_dot(*point, *point).sqrt())
        .fold(0.0, f32::max)
}

fn preview_triangle_world_vertices(
    vertices: [[f32; 3]; 3],
    render_layer: PreviewRenderLayer,
    camera_position: [f32; 3],
) -> [[f32; 3]; 3] {
    if render_layer == PreviewRenderLayer::Sky {
        vertices.map(|vertex| vec3_add(vertex, camera_position))
    } else {
        vertices
    }
}

fn recompute_model_preview_bounds(preview: &mut ModelPreview) {
    if let Some((bounds_min, bounds_max)) =
        robust_preview_bounds(&preview.triangles, &preview.points)
    {
        preview.bounds_min = bounds_min;
        preview.bounds_max = bounds_max;
        if !bounds_are_finite(preview.camera_bounds_min, preview.camera_bounds_max) {
            preview.camera_bounds_min = bounds_min;
            preview.camera_bounds_max = bounds_max;
        }
    }
}

fn bounds_are_finite(bounds_min: [f32; 3], bounds_max: [f32; 3]) -> bool {
    bounds_min
        .iter()
        .chain(bounds_max.iter())
        .all(|value| value.is_finite())
        && (0..3).all(|axis| bounds_max[axis] > bounds_min[axis])
}

fn triangle_vertices_are_finite(vertices: [[f32; 3]; 3]) -> bool {
    vertices
        .iter()
        .flatten()
        .all(|value| value.is_finite() && value.abs() < 2_000_000.0)
}

fn robust_preview_bounds(
    triangles: &[PreviewTriangle],
    points: &[PreviewPoint],
) -> Option<([f32; 3], [f32; 3])> {
    let mut axes = [Vec::new(), Vec::new(), Vec::new()];
    let has_world_triangles = triangles
        .iter()
        .any(|triangle| triangle.render_layer != PreviewRenderLayer::Sky);
    if !has_world_triangles {
        for point in points {
            for (axis, value) in axes.iter_mut().zip(point.position) {
                if value.is_finite() {
                    axis.push(value);
                }
            }
        }
    } else {
        for triangle in triangles
            .iter()
            .filter(|triangle| triangle.render_layer != PreviewRenderLayer::Sky)
        {
            for vertex in triangle.vertices {
                for (axis, value) in axes.iter_mut().zip(vertex) {
                    if value.is_finite() {
                        axis.push(value);
                    }
                }
            }
        }
    }

    robust_axes_bounds(axes)
}

fn robust_position_bounds(points: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
    let mut axes = [Vec::new(), Vec::new(), Vec::new()];
    for point in points {
        for axis in 0..3 {
            if point[axis].is_finite() {
                axes[axis].push(point[axis]);
            }
        }
    }

    robust_axes_bounds(axes)
}

fn robust_axes_bounds(mut axes: [Vec<f32>; 3]) -> Option<([f32; 3], [f32; 3])> {
    if axes[0].len() < 16 || axes.iter().any(Vec::is_empty) {
        return None;
    }

    let mut min = [0.0; 3];
    let mut max = [0.0; 3];
    for axis in 0..3 {
        axes[axis].sort_by(|a, b| a.total_cmp(b));
        let trim = (axes[axis].len() / 20).min(axes[axis].len().saturating_sub(1));
        let high = axes[axis].len().saturating_sub(1 + trim);
        min[axis] = axes[axis][trim];
        max[axis] = axes[axis][high];
        if !min[axis].is_finite() || !max[axis].is_finite() || max[axis] <= min[axis] {
            return None;
        }
    }

    for axis in 0..3 {
        let pad = ((max[axis] - min[axis]) * 0.06).max(120.0);
        min[axis] -= pad;
        max[axis] += pad;
    }

    Some((min, max))
}

fn perspective_focal_length(rect: egui::Rect, viewport_zoom: f32) -> f32 {
    const VERTICAL_FOV_DEGREES: f32 = 50.0;
    let fov = VERTICAL_FOV_DEGREES.to_radians();
    rect.height().min(rect.width()) * 0.5 / (fov * 0.5).tan() * viewport_zoom.max(0.05)
}

fn vec3_add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn vec3_sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn vec3_scale(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn vec3_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn vec3_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn vec3_normalize(value: [f32; 3]) -> [f32; 3] {
    let length = vec3_dot(value, value).sqrt();
    if length <= 0.0001 || !length.is_finite() {
        [0.0, 0.0, 0.0]
    } else {
        vec3_scale(value, 1.0 / length)
    }
}

fn framebuffer_size_for_rect(rect: egui::Rect) -> [usize; 2] {
    let max_side: f32 = 1280.0;
    let width = rect.width().max(1.0);
    let height = rect.height().max(1.0);
    let scale = (max_side / width.max(height)).min(1.0);
    [
        (width * scale).round().clamp(1.0, max_side) as usize,
        (height * scale).round().clamp(1.0, max_side) as usize,
    ]
}

fn viewport_framebuffer_background(size: [usize; 2]) -> egui::ColorImage {
    let width = size[0].max(1);
    let height = size[1].max(1);
    let mut image = egui::ColorImage::filled(size, egui::Color32::from_rgb(18, 24, 26));
    let top = [35.0, 47.0, 55.0];
    let horizon = [23.0, 36.0, 40.0];
    let lower = [18.0, 24.0, 26.0];
    let denom = height.saturating_sub(1).max(1) as f32;

    for y in 0..height {
        let t = y as f32 / denom;
        let color = if t < 0.48 {
            lerp_rgb(top, horizon, t / 0.48)
        } else {
            lerp_rgb(horizon, lower, (t - 0.48) / 0.52)
        };
        for x in 0..width {
            image.pixels[y * width + x] = egui::Color32::from_rgb(color[0], color[1], color[2]);
        }
    }

    image
}

fn lerp_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] + (b[0] - a[0]) * t).clamp(0.0, 255.0) as u8,
        (a[1] + (b[1] - a[1]) * t).clamp(0.0, 255.0) as u8,
        (a[2] + (b[2] - a[2]) * t).clamp(0.0, 255.0) as u8,
    ]
}

fn project_triangle_to_framebuffer(
    app: &SmsEditorApp,
    rect: egui::Rect,
    size: [usize; 2],
    vertices: [[f32; 3]; 3],
) -> Option<[ProjectedVertex; 3]> {
    Some([
        app.world_to_framebuffer(rect, size, vertices[0])?,
        app.world_to_framebuffer(rect, size, vertices[1])?,
        app.world_to_framebuffer(rect, size, vertices[2])?,
    ])
}

fn projected_triangle_overlaps_frame(vertices: [ProjectedVertex; 3], size: [usize; 2]) -> bool {
    if vertices
        .iter()
        .any(|vertex| !vertex.x.is_finite() || !vertex.y.is_finite() || !vertex.depth.is_finite())
    {
        return false;
    }

    let max_x = size[0] as f32;
    let max_y = size[1] as f32;
    let min_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::INFINITY, f32::min);
    let max_tri_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::INFINITY, f32::min);
    let max_tri_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::NEG_INFINITY, f32::max);

    min_x < max_x && max_tri_x >= 0.0 && min_y < max_y && max_tri_y >= 0.0
}

fn projected_triangle_is_culled(vertices: [ProjectedVertex; 3], cull_mode: Option<u8>) -> bool {
    let Some(cull_mode) = cull_mode else {
        return false;
    };
    match cull_mode {
        0 => false,
        1 => edge_function(vertices[0], vertices[1], vertices[2]) < 0.0,
        2 => edge_function(vertices[0], vertices[1], vertices[2]) > 0.0,
        3 => true,
        _ => false,
    }
}

fn rasterize_projected_preview_triangle(
    preview: &ModelPreview,
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    projected: ProjectedPreviewTriangle<'_>,
    write_depth: bool,
) {
    let triangle = projected.triangle;
    let normal = preview_triangle_normal(triangle);
    let average_y = triangle
        .vertices
        .iter()
        .map(|vertex| vertex[1])
        .sum::<f32>()
        / 3.0;
    let alpha_test_fallback =
        triangle.alpha_compare.is_none() && preview_triangle_uses_alpha_test(preview, triangle);

    if let (Some(texture_index), Some(tex_coords)) = (triangle.texture_index, triangle.tex_coords) {
        if let Some(texture) = preview.textures.get(texture_index) {
            let mask_texture = triangle
                .mask_texture_index
                .and_then(|index| preview.textures.get(index))
                .zip(triangle.mask_tex_coords);
            rasterize_preview_triangle(
                image,
                depth,
                projected.screen,
                Some((texture, tex_coords)),
                mask_texture,
                preview_texture_tints(
                    triangle.color,
                    triangle.vertex_colors,
                    triangle.combine_mode,
                    triangle.render_layer,
                ),
                write_depth,
                triangle.alpha_compare,
                alpha_test_fallback,
            );
            return;
        }
    }

    rasterize_preview_triangle(
        image,
        depth,
        projected.screen,
        None,
        None,
        preview_solid_triangle_colors(triangle, normal, average_y),
        write_depth,
        triangle.alpha_compare,
        alpha_test_fallback,
    );
}

fn preview_triangle_is_translucent(preview: &ModelPreview, triangle: &PreviewTriangle) -> bool {
    if triangle.render_layer == PreviewRenderLayer::Water {
        return true;
    }
    if preview_triangle_uses_alpha_test(preview, triangle) {
        return false;
    }
    if triangle.render_layer == PreviewRenderLayer::Goop {
        return true;
    }

    let has_alpha_source = preview_triangle_has_alpha_source(preview, triangle);
    if !has_alpha_source {
        return false;
    }

    triangle
        .blend_mode
        .map(|blend| blend.mode != 0)
        .unwrap_or(true)
}

fn preview_triangle_uses_alpha_test(preview: &ModelPreview, triangle: &PreviewTriangle) -> bool {
    if triangle.render_layer == PreviewRenderLayer::Water {
        return false;
    }

    if triangle
        .alpha_compare
        .is_some_and(alpha_compare_can_discard)
    {
        return true;
    }

    (triangle
        .texture_index
        .and_then(|index| preview.textures.get(index))
        .is_some_and(|texture| texture.has_alpha)
        && (triangle.blend_mode.is_none_or(|blend| blend.mode == 0)
            || triangle
                .texture_index
                .and_then(|index| preview.textures.get(index))
                .is_some_and(|texture| !texture.has_translucent_alpha)))
        || triangle.mask_texture_index.is_some()
}

fn preview_triangle_has_alpha_source(preview: &ModelPreview, triangle: &PreviewTriangle) -> bool {
    let material_alpha = triangle.color.is_some_and(|color| {
        triangle.texture_index.is_none()
            && color[3] < 245
            && matches!(
                triangle.combine_mode,
                J3dPreviewCombineMode::TextureModulateMaterial
                    | J3dPreviewCombineMode::MaterialOnly
            )
    });
    let vertex_alpha = triangle.vertex_colors.is_some_and(|colors| {
        colors.iter().any(|color| color[3] < 245)
            && matches!(
                triangle.combine_mode,
                J3dPreviewCombineMode::TextureModulateVertex | J3dPreviewCombineMode::VertexOnly
            )
    });
    let texture_alpha = triangle
        .texture_index
        .and_then(|index| preview.textures.get(index))
        .is_some_and(|texture| texture.has_translucent_alpha);
    let mask_alpha = triangle.mask_texture_index.is_some();

    material_alpha || vertex_alpha || texture_alpha || mask_alpha
}

fn alpha_compare_can_discard(compare: J3dAlphaCompare) -> bool {
    (0..=255).any(|alpha| !alpha_compare_passes(compare, alpha))
}

fn alpha_compare_passes(compare: J3dAlphaCompare, alpha: u8) -> bool {
    let a = alpha as i16;
    let pass0 = alpha_compare_op_passes(compare.comp0, a, compare.ref0 as i16);
    let pass1 = alpha_compare_op_passes(compare.comp1, a, compare.ref1 as i16);
    match compare.op {
        0 => pass0 && pass1,
        1 => pass0 || pass1,
        2 => pass0 ^ pass1,
        3 => pass0 == pass1,
        _ => pass0 && pass1,
    }
}

fn alpha_compare_op_passes(compare: u8, alpha: i16, reference: i16) -> bool {
    match compare {
        0 => false,
        1 => alpha < reference,
        2 => alpha == reference,
        3 => alpha <= reference,
        4 => alpha > reference,
        5 => alpha != reference,
        6 => alpha >= reference,
        7 => true,
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_preview_triangle(
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    vertices: [ProjectedVertex; 3],
    texture: Option<(&PreviewTexture, [[f32; 2]; 3])>,
    mask_texture: Option<(&PreviewTexture, [[f32; 2]; 3])>,
    tints: [egui::Color32; 3],
    write_depth: bool,
    alpha_compare: Option<J3dAlphaCompare>,
    alpha_test_fallback: bool,
) {
    let area = edge_function(vertices[0], vertices[1], vertices[2]);
    if !area.is_finite() || area.abs() < 0.5 {
        return;
    }

    let width = image.size[0];
    let height = image.size[1];
    let min_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x = vertices
        .iter()
        .map(|vertex| vertex.x)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min((width.saturating_sub(1)) as f32) as usize;
    let min_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_y = vertices
        .iter()
        .map(|vertex| vertex.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min((height.saturating_sub(1)) as f32) as usize;

    if min_x > max_x || min_y > max_y {
        return;
    }

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let weights = [
                edge_function_point(vertices[1], vertices[2], px, py) / area,
                edge_function_point(vertices[2], vertices[0], px, py) / area,
                edge_function_point(vertices[0], vertices[1], px, py) / area,
            ];
            if weights.iter().any(|weight| *weight < -0.0001) {
                continue;
            }

            let Some(corrected_weights) = perspective_correct_weights(vertices, weights) else {
                continue;
            };
            let pixel_depth = perspective_correct_depth(vertices, weights);
            let index = y * width + x;
            if pixel_depth >= depth[index] {
                continue;
            }

            let color = if let Some((texture, tex_coords)) = texture {
                let uv = [
                    corrected_weights[0] * tex_coords[0][0]
                        + corrected_weights[1] * tex_coords[1][0]
                        + corrected_weights[2] * tex_coords[2][0],
                    corrected_weights[0] * tex_coords[0][1]
                        + corrected_weights[1] * tex_coords[1][1]
                        + corrected_weights[2] * tex_coords[2][1],
                ];
                let texture_color = sample_preview_texture(texture, uv);
                let tint = interpolate_color(tints, corrected_weights);
                combine_texture_and_tint(texture, texture_color, tint)
            } else {
                interpolate_color(tints, corrected_weights)
            };
            let color = if let Some((mask_texture, mask_tex_coords)) = mask_texture {
                let uv = [
                    corrected_weights[0] * mask_tex_coords[0][0]
                        + corrected_weights[1] * mask_tex_coords[1][0]
                        + corrected_weights[2] * mask_tex_coords[2][0],
                    corrected_weights[0] * mask_tex_coords[0][1]
                        + corrected_weights[1] * mask_tex_coords[1][1]
                        + corrected_weights[2] * mask_tex_coords[2][1],
                ];
                let mask_color = sample_preview_texture(mask_texture, uv);
                let mask_alpha = (mask_color[0] + mask_color[1] + mask_color[2]) / 3.0;
                [color[0], color[1], color[2], color[3] * mask_alpha]
            } else {
                color
            };

            if let Some(compare) = alpha_compare {
                let alpha = (color[3].clamp(0.0, 1.0) * 255.0) as u8;
                if !alpha_compare_passes(compare, alpha) {
                    continue;
                }
            } else if color[3] < (if alpha_test_fallback { 0.28 } else { 0.12 }) {
                continue;
            }
            let color = software_output_color_for_pass(color, write_depth);
            blend_depth_pixel(image, depth, index, pixel_depth, color, write_depth);
        }
    }
}

fn software_output_color_for_pass(color: [f32; 4], write_depth: bool) -> [f32; 4] {
    if write_depth {
        [color[0], color[1], color[2], 1.0]
    } else {
        color
    }
}

fn edge_function(a: ProjectedVertex, b: ProjectedVertex, c: ProjectedVertex) -> f32 {
    edge_function_point(a, b, c.x, c.y)
}

fn edge_function_point(a: ProjectedVertex, b: ProjectedVertex, x: f32, y: f32) -> f32 {
    (x - a.x) * (b.y - a.y) - (y - a.y) * (b.x - a.x)
}

fn perspective_correct_weights(
    vertices: [ProjectedVertex; 3],
    weights: [f32; 3],
) -> Option<[f32; 3]> {
    let weighted_inv_depth = [
        weights[0] * vertices[0].inv_depth,
        weights[1] * vertices[1].inv_depth,
        weights[2] * vertices[2].inv_depth,
    ];
    let sum = weighted_inv_depth[0] + weighted_inv_depth[1] + weighted_inv_depth[2];
    if !sum.is_finite() || sum.abs() <= f32::EPSILON {
        return None;
    }
    Some([
        weighted_inv_depth[0] / sum,
        weighted_inv_depth[1] / sum,
        weighted_inv_depth[2] / sum,
    ])
}

fn perspective_correct_depth(vertices: [ProjectedVertex; 3], weights: [f32; 3]) -> f32 {
    let inv_depth = weights[0] * vertices[0].inv_depth
        + weights[1] * vertices[1].inv_depth
        + weights[2] * vertices[2].inv_depth;
    if inv_depth > 0.0 && inv_depth.is_finite() {
        1.0 / inv_depth
    } else {
        f32::INFINITY
    }
}

fn sample_preview_texture(texture: &PreviewTexture, uv: [f32; 2]) -> [f32; 4] {
    let width = texture.image.size[0].max(1);
    let height = texture.image.size[1].max(1);
    let u = wrap_texture_coord(uv[0], texture.wrap_s);
    let v = wrap_texture_coord(uv[1], texture.wrap_t);
    let x = u * (width.saturating_sub(1)) as f32;
    let y = v * (height.saturating_sub(1)) as f32;
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let c00 = color32_to_rgba(texture.image.pixels[y0 * width + x0]);
    let c10 = color32_to_rgba(texture.image.pixels[y0 * width + x1]);
    let c01 = color32_to_rgba(texture.image.pixels[y1 * width + x0]);
    let c11 = color32_to_rgba(texture.image.pixels[y1 * width + x1]);
    let top = lerp_rgba(c00, c10, tx);
    let bottom = lerp_rgba(c01, c11, tx);
    lerp_rgba(top, bottom, ty)
}

fn interpolate_color(colors: [egui::Color32; 3], weights: [f32; 3]) -> [f32; 4] {
    let colors = colors.map(color32_to_rgba);
    [
        weights[0] * colors[0][0] + weights[1] * colors[1][0] + weights[2] * colors[2][0],
        weights[0] * colors[0][1] + weights[1] * colors[1][1] + weights[2] * colors[2][1],
        weights[0] * colors[0][2] + weights[1] * colors[1][2] + weights[2] * colors[2][2],
        weights[0] * colors[0][3] + weights[1] * colors[1][3] + weights[2] * colors[2][3],
    ]
}

fn multiply_rgba(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

fn combine_texture_and_tint(
    texture: &PreviewTexture,
    texture_color: [f32; 4],
    tint: [f32; 4],
) -> [f32; 4] {
    let color = multiply_rgba(texture_color, tint);
    let tint_is_dark = tint[0].max(tint[1]).max(tint[2]) < 0.35;
    let intensity_mask = matches!(texture.format, 0 | 1) && !texture.has_alpha;
    if intensity_mask && tint_is_dark && tint[3] < 0.98 {
        let intensity = (texture_color[0] + texture_color[1] + texture_color[2]) / 3.0;
        return [tint[0], tint[1], tint[2], intensity * tint[3]];
    }

    color
}

fn lerp_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

fn color32_to_rgba(color: egui::Color32) -> [f32; 4] {
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        alpha as f32 / 255.0,
    ]
}

fn rgba_to_color32(color: [f32; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
        (color[3].clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn blend_depth_pixel(
    image: &mut egui::ColorImage,
    depth: &mut [f32],
    index: usize,
    pixel_depth: f32,
    src: [f32; 4],
    write_depth: bool,
) {
    let alpha = src[3].clamp(0.0, 1.0);
    if alpha >= 0.98 {
        image.pixels[index] = rgba_to_color32(src);
        if write_depth {
            depth[index] = pixel_depth;
        }
        return;
    }

    let dst = color32_to_rgba(image.pixels[index]);
    let out_alpha = alpha + dst[3] * (1.0 - alpha);
    if out_alpha <= 0.001 {
        return;
    }
    let out = [
        (src[0] * alpha + dst[0] * dst[3] * (1.0 - alpha)) / out_alpha,
        (src[1] * alpha + dst[1] * dst[3] * (1.0 - alpha)) / out_alpha,
        (src[2] * alpha + dst[2] * dst[3] * (1.0 - alpha)) / out_alpha,
        out_alpha,
    ];
    image.pixels[index] = rgba_to_color32(out);
    if write_depth && alpha >= 0.65 {
        depth[index] = pixel_depth;
    }
}

fn preview_triangle_normal(triangle: &PreviewTriangle) -> [f32; 3] {
    if let Some(normals) = triangle.normals {
        return vec3_normalize([
            (normals[0][0] + normals[1][0] + normals[2][0]) / 3.0,
            (normals[0][1] + normals[1][1] + normals[2][1]) / 3.0,
            (normals[0][2] + normals[1][2] + normals[2][2]) / 3.0,
        ]);
    }
    triangle_normal(triangle.vertices)
}

fn triangle_normal(vertices: [[f32; 3]; 3]) -> [f32; 3] {
    let ab = [
        vertices[1][0] - vertices[0][0],
        vertices[1][1] - vertices[0][1],
        vertices[1][2] - vertices[0][2],
    ];
    let ac = [
        vertices[2][0] - vertices[0][0],
        vertices[2][1] - vertices[0][1],
        vertices[2][2] - vertices[0][2],
    ];
    let normal = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2])
        .sqrt()
        .max(0.0001);
    [normal[0] / length, normal[1] / length, normal[2] / length]
}

fn wrap_texture_coord(value: f32, wrap: u8) -> f32 {
    match wrap {
        0 => value.clamp(0.0, 1.0),
        2 => {
            let value = value.rem_euclid(2.0);
            if value > 1.0 {
                2.0 - value
            } else {
                value
            }
        }
        _ => value.rem_euclid(1.0),
    }
}

fn is_default_preview_model_path(
    path: &str,
    show_environment_meshes: bool,
    show_goop_meshes: bool,
    show_effect_meshes: bool,
) -> bool {
    let path = path.to_ascii_lowercase();
    if !(path.contains("!/map/") || path.contains("/scene/map/")) {
        return false;
    }
    if path_is_sky_model_path(&path) {
        return true;
    }
    if path_is_indirect_water_model_path(&path) {
        return false;
    }
    if path_is_water_model_path(&path) {
        return show_environment_meshes;
    }
    if path_is_goop_model_path(&path) {
        return show_goop_meshes;
    }
    if path.contains("/map/mirror/") {
        return show_effect_meshes;
    }
    if path.contains("/map/map/reflect") || path.contains("/map/reflect") {
        return show_effect_meshes;
    }
    true
}

fn is_camera_bounds_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    if !(path.contains("!/map/") || path.contains("/scene/map/")) {
        return false;
    }
    !(path_is_sky_model_path(&path)
        || path_is_water_model_path(&path)
        || path_is_indirect_water_model_path(&path)
        || path_is_goop_model_path(&path)
        || path.contains("/map/mirror/")
        || path.contains("/map/map/reflect")
        || path.contains("/map/reflect"))
}

fn preview_render_layer_for_model_path(path: &str) -> PreviewRenderLayer {
    let path = path.to_ascii_lowercase();
    if path_is_sky_model_path(&path) {
        PreviewRenderLayer::Sky
    } else if path_is_goop_model_path(&path) {
        PreviewRenderLayer::Goop
    } else if path_is_water_model_path(&path) {
        PreviewRenderLayer::Water
    } else {
        PreviewRenderLayer::Main
    }
}

fn path_is_sky_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.ends_with("/map/map/sky.bmd") || path.ends_with("/map/map/sky.bdl")
}

fn path_is_water_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    if path_is_indirect_water_model_path(&path) {
        return false;
    }
    path.contains("/map/map/sea")
        || path.contains("/map/map/water")
        || path.contains("/map/water/")
        || path.contains("/map/mirror/puddle")
        || path.contains("/map/map/puddle")
        || path.contains("/map/map/yogan")
        || path.contains("/map/map/lava")
}

fn path_is_indirect_water_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("seaindirect") || path.contains("puddle_ind")
}

fn path_is_goop_model_path(path: &str) -> bool {
    path.contains("/map/pollution/") || path.contains("pollution")
}

fn model_loader_flags_for_path(path: &str) -> u32 {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let model_name = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".bmd")
        .trim_end_matches(".bdl");

    match model_name {
        // TSky::load uses SMS_MakeMActorWithAnmData(..., 0x10220000).
        "sky" if path_is_sky_model_path(&path) => SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
        // TMapStaticObj::actor_data_table in MapStaticObject.cpp.
        "seaindirect" => 0x1121_0000,
        "sea" => 0x1022_0000,
        "riccosea" => 0x1021_0000,
        name if name.starts_with("riccoseapollution") => 0x1121_0000,
        _ if path_is_goop_model_path(&path) => SMS_POLLUTION_MODEL_LOAD_FLAGS,
        _ if path.contains("/mapobj/") || path.contains("/scene/mapobj/") => {
            // TMapObjBase::makeMActors uses this unless the object's indirect flag is set.
            SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS
        }
        _ => SMS_MAP_MODEL_LOAD_FLAGS,
    }
}

fn preview_texture_tints(
    material_color: Option<[u8; 4]>,
    vertex_colors: Option<[[u8; 4]; 3]>,
    combine_mode: J3dPreviewCombineMode,
    render_layer: PreviewRenderLayer,
) -> [egui::Color32; 3] {
    let useful_material = material_color.filter(|color| preview_color_is_useful(*color));
    let base = match combine_mode {
        J3dPreviewCombineMode::TextureModulateMaterial => useful_material
            .map(material_color_rgb_tint)
            .unwrap_or([255, 255, 255, 255]),
        _ => [255, 255, 255, 255],
    };
    std::array::from_fn(|index| {
        let vertex = if combine_mode == J3dPreviewCombineMode::TextureModulateVertex {
            vertex_colors
                .map(|colors| colors[index])
                .filter(|color| preview_color_is_useful(*color))
                .unwrap_or([255, 255, 255, 255])
        } else {
            [255, 255, 255, 255]
        };
        let has_alpha_source = useful_material.is_some()
            || (combine_mode == J3dPreviewCombineMode::TextureModulateVertex
                && vertex_colors.is_some());
        apply_layer_preview_tint(
            modulated_color(base, vertex, if has_alpha_source { 0 } else { 255 }),
            render_layer,
        )
    })
}

fn material_color_rgb_tint(mut color: [u8; 4]) -> [u8; 4] {
    color[3] = 255;
    color
}

fn preview_triangle_color(
    model_index: usize,
    normal: [f32; 3],
    average_y: f32,
    material_color: Option<[u8; 4]>,
    vertex_colors: Option<[[u8; 4]; 3]>,
    combine_mode: J3dPreviewCombineMode,
) -> egui::Color32 {
    let palette = [
        [89.0, 123.0, 102.0],
        [129.0, 118.0, 86.0],
        [86.0, 111.0, 130.0],
        [120.0, 101.0, 82.0],
        [91.0, 128.0, 122.0],
        [139.0, 132.0, 102.0],
    ];
    let mut base =
        if let Some(color) = material_color.filter(|color| preview_color_is_useful(*color)) {
            [color[0] as f32, color[1] as f32, color[2] as f32]
        } else {
            palette[model_index % palette.len()]
        };
    if material_color.is_none() {
        if average_y < -3500.0 {
            base = [42.0, 94.0, 104.0];
        } else if average_y > 2800.0 {
            base = [117.0, 129.0, 122.0];
        }
    }

    let up = normal[1].abs();
    let side = (normal[0].abs() + normal[2].abs()) * 0.5;
    let shade = (0.58 + up * 0.26 + side * 0.08).clamp(0.42, 1.0);
    if combine_mode == J3dPreviewCombineMode::VertexOnly {
        if let Some(colors) = vertex_colors {
            let average = average_vertex_color(colors);
            if preview_color_is_useful(average) {
                return egui::Color32::from_rgba_unmultiplied(
                    average[0], average[1], average[2], average[3],
                );
            }
        }
    }
    if let Some(color) = material_color.filter(|color| preview_color_is_useful(*color)) {
        return egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
    }
    egui::Color32::from_rgba_unmultiplied(
        (base[0] * shade).clamp(0.0, 255.0) as u8,
        (base[1] * shade).clamp(0.0, 255.0) as u8,
        (base[2] * shade).clamp(0.0, 255.0) as u8,
        198,
    )
}

fn preview_solid_triangle_colors(
    triangle: &PreviewTriangle,
    normal: [f32; 3],
    average_y: f32,
) -> [egui::Color32; 3] {
    if triangle.combine_mode == J3dPreviewCombineMode::VertexOnly {
        if let Some(colors) = triangle.vertex_colors {
            if colors.iter().any(|color| preview_color_is_useful(*color)) {
                return colors.map(|color| {
                    apply_layer_preview_tint(
                        egui::Color32::from_rgba_unmultiplied(
                            color[0], color[1], color[2], color[3],
                        ),
                        triangle.render_layer,
                    )
                });
            }
        }
    }

    let color = preview_triangle_color(
        triangle.model_index,
        normal,
        average_y,
        triangle.color,
        triangle.vertex_colors,
        triangle.combine_mode,
    );
    [apply_layer_preview_tint(color, triangle.render_layer); 3]
}

fn apply_layer_preview_tint(
    color: egui::Color32,
    render_layer: PreviewRenderLayer,
) -> egui::Color32 {
    let rgba = color32_to_rgba(color);
    match render_layer {
        PreviewRenderLayer::Water => {
            let water = [0.35, 0.78, 0.96];
            rgba_to_color32([
                rgba[0] * 0.72 + water[0] * 0.28,
                rgba[1] * 0.72 + water[1] * 0.28,
                rgba[2] * 0.72 + water[2] * 0.28,
                rgba[3].min(0.82),
            ])
        }
        PreviewRenderLayer::Goop => {
            let goop = [0.28, 0.12, 0.36];
            rgba_to_color32([
                rgba[0] * 0.82 + goop[0] * 0.18,
                rgba[1] * 0.82 + goop[1] * 0.18,
                rgba[2] * 0.82 + goop[2] * 0.18,
                rgba[3].min(0.78),
            ])
        }
        PreviewRenderLayer::Sky | PreviewRenderLayer::Main => color,
    }
}

fn preview_color_is_useful(color: [u8; 4]) -> bool {
    color[3] > 12
        && !(color[0] > 242 && color[1] > 242 && color[2] > 242)
        && !(color[0] < 8 && color[1] < 8 && color[2] < 8)
}

fn average_vertex_color(colors: [[u8; 4]; 3]) -> [u8; 4] {
    [
        ((colors[0][0] as u16 + colors[1][0] as u16 + colors[2][0] as u16) / 3) as u8,
        ((colors[0][1] as u16 + colors[1][1] as u16 + colors[2][1] as u16) / 3) as u8,
        ((colors[0][2] as u16 + colors[1][2] as u16 + colors[2][2] as u16) / 3) as u8,
        ((colors[0][3] as u16 + colors[1][3] as u16 + colors[2][3] as u16) / 3) as u8,
    ]
}

fn modulated_color(material: [u8; 4], vertex: [u8; 4], fallback_alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (material[0] as u16 * vertex[0] as u16 / 255) as u8,
        (material[1] as u16 * vertex[1] as u16 / 255) as u8,
        (material[2] as u16 * vertex[2] as u16 / 255) as u8,
        ((material[3] as u16 * vertex[3] as u16 / 255) as u8).max(fallback_alpha),
    )
}

fn vector_drag(ui: &mut egui::Ui, values: &mut [f32; 3], speed: f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for (label, value) in ["X", "Y", "Z"].iter().zip(values.iter_mut()) {
            ui.label(*label);
            changed |= ui.add(egui::DragValue::new(value).speed(speed)).changed();
        }
    });
    changed
}

fn snap_transform(transform: &mut Transform, translation: f32, rotation: f32, scale: f32) {
    if translation > f32::EPSILON {
        for value in &mut transform.translation {
            *value = snap_value(*value, translation);
        }
    }
    if rotation > f32::EPSILON {
        for value in &mut transform.rotation_degrees {
            *value = snap_value(*value, rotation);
        }
    }
    if scale > f32::EPSILON {
        for value in &mut transform.scale {
            *value = snap_value(*value, scale).max(0.001);
        }
    }
}

fn snap_value(value: f32, step: f32) -> f32 {
    (value / step).round() * step
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 0.001,
                "expected {expected}, got {actual}"
            );
        }
    }

    fn camera_app() -> SmsEditorApp {
        let mut app = SmsEditorApp::default();
        {
            let camera = app.renderer.camera_mut();
            camera.focus = [0.0, 0.0, 1000.0];
            camera.yaw_degrees = 0.0;
            camera.pitch_degrees = 0.0;
            camera.distance = 1000.0;
        }
        app
    }

    #[test]
    fn nozzle_box_tev_color_matches_runtime_item_type() {
        let mut rocket = SceneObject::new("rocket-box", "NozzleBox");
        rocket.raw_params.insert(
            "stream_string_1".to_string(),
            "rocket_nozzle_item".to_string(),
        );
        let mut hover = SceneObject::new("hover-box", "NozzleBox");
        hover.raw_params.insert(
            "stream_string_1".to_string(),
            "back_nozzle_item".to_string(),
        );

        assert_eq!(nozzle_box_tev_reg1_color(&rocket), Some([255, 0, 0, 100]));
        assert_eq!(nozzle_box_tev_reg1_color(&hover), Some([90, 90, 120, 100]));
        assert_eq!(
            nozzle_box_tev_reg1_color(&SceneObject::new("coin", "Coin")),
            None
        );
    }

    #[test]
    fn material_table_candidates_include_base_actor_table() {
        assert_eq!(
            material_table_candidates_for_model("C:/game/dolpic.szs!/mapobj/kibako.bmd"),
            ["c:/game/dolpic.szs!/mapobj/kibako.bmt"]
        );
        assert_eq!(
            material_table_candidates_for_model("C:/game/dolpic.szs!/mapobj/kibako_crash.bmd"),
            [
                "c:/game/dolpic.szs!/mapobj/kibako_crash.bmt",
                "c:/game/dolpic.szs!/mapobj/kibako.bmt",
            ]
        );
    }

    fn preview_for_texture_alpha(has_alpha: bool, has_translucent_alpha: bool) -> ModelPreview {
        let image = egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
        ModelPreview {
            points: Vec::new(),
            triangles: Vec::new(),
            textures: vec![PreviewTexture {
                image: image.clone(),
                mips: vec![image],
                format: 6,
                wrap_s: 1,
                wrap_t: 1,
                min_filter: 1,
                mag_filter: 1,
                mipmap_count: 1,
                has_alpha,
                has_translucent_alpha,
            }],
            materials: Vec::new(),
            bounds_min: [0.0, 0.0, 0.0],
            bounds_max: [1.0, 1.0, 1.0],
            camera_bounds_min: [0.0, 0.0, 0.0],
            camera_bounds_max: [1.0, 1.0, 1.0],
            sky_radius: 0.0,
            loaded_models: 1,
            failed_models: 0,
            source_vertices: 0,
            source_triangles: 0,
            source_textures: 1,
            object_model_indices: BTreeMap::new(),
        }
    }

    fn preview_for_alpha_texture(has_translucent_alpha: bool) -> ModelPreview {
        preview_for_texture_alpha(true, has_translucent_alpha)
    }

    fn textured_blended_triangle() -> PreviewTriangle {
        PreviewTriangle {
            vertices: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: None,
            color_channels: [None; 2],
            tex_coord_sets: [None; 8],
            material_index: None,
            packet_index: 0,
            model_index: 1,
            render_layer: PreviewRenderLayer::Main,
            color: None,
            vertex_colors: None,
            combine_mode: J3dPreviewCombineMode::TextureOnly,
            tex_coords: Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            texture_index: Some(0),
            mask_tex_coords: None,
            mask_texture_index: None,
            cull_mode: None,
            alpha_compare: None,
            blend_mode: Some(J3dBlendMode {
                mode: 1,
                src_factor: 4,
                dst_factor: 5,
                logic_op: 0,
            }),
            z_mode: None,
        }
    }

    #[test]
    fn rmb_free_look_keeps_camera_position_fixed() {
        let mut app = camera_app();
        let old_position = app.camera_frame().position;

        app.rotate_camera_in_place(egui::vec2(80.0, -30.0));

        assert_vec3_close(app.camera_frame().position, old_position);
    }

    #[test]
    fn rmb_horizontal_drag_uses_unreal_style_yaw_sign() {
        let mut app = camera_app();

        app.rotate_camera_in_place(egui::vec2(80.0, 0.0));

        assert!(app.renderer.camera().yaw_degrees < 0.0);
    }

    #[test]
    fn alt_orbit_uses_same_horizontal_yaw_sign() {
        let mut app = camera_app();

        app.orbit_camera(egui::vec2(80.0, 0.0));

        assert!(app.renderer.camera().yaw_degrees < 0.0);
    }

    #[test]
    fn move_drag_uses_camera_relative_ground_plane() {
        let app = camera_app();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let right_drag = app.viewport_drag_move_delta(rect, egui::vec2(10.0, 0.0));
        let up_drag = app.viewport_drag_move_delta(rect, egui::vec2(0.0, -10.0));

        assert!(right_drag[0] < 0.0);
        assert!(right_drag[2].abs() < 0.001);
        assert!(up_drag[2] > 0.0);
        assert!(up_drag[0].abs() < 0.001);
    }

    #[test]
    fn move_drag_rotates_with_camera_yaw() {
        let mut app = camera_app();
        app.renderer.camera_mut().yaw_degrees = 90.0;
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let right_drag = app.viewport_drag_move_delta(rect, egui::vec2(10.0, 0.0));
        let up_drag = app.viewport_drag_move_delta(rect, egui::vec2(0.0, -10.0));

        assert!(right_drag[2] > 0.0);
        assert!(right_drag[0].abs() < 0.001);
        assert!(up_drag[0] > 0.0);
        assert!(up_drag[2].abs() < 0.001);
    }

    #[test]
    fn reflection_helper_meshes_are_effect_meshes() {
        let path = "stage.szs!/map/map/reflectsky.bmd";

        assert!(!is_default_preview_model_path(path, true, true, false));
        assert!(is_default_preview_model_path(path, true, true, true));
        assert!(!is_camera_bounds_model_path(path));
    }

    #[test]
    fn skybox_model_is_loaded_as_camera_relative_environment() {
        let path = "stage.szs!/map/map/sky.bmd";

        assert!(is_default_preview_model_path(path, false, false, false));
        assert!(!is_camera_bounds_model_path(path));
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Sky
        );
        assert_eq!(
            model_loader_flags_for_path(path),
            SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS
        );
        assert!(!path_is_sky_model_path("stage.szs!/map/map/reflectsky.bmd"));
    }

    #[test]
    fn skybox_vertices_track_camera_translation() {
        let vertices = [[1.0, 2.0, 3.0], [-4.0, 5.0, 6.0], [7.0, 8.0, -9.0]];
        let camera = [100.0, 200.0, 300.0];

        assert_eq!(
            preview_triangle_world_vertices(vertices, PreviewRenderLayer::Sky, camera),
            [
                [101.0, 202.0, 303.0],
                [96.0, 205.0, 306.0],
                [107.0, 208.0, 291.0],
            ]
        );
        assert_eq!(
            preview_triangle_world_vertices(vertices, PreviewRenderLayer::Main, camera),
            vertices
        );
    }

    #[test]
    fn skybox_radius_expands_the_far_clip() {
        let mut preview = preview_for_texture_alpha(false, false);
        preview.sky_radius = 150_000.0;

        let far = preview.far_clip(1_000.0);
        assert!((157_000.0..158_000.0).contains(&far));
    }

    #[test]
    fn pollution_meshes_are_goop_not_generic_effects() {
        let path = "stage.szs!/map/pollution/pollution00.bmd";

        assert!(is_default_preview_model_path(path, true, true, false));
        assert!(!is_default_preview_model_path(path, true, false, true));
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Goop
        );
        assert!(!is_camera_bounds_model_path(path));
    }

    #[test]
    fn named_pollution_map_meshes_are_goop() {
        let path = "stage.szs!/map/map/mareseapollutions0.bmd";

        assert!(is_default_preview_model_path(path, true, true, false));
        assert!(!is_default_preview_model_path(path, true, false, true));
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Goop
        );
        assert!(!is_camera_bounds_model_path(path));
    }

    #[test]
    fn sea_meshes_are_level_water_layer() {
        let path = "stage.szs!/map/map/sea.bmd";

        assert!(is_default_preview_model_path(path, true, true, false));
        assert!(!is_default_preview_model_path(path, false, true, true));
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Water
        );
        assert!(!is_camera_bounds_model_path(path));
    }

    #[test]
    fn map_puddles_are_level_water_layer() {
        let path = "stage.szs!/map/mirror/puddle00.bmd";

        assert!(is_default_preview_model_path(path, true, true, false));
        assert!(!is_default_preview_model_path(path, false, true, true));
        assert_eq!(
            preview_render_layer_for_model_path(path),
            PreviewRenderLayer::Water
        );
        assert!(!is_camera_bounds_model_path(path));
    }

    #[test]
    fn indirect_water_helpers_stay_hidden_by_default() {
        let sea_path = "stage.szs!/map/map/seaindirect.bmd";
        let puddle_path = "stage.szs!/map/mirror/puddle_ind00.bmd";

        for path in [sea_path, puddle_path] {
            assert!(!is_default_preview_model_path(path, true, true, true));
            assert_ne!(
                preview_render_layer_for_model_path(path),
                PreviewRenderLayer::Water
            );
            assert!(!is_camera_bounds_model_path(path));
        }
    }

    #[test]
    fn water_layer_renders_translucent_without_texture_alpha() {
        let preview = preview_for_alpha_texture(false);
        let mut triangle = textured_blended_triangle();
        triangle.render_layer = PreviewRenderLayer::Water;
        triangle.texture_index = None;
        triangle.tex_coords = None;
        triangle.blend_mode = None;

        assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn unmasked_goop_layer_renders_as_translucent_overlay() {
        let preview = preview_for_texture_alpha(false, false);
        let mut triangle = textured_blended_triangle();
        triangle.render_layer = PreviewRenderLayer::Goop;
        triangle.blend_mode = None;

        assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn masked_goop_layer_uses_alpha_test() {
        let preview = preview_for_texture_alpha(false, false);
        let mut triangle = textured_blended_triangle();
        triangle.render_layer = PreviewRenderLayer::Goop;
        triangle.blend_mode = None;
        triangle.mask_texture_index = Some(0);
        triangle.mask_tex_coords = triangle.tex_coords;

        assert!(preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(!preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn blended_cutout_texture_uses_alpha_test_not_translucency() {
        let preview = preview_for_alpha_texture(false);
        let triangle = textured_blended_triangle();

        assert!(preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(!preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn blended_fractional_alpha_texture_stays_translucent() {
        let preview = preview_for_alpha_texture(true);
        let triangle = textured_blended_triangle();

        assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn masked_texture_triangle_uses_alpha_test() {
        let preview = preview_for_texture_alpha(false, false);
        let mut triangle = textured_blended_triangle();
        triangle.blend_mode = None;
        triangle.mask_texture_index = Some(0);
        triangle.mask_tex_coords = triangle.tex_coords;

        assert!(preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(!preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn parses_comma_separated_camera_focus_arg() {
        assert_eq!(
            parse_vec3_arg("-995.2,8353,6493"),
            Some([-995.2, 8353.0, 6493.0])
        );
        assert_eq!(parse_vec3_arg("1,2"), None);
    }

    #[test]
    fn textured_material_tint_does_not_inherit_material_alpha() {
        let tints = preview_texture_tints(
            Some([128, 128, 128, 50]),
            None,
            J3dPreviewCombineMode::TextureModulateMaterial,
            PreviewRenderLayer::Main,
        );

        assert_eq!(
            tints[0],
            egui::Color32::from_rgba_unmultiplied(128, 128, 128, 255)
        );
    }

    #[test]
    fn color32_to_rgba_unpremultiplies_transparent_editor_tints() {
        let rgba = color32_to_rgba(egui::Color32::from_rgba_unmultiplied(144, 217, 255, 50));

        assert!((rgba[0] - 144.0 / 255.0).abs() < 0.01);
        assert!((rgba[1] - 217.0 / 255.0).abs() < 0.01);
        assert!((rgba[2] - 1.0).abs() < 0.01);
        assert!((rgba[3] - 50.0 / 255.0).abs() < 0.01);
    }

    #[test]
    fn software_opaque_pass_outputs_solid_pixels_after_alpha_keep() {
        let mut image = egui::ColorImage::filled([1, 1], egui::Color32::from_rgb(10, 20, 30));
        let mut depth = vec![f32::INFINITY];
        let src = software_output_color_for_pass([0.8, 0.4, 0.2, 0.25], true);

        blend_depth_pixel(&mut image, &mut depth, 0, 42.0, src, true);

        let rgba = color32_to_rgba(image.pixels[0]);
        assert!((rgba[0] - 0.8).abs() < 0.01);
        assert!((rgba[1] - 0.4).abs() < 0.01);
        assert!((rgba[2] - 0.2).abs() < 0.01);
        assert!((rgba[3] - 1.0).abs() < 0.01);
        assert_eq!(depth[0], 42.0);
    }

    #[test]
    fn software_translucent_pass_keeps_fractional_alpha_blending() {
        let mut image = egui::ColorImage::filled([1, 1], egui::Color32::from_rgb(10, 20, 30));
        let mut depth = vec![f32::INFINITY];
        let src = software_output_color_for_pass([1.0, 0.0, 0.0, 0.25], false);

        blend_depth_pixel(&mut image, &mut depth, 0, 42.0, src, false);

        let rgba = color32_to_rgba(image.pixels[0]);
        assert!(rgba[0] > 0.25);
        assert!(rgba[1] < 20.0 / 255.0);
        assert!(rgba[2] < 30.0 / 255.0);
        assert_eq!(depth[0], f32::INFINITY);
    }

    #[test]
    fn textured_material_alpha_does_not_make_opaque_texture_translucent() {
        let preview = preview_for_texture_alpha(false, false);
        let mut triangle = textured_blended_triangle();
        triangle.color = Some([128, 128, 128, 50]);
        triangle.combine_mode = J3dPreviewCombineMode::TextureModulateMaterial;

        assert!(!preview_triangle_uses_alpha_test(&preview, &triangle));
        assert!(!preview_triangle_is_translucent(&preview, &triangle));
    }

    #[test]
    fn retransform_preview_point_preserves_object_local_space() {
        let old_transform = Transform {
            translation: [100.0, 20.0, -40.0],
            rotation_degrees: [0.0, 90.0, 0.0],
            scale: [2.0, 1.0, 1.0],
        };
        let new_transform = Transform {
            translation: [-30.0, 10.0, 80.0],
            rotation_degrees: [0.0, -45.0, 0.0],
            scale: [1.0, 2.0, 1.0],
        };
        let local = [8.0, 4.0, -12.0];
        let old_world = transform_preview_point(local, old_transform);
        let new_world = transform_preview_point(local, new_transform);

        assert_vec3_close(
            retransform_preview_point(old_world, old_transform, new_transform),
            new_world,
        );
    }

    #[test]
    fn transform_preview_normal_ignores_translation_and_normalizes() {
        let transform = Transform {
            translation: [500.0, 0.0, -1000.0],
            rotation_degrees: [0.0, 90.0, 0.0],
            scale: [2.0, 1.0, 1.0],
        };

        assert_vec3_close(
            transform_preview_normal([1.0, 0.0, 0.0], transform),
            [0.0, 0.0, -1.0],
        );
    }

    #[test]
    fn updating_object_transform_moves_cached_preview_mesh() {
        let old_transform = Transform::default();
        let new_transform = Transform {
            translation: [50.0, 0.0, -25.0],
            ..Transform::default()
        };
        let mut object_model_indices = BTreeMap::new();
        object_model_indices.insert("obj-1".to_string(), 7);
        let mut app = SmsEditorApp {
            model_preview: Some(ModelPreview {
                points: vec![PreviewPoint {
                    position: [1.0, 2.0, 3.0],
                    model_index: 7,
                }],
                triangles: vec![PreviewTriangle {
                    vertices: [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]],
                    normals: Some([[0.0, 1.0, 0.0]; 3]),
                    color_channels: [None; 2],
                    tex_coord_sets: [None; 8],
                    material_index: None,
                    packet_index: 0,
                    model_index: 7,
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
                }],
                textures: Vec::new(),
                materials: Vec::new(),
                bounds_min: [0.0, 0.0, 0.0],
                bounds_max: [1.0, 2.0, 3.0],
                camera_bounds_min: [0.0, 0.0, 0.0],
                camera_bounds_max: [1.0, 2.0, 3.0],
                sky_radius: 0.0,
                loaded_models: 1,
                failed_models: 0,
                source_vertices: 3,
                source_triangles: 1,
                source_textures: 0,
                object_model_indices,
            }),
            ..SmsEditorApp::default()
        };

        assert!(app.update_object_preview_transform("obj-1", old_transform, new_transform));
        let preview = app.model_preview.as_ref().unwrap();
        assert_vec3_close(preview.points[0].position, [51.0, 2.0, -22.0]);
        assert_vec3_close(preview.triangles[0].vertices[0], [51.0, 0.0, -25.0]);
        assert_vec3_close(preview.triangles[0].vertices[1], [50.0, 2.0, -25.0]);
        assert_vec3_close(preview.triangles[0].vertices[2], [50.0, 0.0, -22.0]);
    }
}
