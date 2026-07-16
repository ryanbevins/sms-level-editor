use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    mpsc::{self, Receiver, TryRecvError},
    Arc,
};
use std::thread;
use std::time::Instant;

use eframe::egui;
use sms_formats::{
    decode_bti_texture, discover_scene_archives, mount_scene_archive, parse_jdrama_object_records,
    read_stage_asset_bytes, J3dAlphaCompare, J3dBillboard, J3dBillboardMode, J3dBlendMode, J3dFile,
    J3dGeometryPreview, J3dJointAnimation, J3dJointTransformOverride, J3dMaterial, J3dMatrix34,
    J3dPreparedAnimatedTriangles, J3dPreviewCombineMode, J3dTexturePatternAnimation,
    J3dTextureSrtAnimation, J3dTriangle, J3dZMode, JpaEffect, SceneArchiveInfo, StageAsset,
    StageAssetKind, SMS_ANIMATION_FRAMES_PER_SECOND, SMS_DEFAULT_OBJECT_MODEL_LOAD_FLAGS,
    SMS_MAP_MODEL_LOAD_FLAGS, SMS_POLLUTION_MODEL_LOAD_FLAGS,
};
use sms_render::{
    gx_blend_compatibility, GxBlendCompatibility, RenderScene, RendererConfig, ViewportRenderer,
};
use sms_scene::{
    AssetRef, AssetRole, SceneObject, StageDocument, Transform, ValidationIssue, ValidationSeverity,
};
use sms_schema::{ObjectDefinition, ObjectRegistry, ParticleBindingTarget, SchemaGenerator};

mod camera;
mod document_commands;
mod gpu_viewport;
mod ui_panels;
mod viewport_ui;

const VIEWPORT_NEAR_CLIP: f32 = 8.0;

pub fn run() -> eframe::Result<()> {
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

#[derive(Debug, Clone, Copy)]
struct PreviewVisibility {
    environment: bool,
    goop: bool,
    effects: bool,
}

struct LoadedStage {
    base_root: String,
    project_root: String,
    archives: Vec<SceneArchiveInfo>,
    registry: Option<ObjectRegistry>,
    schema_warning: Option<String>,
    document: StageDocument,
    scene: RenderScene,
    preview: Option<ModelPreview>,
}

enum BackgroundResult {
    Schema(Box<Result<ObjectRegistry, String>>),
    Scan {
        base_root: String,
        result: Result<Vec<SceneArchiveInfo>, String>,
    },
    Open(Result<Box<LoadedStage>, String>),
}

mod preview_types;
use preview_types::*;

fn validation_issues_for_preview(
    document: &StageDocument,
    preview: Option<&ModelPreview>,
) -> Vec<ValidationIssue> {
    let mut issues = document.validate();
    let Some(preview) = preview else {
        return issues;
    };
    for (index, failure) in preview.model_failures.iter().enumerate() {
        issues.push(ValidationIssue::warning(
            format!("renderer-model-preview-failed-{index}"),
            format!(
                "Model preview failed for '{}': {}",
                failure.asset_path, failure.error
            ),
        ));
    }
    if preview.failed_models > preview.model_failures.len() {
        issues.push(ValidationIssue::warning(
            "renderer-model-preview-failures-truncated",
            format!(
                "{} additional model asset failure(s) were omitted after the first {} details",
                preview.failed_models - preview.model_failures.len(),
                preview.model_failures.len()
            ),
        ));
    }
    for (index, material) in preview.materials.iter().enumerate() {
        let blend = material.blend_mode;
        append_gpu_blend_validation_issue(
            &mut issues,
            index,
            &material.name,
            blend.mode,
            blend.logic_op,
        );
    }
    for (index, texture) in preview.textures.iter().enumerate() {
        let mut unsupported_flags = Vec::new();
        if texture.do_edge_lod {
            unsupported_flags.push("edge LOD");
        }
        if texture.bias_clamp {
            unsupported_flags.push("LOD bias clamp");
        }
        if !unsupported_flags.is_empty() {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-texture-lod-unsupported-{index}"),
                format!(
                    "Texture #{index} requests GX {}, which the wgpu preview does not emulate",
                    unsupported_flags.join(" and ")
                ),
            ));
        }
        if !gpu_viewport::authored_sampler_anisotropy_is_supported(texture) {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-texture-anisotropy-filter-conflict-{index}"),
                format!(
                    "Texture #{index} requests GX {}x anisotropy with non-linear minification filters; WebGPU cannot combine those states, so the preview preserves the authored filters and disables anisotropy",
                    1u16 << texture.max_anisotropy.min(2)
                ),
            ));
        }
    }
    issues
}

fn append_gpu_blend_validation_issue(
    issues: &mut Vec<ValidationIssue>,
    material_index: usize,
    material_name: &str,
    mode: u8,
    logic_op: u8,
) {
    match gx_blend_compatibility(mode, logic_op) {
        GxBlendCompatibility::UnsupportedLogicOperation { logic_operation } => {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-logic-op-unsupported-{material_index}"),
                format!(
                    "Material #{material_index} '{material_name}' uses GX framebuffer logic operation {logic_operation}; the wgpu preview uses overwrite fallback because this operation depends on the existing framebuffer value"
                ),
            ));
        }
        GxBlendCompatibility::UnsupportedMode { mode } => {
            issues.push(ValidationIssue::warning(
                format!("renderer-gx-blend-mode-unsupported-{material_index}"),
                format!(
                    "Material #{material_index} '{material_name}' uses unknown GX blend mode {mode}; the wgpu preview uses overwrite fallback"
                ),
            ));
        }
        GxBlendCompatibility::Native
        | GxBlendCompatibility::SourceIndependentLogicOperation { .. } => {}
    }
}

const MAX_MODEL_FAILURE_DETAILS: usize = 32;

fn record_model_preview_failure(
    failed_assets: &mut BTreeSet<String>,
    failures: &mut Vec<PreviewModelFailure>,
    asset_path: &str,
    error: String,
) {
    if !failed_assets.insert(normalized_preview_asset_path(asset_path)) {
        return;
    }
    if failures.len() < MAX_MODEL_FAILURE_DETAILS {
        failures.push(PreviewModelFailure {
            asset_path: asset_path.to_string(),
            error,
        });
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ObjectDelta {
    Insert {
        index: usize,
        object: SceneObject,
    },
    Remove {
        index: usize,
        object: SceneObject,
    },
    Update {
        before: Box<SceneObject>,
        after: Box<SceneObject>,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct ObjectUndoRecord {
    deltas: Vec<ObjectDelta>,
}

#[derive(Debug, Clone, PartialEq)]
struct ObjectUndoTransaction {
    index: usize,
    before: SceneObject,
}

struct SmsEditorApp {
    repo_root: String,
    base_root: String,
    project_root: String,
    stage_id: String,
    dolphin_path: String,
    game_path: String,
    dolphin_user_dir: String,
    registry: Option<ObjectRegistry>,
    document: Option<StageDocument>,
    render_scene: Option<RenderScene>,
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
    pending_auto_refresh_root: Option<String>,
    last_auto_refresh_attempt_root: String,
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
    camera_fly_velocity: [f32; 3],
    next_object_serial: u32,
    saved_objects: Vec<SceneObject>,
    document_dirty: bool,
    undo_stack: VecDeque<ObjectUndoRecord>,
    redo_stack: VecDeque<ObjectUndoRecord>,
    undo_transaction: Option<ObjectUndoTransaction>,
    pending_stage_open: Option<String>,
    close_confirmation_requested: bool,
    close_authorized: bool,
    background_receiver: Option<Receiver<BackgroundResult>>,
    background_label: Option<String>,
    animation_started_at: Instant,
    last_skeletal_animation_tick: u64,
    level_transform_progress: f32,
    level_transform_playing: bool,
    level_transform_started_at: Instant,
    level_transform_playback_origin: f32,
    last_level_transform_progress_bits: u32,
}

impl Default for SmsEditorApp {
    fn default() -> Self {
        let args = editor_startup_args();
        let base_root = args.base_root.unwrap_or_else(default_base_root);
        Self {
            repo_root: args.repo_root.unwrap_or_else(default_repo_root),
            base_root,
            project_root: "sms-editor-project".to_string(),
            stage_id: args.stage_id.unwrap_or_else(|| "dolpic0".to_string()),
            dolphin_path: String::new(),
            game_path: String::new(),
            dolphin_user_dir: String::new(),
            registry: None,
            document: None,
            render_scene: None,
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
            pending_auto_refresh_root: None,
            last_auto_refresh_attempt_root: String::new(),
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
            camera_fly_velocity: [0.0; 3],
            next_object_serial: 1,
            saved_objects: Vec::new(),
            document_dirty: false,
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            undo_transaction: None,
            pending_stage_open: None,
            close_confirmation_requested: false,
            close_authorized: false,
            background_receiver: None,
            background_label: None,
            animation_started_at: Instant::now(),
            last_skeletal_animation_tick: u64::MAX,
            level_transform_progress: 0.0,
            level_transform_playing: false,
            level_transform_started_at: Instant::now(),
            level_transform_playback_origin: 0.0,
            last_level_transform_progress_bits: u32::MAX,
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
        self.poll_background_task(ctx);
        if ctx.input(|input| input.viewport().close_requested())
            && self.is_dirty()
            && !self.close_authorized
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_confirmation_requested = true;
        }

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
        if self.undo_transaction.is_some() && !root_ui.input(|input| input.pointer.primary_down()) {
            self.commit_undo_transaction("Updated transform");
        }
        self.unsaved_changes_dialog(root_ui.ctx());
    }
}

impl SmsEditorApp {
    // Panel implementations live in ui_panels.rs.

    // Viewport interaction and painting live in viewport_ui.rs.

    fn generate_schema(&mut self) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let repo_root = self.repo_root.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = SchemaGenerator::new(repo_root)
                .generate()
                .map_err(|err| err.to_string());
            let _ = sender.send(BackgroundResult::Schema(Box::new(result)));
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some("Generating schema".to_string());
        self.log.push("Generating object schema...".to_string());
    }

    fn refresh_scene_browser_if_needed(&mut self) {
        let base_root = self.base_root.trim().to_string();
        if base_root.is_empty() {
            self.pending_auto_refresh_root = None;
            self.last_auto_refresh_attempt_root.clear();
            return;
        }

        if self.last_scanned_base_root == base_root {
            self.pending_auto_refresh_root = None;
            self.last_auto_refresh_attempt_root = base_root;
            return;
        }

        if self.last_auto_refresh_attempt_root != base_root
            && self.pending_auto_refresh_root.as_deref() != Some(base_root.as_str())
        {
            self.pending_auto_refresh_root = Some(base_root.clone());
        }

        if self.background_receiver.is_some()
            || self.pending_auto_refresh_root.as_deref() != Some(base_root.as_str())
            || !PathBuf::from(&base_root).exists()
        {
            return;
        }

        self.pending_auto_refresh_root = None;
        self.last_auto_refresh_attempt_root = base_root;
        let should_open = self.document.is_none();
        if should_open && !self.stage_id.trim().is_empty() {
            self.open_stage();
        } else {
            self.scan_scenes();
        }
    }

    fn scan_scenes(&mut self) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let base_root = self.base_root.trim().to_string();
        if base_root.is_empty() {
            self.log
                .push("Base root is required for scene scan.".to_string());
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let task_base_root = base_root.clone();
        thread::spawn(move || {
            let result = discover_scene_archives(PathBuf::from(&task_base_root))
                .map_err(|err| err.to_string());
            let _ = sender.send(BackgroundResult::Scan {
                base_root: task_base_root,
                result,
            });
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some("Scanning scenes".to_string());
        self.log
            .push(format!("Scanning scenes under {base_root}..."));
    }

    fn open_scene_archive(&mut self, archive: SceneArchiveInfo) {
        self.log
            .push(format!("Selected scene '{}'.", archive.stage_id));
        self.request_open_stage(archive.stage_id);
    }

    fn request_open_stage(&mut self, stage_id: String) {
        if self.is_dirty() {
            self.pending_stage_open = Some(stage_id);
        } else {
            self.stage_id = stage_id;
            self.open_stage();
        }
    }

    fn open_stage(&mut self) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let base_root = self.base_root.trim().to_string();
        let repo_root = self.repo_root.trim().to_string();
        let project_root = self.project_root.trim().to_string();
        let stage_id = self.stage_id.trim().to_string();
        if base_root.is_empty() || stage_id.is_empty() {
            self.log
                .push("Base root and stage are required.".to_string());
            return;
        }

        let visibility = self.preview_visibility();
        let existing_registry = self.registry.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> Result<Box<LoadedStage>, String> {
                let archives = discover_scene_archives(PathBuf::from(&base_root))
                    .map_err(|err| err.to_string())?;
                let (registry, schema_warning) = if let Some(registry) = existing_registry {
                    (Some(registry), None)
                } else {
                    match SchemaGenerator::new(&repo_root).generate() {
                        Ok(registry) => (Some(registry), None),
                        Err(err) => (None, Some(err.to_string())),
                    }
                };
                let mut document = StageDocument::open(PathBuf::from(&base_root), stage_id)
                    .map_err(|err| err.to_string())?;
                if !project_root.is_empty() {
                    document
                        .load_project_folder(PathBuf::from(&project_root))
                        .map_err(|err| err.to_string())?;
                }
                if let Some(registry) = registry.clone() {
                    document = document.with_registry(registry);
                }
                let scene = RenderScene::from_document(&document);
                let preview = SmsEditorApp::build_model_preview(&document, visibility);
                Ok(Box::new(LoadedStage {
                    base_root,
                    project_root,
                    archives,
                    registry,
                    schema_warning,
                    document,
                    scene,
                    preview,
                }))
            })();
            let _ = sender.send(BackgroundResult::Open(result));
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some(format!("Opening {}", self.stage_id));
        self.log
            .push(format!("Opening stage '{}'...", self.stage_id));
    }

    fn poll_background_task(&mut self, ctx: &egui::Context) {
        let result = self.background_receiver.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(result)) => {
                self.background_receiver = None;
                self.background_label = None;
                match result {
                    BackgroundResult::Schema(result) => match *result {
                        Ok(registry) => {
                            self.log.push(format!(
                                "Generated {} object entries.",
                                registry.objects.len()
                            ));
                            if let Some(document) = &mut self.document {
                                document.refresh_registry_derived_object_fields(
                                    &mut self.saved_objects,
                                    &registry,
                                );
                                document.set_registry(registry.clone());
                                self.document_dirty = document.objects != self.saved_objects;
                                self.issues = document.validate();
                            }
                            self.registry = Some(registry);
                            if self.document.is_some() {
                                self.rebuild_model_preview_from_document();
                            }
                        }
                        Err(err) => self.log.push(format!("Schema generation failed: {err}")),
                    },
                    BackgroundResult::Scan { base_root, result } => match result {
                        Ok(archives) => {
                            if self.base_root.trim() != base_root {
                                self.log.push(format!(
                                    "Discarded scene scan for superseded base root {base_root}."
                                ));
                                return;
                            }
                            let count = archives.len();
                            self.scene_archives = archives;
                            self.last_scanned_base_root = base_root;
                            if self.stage_id.trim().is_empty() {
                                if let Some(first) = self.scene_archives.first() {
                                    self.stage_id = first.stage_id.clone();
                                }
                            }
                            self.log
                                .push(format!("Discovered {count} scene archive(s)."));
                        }
                        Err(err) => self.log.push(format!("Scene scan failed: {err}")),
                    },
                    BackgroundResult::Open(result) => match result {
                        Ok(loaded) => self.apply_loaded_stage(*loaded),
                        Err(err) => self.log.push(format!("Open stage failed: {err}")),
                    },
                }
            }
            Some(Err(TryRecvError::Empty)) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.background_receiver = None;
                self.background_label = None;
                self.log
                    .push("Background operation ended unexpectedly.".to_string());
            }
            None => {}
        }
    }

    fn apply_loaded_stage(&mut self, loaded: LoadedStage) {
        let LoadedStage {
            base_root,
            project_root,
            archives,
            registry,
            schema_warning,
            document,
            scene,
            preview,
        } = loaded;
        if self.base_root.trim() != base_root {
            self.log.push(format!(
                "Discarded stage load for superseded base root {base_root}."
            ));
            return;
        }
        if self.project_root.trim() != project_root {
            self.log.push(format!(
                "Discarded stage load for superseded project root {project_root}."
            ));
            return;
        }
        if self.stage_id.trim() != document.stage_id {
            self.log.push(format!(
                "Discarded stage load for superseded stage {}.",
                document.stage_id
            ));
            return;
        }
        if let Some(warning) = schema_warning {
            self.log.push(format!(
                "Schema generation failed; stage opened without it: {warning}"
            ));
        }
        self.registry = registry;
        self.scene_archives = archives;
        self.last_scanned_base_root = self.base_root.trim().to_string();
        self.log.push(format!(
            "Opened stage '{}' with {} asset(s), {} model(s), {} collision file(s).",
            document.stage_id,
            document.assets.len(),
            scene.model_paths.len(),
            scene.collision_paths.len()
        ));
        if let Some(preview) = &preview {
            self.log.push(format!(
                "Viewport preview loaded {} model(s), {} sampled point(s), {} triangle(s), {} texture(s), {} BTK material animation(s), {} BCK skeletal animation(s), {} procedural map-joint transformation(s), {} level-change JPA effect(s), {} actor-bound JPA effect(s), {} source vertex/vertices.",
                preview.loaded_models,
                preview.points.len(),
                preview.triangles.len(),
                preview.textures.len(),
                preview.texture_srt_animations.len(),
                preview.animated_models.len(),
                preview
                    .level_transform_models
                    .iter()
                    .map(|model| model.targets.len())
                    .sum::<usize>(),
                preview.level_transform_particles.len(),
                preview.actor_particles.len(),
                preview.source_vertices
            ));
        } else if !scene.model_paths.is_empty() {
            self.log
                .push("Viewport preview could not decode BMD vertex data.".to_string());
        }
        self.issues = validation_issues_for_preview(&document, preview.as_ref());
        for issue in self
            .issues
            .iter()
            .filter(|issue| issue.code.starts_with("renderer-"))
        {
            self.log.push(format!(
                "Renderer warning [{}] {}",
                issue.code, issue.message
            ));
        }
        self.saved_objects = document.objects.clone();
        self.document_dirty = false;
        self.stage_id = document.stage_id.clone();
        self.document = Some(document);
        self.render_scene = Some(scene);
        self.model_preview = preview;
        self.animation_started_at = Instant::now();
        self.last_skeletal_animation_tick = u64::MAX;
        self.level_transform_progress = 0.0;
        self.level_transform_playing = false;
        self.level_transform_playback_origin = 0.0;
        self.last_level_transform_progress_bits = u32::MAX;
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
        self.selected_object_id = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_transaction = None;
        self.reset_camera();
        self.apply_startup_camera_focus();
    }

    fn build_model_preview(
        document: &StageDocument,
        visibility: PreviewVisibility,
    ) -> Option<ModelPreview> {
        const POINT_BUDGET: usize = 16_000;
        const POINTS_PER_MODEL: usize = 1_200;
        const POINTS_PER_OBJECT_INSTANCE: usize = 500;

        let active_pollution_layers = active_pollution_layer_count(document);
        let mirror_cubes = mirror_cubes(document);
        let active_mirror_model_slots = mirror_cubes
            .iter()
            .map(|cube| cube.model_slot)
            .collect::<BTreeSet<_>>();
        let models: Vec<_> = document
            .assets
            .iter()
            .filter(|asset| asset.kind == StageAssetKind::Model)
            .filter(|asset| map_static_model_is_active(document, &asset.path.to_string_lossy()))
            .filter(|asset| {
                mirror_surface_model_is_active(
                    &document.stage_id,
                    &asset.path.to_string_lossy(),
                    &active_mirror_model_slots,
                )
            })
            .filter(|asset| {
                pollution_layer_model_is_active(
                    &asset.path.to_string_lossy(),
                    active_pollution_layers,
                )
            })
            .collect();
        let preferred: Vec<_> = models
            .iter()
            .copied()
            .filter(|asset| {
                let path = asset.path.to_string_lossy().replace('\\', "/");
                is_default_preview_model_path(
                    &path,
                    visibility.environment,
                    visibility.goop,
                    visibility.effects,
                )
            })
            .collect();
        let models = if preferred.is_empty() {
            models
        } else {
            preferred
        };
        let world_model_paths: BTreeSet<_> = models
            .iter()
            .map(|asset| normalized_preview_asset_path(&asset.path.to_string_lossy()))
            .collect();

        let mut points = Vec::new();
        let mut triangles = Vec::new();
        let mut textures = Vec::new();
        let mut materials = Vec::new();
        let mut texture_srt_animations = Vec::new();
        let mut texture_pattern_animations = Vec::new();
        let mut material_animation_bindings = Vec::new();
        let mut animated_flags = Vec::new();
        let mut level_transform_models = Vec::new();
        let mut level_transform_particles = Vec::new();
        let mut actor_particles = Vec::new();
        let actor_particle_effects = load_actor_particle_effects(document);
        let mut next_packet_index = 0usize;
        let mut bounds_min = [f32::INFINITY; 3];
        let mut bounds_max = [f32::NEG_INFINITY; 3];
        let mut camera_bounds_min = [f32::INFINITY; 3];
        let mut camera_bounds_max = [f32::NEG_INFINITY; 3];
        let mut camera_bound_points = Vec::new();
        let mut loaded_models = 0;
        let mut failed_model_assets = BTreeSet::new();
        let mut model_failures = Vec::new();
        let mut source_vertices = 0;
        let mut source_triangles = 0;
        let mut source_textures = 0;
        let mut object_model_indices = BTreeMap::new();
        let mut mirror_model_slots = BTreeMap::new();

        for asset in models {
            let asset_path = asset.path.to_string_lossy().replace('\\', "/");
            let include_in_camera_bounds = is_camera_bounds_model_path(&asset_path);
            let model_render_layer = preview_render_layer_for_model_path(&asset_path);
            let is_sky = model_render_layer == PreviewRenderLayer::Sky;
            let bytes = match read_stage_asset_bytes(&asset.path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    record_model_preview_failure(
                        &mut failed_model_assets,
                        &mut model_failures,
                        &asset_path,
                        format!("read asset: {error}"),
                    );
                    continue;
                }
            };
            let file = match J3dFile::parse(&bytes) {
                Ok(file) => file,
                Err(error) => {
                    record_model_preview_failure(
                        &mut failed_model_assets,
                        &mut model_failures,
                        &asset_path,
                        format!("parse J3D: {error}"),
                    );
                    continue;
                }
            };

            let loader_flags = map_static_model_loader_flags(document, &asset_path)
                .unwrap_or_else(|| model_loader_flags_for_path(&asset_path));
            let level_targets = level_transform_targets(document, &asset_path, &file);
            let initial_overrides = level_transform_overrides(&level_targets, 0.0);
            let preview_result = if initial_overrides.is_empty() {
                file.geometry_preview_with_loader_flags(loader_flags)
            } else {
                file.geometry_preview_with_joint_overrides(loader_flags, &initial_overrides)
            };
            match preview_result {
                Ok(mut preview) => {
                    apply_level_transform_visibility(
                        &file,
                        &level_targets,
                        0.0,
                        &mut preview.triangles,
                    );
                    apply_model_material_table(document, &asset_path, loader_flags, &mut preview);
                    apply_pollution_bitmap_mask(document, &asset_path, &mut preview);
                    loaded_models += 1;
                    let model_index = loaded_models;
                    if let Some(slot) = mirror_surface_model_slot(&document.stage_id, &asset_path) {
                        mirror_model_slots.insert(model_index, slot);
                    }
                    let texture_base = push_preview_textures(&mut textures, &preview);
                    let material_base =
                        push_preview_materials(&mut materials, &preview, texture_base);
                    material_animation_bindings.resize_with(materials.len(), Vec::new);
                    attach_model_texture_srt_animation(
                        document,
                        &asset_path,
                        material_base,
                        &preview.materials,
                        &mut texture_srt_animations,
                        &mut material_animation_bindings,
                    );
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
                    if !is_sky {
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

                    let point_stride = (preview.positions.len() / POINTS_PER_MODEL).max(1);
                    let point_start = points.len();
                    if !is_sky {
                        for position in preview.positions.iter().step_by(point_stride) {
                            points.push(PreviewPoint {
                                position: *position,
                                model_index,
                            });
                        }
                    }

                    let triangle_start = triangles.len();
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
                                billboard: triangle.billboard,
                                particle_type: None,
                                particle_pivot: None,
                                particle_direction: None,
                                particle_color_mode: None,
                                particle_environment_color: None,
                            });
                        }
                    }
                    if !level_targets.is_empty() {
                        level_transform_models.push(LevelTransformModelPreview {
                            file: file.clone(),
                            loader_flags,
                            targets: level_targets,
                            point_range: point_start..points.len(),
                            point_stride,
                            triangle_range: triangle_start..triangles.len(),
                        });
                    }
                }
                Err(geometry_error) => {
                    let preview = match file.vertex_preview() {
                        Ok(preview) => preview,
                        Err(vertex_error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!(
                                    "decode geometry: {geometry_error}; fallback vertex preview: {vertex_error}"
                                ),
                            );
                            continue;
                        }
                    };

                    loaded_models += 1;
                    let model_index = loaded_models;
                    source_vertices += preview.positions.len();
                    if !is_sky {
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

        let world_triangle_end = triangles.len();
        if visibility.environment {
            let wave = build_procedural_wave_preview(
                document,
                loaded_models + 1,
                next_packet_index,
                &mut textures,
                &mut materials,
            );
            if wave.triangle_count != 0 {
                loaded_models += 1;
                source_vertices += wave.source_vertices;
                source_triangles += wave.triangle_count;
                source_textures += 1;
                triangles.extend(wave.triangles);
                next_packet_index += 1;
                material_animation_bindings.resize_with(materials.len(), Vec::new);
            }

            let grass =
                build_procedural_grass_preview(document, loaded_models + 1, next_packet_index);
            if grass.group_count != 0 {
                loaded_models += grass.group_count;
                source_vertices += grass.blade_count * 3;
                source_triangles += grass.blade_count;
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    grass.bounds_min,
                    grass.bounds_max,
                );
                points.extend(grass.points);
                triangles.extend(grass.triangles);
                object_model_indices.extend(grass.object_model_indices);
                next_packet_index += 1;
            }

            let wires = build_procedural_wire_preview(
                document,
                loaded_models + 1,
                next_packet_index,
                &mut textures,
                &mut materials,
            );
            if wires.wire_count != 0 {
                loaded_models += wires.wire_count;
                source_vertices += wires.source_vertices;
                source_triangles += wires.source_triangles;
                source_textures += wires.source_textures;
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    wires.bounds_min,
                    wires.bounds_max,
                );
                points.extend(wires.points);
                triangles.extend(wires.triangles);
                next_packet_index += wires.packet_count;
                material_animation_bindings.resize_with(materials.len(), Vec::new);
            }

            let flags = build_procedural_flag_preview(
                document,
                loaded_models + 1,
                next_packet_index,
                &mut textures,
                &mut materials,
            );
            if flags.flag_count != 0 {
                let point_base = points.len();
                let triangle_base = triangles.len();
                loaded_models += flags.flag_count;
                source_vertices += flags.source_vertices;
                source_triangles += flags.source_triangles;
                source_textures += flags.source_textures;
                merge_bounds(
                    &mut bounds_min,
                    &mut bounds_max,
                    flags.bounds_min,
                    flags.bounds_max,
                );
                points.extend(flags.points);
                triangles.extend(flags.triangles);
                animated_flags.extend(flags.animated_flags.into_iter().map(|mut animation| {
                    animation.point_range = (animation.point_range.start + point_base)
                        ..(animation.point_range.end + point_base);
                    animation.triangle_range = (animation.triangle_range.start + triangle_base)
                        ..(animation.triangle_range.end + triangle_base);
                    animation
                }));
                object_model_indices.extend(flags.object_model_indices);
                next_packet_index += flags.packet_count;
                material_animation_bindings.resize_with(materials.len(), Vec::new);
            }
        }
        let mut object_model_cache =
            BTreeMap::<(String, u32, String), CachedObjectModelPreview>::new();
        let mut accessory_model_cache = BTreeMap::<String, CachedAccessoryModelPreview>::new();
        for object in &document.objects {
            let explicit_model_path = object_preview_model_path(object, &world_model_paths);
            let catalog_preview = explicit_model_path
                .is_none()
                .then(|| document.actor_preview(object))
                .flatten()
                .filter(|preview| is_supported_object_preview_model_path(&preview.model_path));
            let model_path = explicit_model_path
                .or_else(|| catalog_preview.map(|preview| preview.model_path.clone()))
                .or_else(|| object_inferred_preview_model_path(object, &world_model_paths));
            let Some(model_path) = model_path else {
                continue;
            };
            // TMapStaticObj::initUnique submits ReflectSky only to Sunshine's
            // mirror-sky draw buffers. The editor mirrors the already-loaded
            // main sky instead, so instantiating this helper as a world object
            // produces a second, enormous sky dome at the stage origin.
            if path_is_mirror_sky_helper_model_path(&model_path) {
                continue;
            }
            let loader_flags = catalog_preview
                .map(|preview| preview.load_flags)
                .or_else(|| document.object_preview_load_flags(object))
                .or_else(|| actor_model_loader_flags(object))
                .unwrap_or_else(|| model_loader_flags_for_path(&model_path));
            let object_render_layer = preview_render_layer_for_model_path(&model_path);
            let object_preview_transform = if object_render_layer == PreviewRenderLayer::Heatwave {
                shimmer_preview_transform(object.transform)
            } else {
                reset_fruit_preview_transform(object, object.transform, document.registry.as_ref())
            };
            let animation_profile = std::iter::once(object.factory_name.clone())
                .chain(
                    npc_accessory_specs(document, object)
                        .into_iter()
                        .filter(|part| part.joint_name.is_none())
                        .map(|part| part.asset_suffix.to_ascii_lowercase()),
                )
                .collect::<Vec<_>>()
                .join("|");
            let model_cache_key = (model_path.clone(), loader_flags, animation_profile);

            if !object_model_cache.contains_key(&model_cache_key) {
                if failed_model_assets.contains(&normalized_preview_asset_path(&model_path)) {
                    continue;
                }
                let bytes = match read_stage_asset_bytes(&model_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        record_model_preview_failure(
                            &mut failed_model_assets,
                            &mut model_failures,
                            &model_path,
                            format!("read actor model: {error}"),
                        );
                        continue;
                    }
                };
                let file = match J3dFile::parse(&bytes) {
                    Ok(file) => file,
                    Err(error) => {
                        record_model_preview_failure(
                            &mut failed_model_assets,
                            &mut model_failures,
                            &model_path,
                            format!("parse actor J3D: {error}"),
                        );
                        continue;
                    }
                };
                let joint_animation = starting_joint_animation(document, object, &model_path);
                let prepared_triangles = joint_animation
                    .as_ref()
                    .and_then(|_| file.prepare_animated_triangles().ok())
                    .map(Arc::new);
                let preview_result = joint_animation.as_ref().map_or_else(
                    || file.geometry_preview_with_loader_flags(loader_flags),
                    |animation| {
                        file.geometry_preview_with_joint_animation(loader_flags, animation, 0.0)
                    },
                );
                let mut preview = match preview_result {
                    Ok(preview) => preview,
                    Err(error) => {
                        record_model_preview_failure(
                            &mut failed_model_assets,
                            &mut model_failures,
                            &model_path,
                            format!("decode actor geometry: {error}"),
                        );
                        continue;
                    }
                };
                apply_model_material_table(document, &model_path, loader_flags, &mut preview);
                apply_actor_runtime_textures(document, object, &mut preview);
                apply_npc_eye_decal_culling(object, &mut preview);
                let texture_base = push_preview_textures(&mut textures, &preview);
                let material_base = push_preview_materials(&mut materials, &preview, texture_base);
                let joint_names = file.joint_names().unwrap_or_default();
                object_model_cache.insert(
                    model_cache_key.clone(),
                    CachedObjectModelPreview {
                        file,
                        joint_animation,
                        prepared_triangles,
                        loader_flags,
                        preview,
                        texture_base,
                        material_base,
                        joint_names,
                        instances: Vec::new(),
                    },
                );
            }

            let Some(cached) = object_model_cache.get(&model_cache_key) else {
                continue;
            };
            let object_material_base = push_object_preview_materials(
                &mut materials,
                cached,
                object,
                document.registry.as_ref(),
            );
            material_animation_bindings.resize_with(materials.len(), Vec::new);
            attach_model_texture_srt_animation(
                document,
                &model_path,
                object_material_base,
                &cached.preview.materials,
                &mut texture_srt_animations,
                &mut material_animation_bindings,
            );
            attach_npc_texture_pattern_animation(
                document,
                object,
                &model_path,
                object_material_base,
                cached.texture_base,
                &cached.preview.materials,
                &mut materials,
                &mut texture_pattern_animations,
            );
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

            if object_render_layer != PreviewRenderLayer::Heatwave {
                let transformed_bounds_min =
                    transform_preview_point(cached.preview.bounds_min, object_preview_transform);
                let transformed_bounds_max =
                    transform_preview_point(cached.preview.bounds_max, object_preview_transform);
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
            }

            let point_stride = (cached.preview.positions.len() / POINTS_PER_OBJECT_INSTANCE).max(1);
            let point_start = points.len();
            if object_render_layer != PreviewRenderLayer::Heatwave {
                for position in cached.preview.positions.iter().step_by(point_stride) {
                    points.push(PreviewPoint {
                        position: transform_preview_point(*position, object_preview_transform),
                        model_index,
                    });
                }
            }

            let triangle_start = triangles.len();
            for triangle in &cached.preview.triangles {
                let vertices =
                    transform_preview_vertices(triangle.vertices, object_preview_transform);
                if triangle_vertices_are_finite(vertices) {
                    triangles.push(PreviewTriangle {
                        vertices,
                        normals: triangle.normals.map(|normals| {
                            transform_preview_normals(normals, object_preview_transform)
                        }),
                        color_channels: triangle.color_channels,
                        tex_coord_sets: triangle.tex_coord_sets,
                        material_index: triangle.material_index.and_then(|index| {
                            let global_index = object_material_base + index;
                            (global_index < materials.len()).then_some(global_index)
                        }),
                        packet_index: packet_base + triangle.packet_index,
                        model_index,
                        render_layer: object_render_layer,
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
                        billboard: triangle.billboard.and_then(|billboard| {
                            transform_j3d_billboard(
                                billboard,
                                object_preview_transform,
                                triangle.normals.map(|normals| {
                                    transform_preview_normals(normals, object_preview_transform)
                                }),
                            )
                        }),
                        particle_type: None,
                        particle_pivot: None,
                        particle_direction: None,
                        particle_color_mode: None,
                        particle_environment_color: None,
                    });
                }
            }
            let body_triangle_end = triangles.len();
            let mut accessories = Vec::new();
            let joint_matrices = cached
                .joint_animation
                .as_ref()
                .map_or_else(
                    || cached.file.joint_matrices(cached.loader_flags),
                    |animation| {
                        cached.file.joint_matrices_with_joint_animation(
                            cached.loader_flags,
                            animation,
                            0.0,
                        )
                    },
                )
                .unwrap_or_default();
            push_actor_particle_previews(
                document,
                object,
                &actor_particle_effects,
                &joint_matrices,
                object_preview_transform,
                model_index,
                &mut textures,
                &mut triangles,
                &mut next_packet_index,
                &mut actor_particles,
            );
            for spec in npc_accessory_specs(document, object) {
                let joint_index = match spec.joint_name.as_deref() {
                    Some(joint_name) => {
                        let Some(index) = cached
                            .joint_names
                            .iter()
                            .position(|name| name.eq_ignore_ascii_case(joint_name))
                        else {
                            continue;
                        };
                        Some(index)
                    }
                    None => None,
                };
                let joint_matrix = match joint_index {
                    Some(index) => {
                        let Some(matrix) = joint_matrices.get(index).copied() else {
                            continue;
                        };
                        matrix
                    }
                    None => j3d_identity_matrix(),
                };
                let Some(asset) =
                    find_stage_asset_by_suffix(document, StageAssetKind::Model, &spec.asset_suffix)
                else {
                    continue;
                };
                let asset_path = asset.path.to_string_lossy().replace('\\', "/");
                let accessory_cache_key = normalized_preview_asset_path(&asset_path);
                if !accessory_model_cache.contains_key(&accessory_cache_key) {
                    if failed_model_assets.contains(&accessory_cache_key) {
                        continue;
                    }
                    let bytes = match read_stage_asset_bytes(&asset.path) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!("read accessory model: {error}"),
                            );
                            continue;
                        }
                    };
                    let file = match J3dFile::parse(&bytes) {
                        Ok(file) => file,
                        Err(error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!("parse accessory J3D: {error}"),
                            );
                            continue;
                        }
                    };
                    let loader_flags = 0x1021_0000;
                    let joint_animation =
                        accessory_joint_animation(document, &asset_path).map(Arc::new);
                    let prepared_triangles = joint_animation
                        .as_ref()
                        .and_then(|_| file.prepare_animated_triangles().ok())
                        .map(Arc::new);
                    let preview_result = joint_animation.as_ref().map_or_else(
                        || file.geometry_preview_with_loader_flags(loader_flags),
                        |animation| {
                            file.geometry_preview_with_joint_animation(
                                loader_flags,
                                animation.as_ref(),
                                0.0,
                            )
                        },
                    );
                    let mut preview = match preview_result {
                        Ok(preview) => preview,
                        Err(error) => {
                            record_model_preview_failure(
                                &mut failed_model_assets,
                                &mut model_failures,
                                &asset_path,
                                format!("decode accessory geometry: {error}"),
                            );
                            continue;
                        }
                    };
                    // Parts are ordinary J3D models. Give them the same BMT
                    // resolution as bodies and map objects so dummy textures
                    // (including custom-model placeholders) resolve by asset
                    // names instead of accessory-specific exceptions.
                    apply_model_material_table(document, &asset_path, loader_flags, &mut preview);
                    let file = Arc::new(file);
                    let local_triangles = Arc::new(preview.triangles.clone());
                    let texture_base = push_preview_textures(&mut textures, &preview);
                    let material_base =
                        push_preview_materials(&mut materials, &preview, texture_base);
                    accessory_model_cache.insert(
                        accessory_cache_key.clone(),
                        CachedAccessoryModelPreview {
                            file,
                            joint_animation,
                            prepared_triangles,
                            loader_flags,
                            preview,
                            local_triangles,
                            texture_base,
                            material_base,
                        },
                    );
                }
                let Some(accessory) = accessory_model_cache.get(&accessory_cache_key) else {
                    continue;
                };
                source_vertices += accessory.preview.positions.len();
                source_triangles += accessory.preview.triangles.len();
                source_textures += accessory.preview.textures.len();
                let accessory_packet_base = next_packet_index;
                next_packet_index += accessory
                    .preview
                    .triangles
                    .iter()
                    .map(|triangle| triangle.packet_index)
                    .max()
                    .map(|index| index + 1)
                    .unwrap_or(1);
                let accessory_triangle_start = triangles.len();
                let accessory_material_base =
                    push_accessory_instance_materials(&mut materials, accessory, object, &spec);
                push_attached_preview_triangles(
                    &mut triangles,
                    accessory,
                    joint_matrix,
                    object.transform,
                    model_index,
                    accessory_packet_base,
                    accessory_material_base,
                    materials.len(),
                    textures.len(),
                );
                accessories.push(AnimatedAccessoryInstance {
                    joint_index,
                    file: accessory.file.clone(),
                    joint_animation: accessory.joint_animation.clone(),
                    prepared_triangles: accessory.prepared_triangles.clone(),
                    loader_flags: accessory.loader_flags,
                    local_triangles: accessory.local_triangles.clone(),
                    triangle_range: accessory_triangle_start..triangles.len(),
                });
            }
            if object.factory_name.starts_with("NPC") {
                push_npc_circle_shadow(
                    &mut triangles,
                    object.transform,
                    model_index,
                    next_packet_index,
                );
                next_packet_index += 1;
            }
            if is_coin_object(object) {
                if let Some(ground_y) = shadow_ground_height(
                    object_preview_transform.translation,
                    &triangles[..world_triangle_end],
                ) {
                    push_coin_circle_shadow(
                        &mut triangles,
                        object_preview_transform,
                        ground_y,
                        model_index,
                        next_packet_index,
                    );
                    next_packet_index += 1;
                }
            }
            let instance = AnimatedModelInstance {
                transform: object.transform,
                model_index,
                point_range: point_start..points.len(),
                point_stride,
                triangle_range: triangle_start..body_triangle_end,
                accessories,
                runtime_yaw_degrees_per_frame: runtime_yaw_degrees_per_frame(object),
            };
            if let Some(cached) = object_model_cache.get_mut(&model_cache_key) {
                cached.instances.push(instance);
            }
        }

        let rotating_models = object_model_cache
            .values()
            .filter_map(|cached| {
                let instances = cached
                    .instances
                    .iter()
                    .filter(|instance| instance.runtime_yaw_degrees_per_frame != 0.0)
                    .cloned()
                    .collect::<Vec<_>>();
                (!instances.is_empty()).then(|| RuntimeRotatingModelPreview {
                    positions: Arc::new(cached.preview.positions.clone()),
                    triangles: Arc::new(cached.preview.triangles.clone()),
                    instances,
                })
            })
            .collect::<Vec<_>>();
        let animated_models: Vec<AnimatedModelPreview> = object_model_cache
            .into_values()
            .filter_map(|cached| {
                cached
                    .joint_animation
                    .map(|animation| AnimatedModelPreview {
                        file: cached.file,
                        animation,
                        prepared_triangles: cached.prepared_triangles,
                        loader_flags: cached.loader_flags,
                        instances: cached.instances,
                    })
            })
            .collect();

        push_level_transform_particle_previews(
            document,
            &level_transform_models,
            &mut textures,
            &mut triangles,
            &mut next_packet_index,
            &mut level_transform_particles,
        );
        let level_transform_duration_frames =
            level_transform_duration_frames(&level_transform_particles);
        let level_transform_particle_end_frames =
            level_transform_particle_end_frames(&level_transform_particles)
                .max(level_transform_duration_frames);

        if points.len() > POINT_BUDGET
            && animated_models.is_empty()
            && animated_flags.is_empty()
            && rotating_models.is_empty()
        {
            let stride = (points.len() / POINT_BUDGET).max(1);
            points = points
                .into_iter()
                .step_by(stride)
                .take(POINT_BUDGET)
                .collect();
        }

        material_animation_bindings.resize_with(materials.len(), Vec::new);

        if loaded_models == 0 || (points.is_empty() && triangles.is_empty()) {
            if failed_model_assets.is_empty() {
                return None;
            }
            // Keep a failure-only preview so validation can report the exact
            // assets that failed instead of collapsing the entire result to
            // `None` and losing every diagnostic.
            bounds_min = [0.0; 3];
            bounds_max = [1.0; 3];
            camera_bounds_min = bounds_min;
            camera_bounds_max = bounds_max;
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
            texture_srt_animations,
            texture_pattern_animations,
            material_animation_bindings,
            bounds_min,
            bounds_max,
            camera_bounds_min,
            camera_bounds_max,
            loaded_models,
            failed_models: failed_model_assets.len(),
            model_failures,
            source_vertices,
            source_triangles,
            source_textures,
            object_model_indices,
            mirror_cubes,
            mirror_model_slots,
            animated_models,
            animated_flags,
            rotating_models,
            level_transform_models,
            level_transform_particles,
            actor_particles,
            level_transform_duration_frames,
            level_transform_particle_end_frames,
        })
    }

    fn preview_visibility(&self) -> PreviewVisibility {
        PreviewVisibility {
            environment: self.show_environment_meshes,
            goop: self.show_goop_meshes,
            effects: self.show_effect_meshes,
        }
    }

    // Document lifecycle and edit commands live in document_commands.rs.

    // Camera projection and selection queries live in camera.rs.
}

fn install_style(ctx: &egui::Context) {
    install_japanese_font(ctx);

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

fn install_japanese_font(ctx: &egui::Context) {
    let Some(font_bytes) = japanese_font_candidates()
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
    else {
        return;
    };

    const FONT_NAME: &str = "sms-japanese-fallback";
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_NAME.to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(font_bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(family_fonts) = fonts.families.get_mut(&family) {
            family_fonts.push(FONT_NAME.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn japanese_font_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        ["YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"]
            .into_iter()
            .map(|name| fonts.join(name))
            .collect()
    }

    #[cfg(target_os = "macos")]
    {
        [
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
            "/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansJP-Regular.otf",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
}

fn command_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).strong())
            .fill(egui::Color32::from_rgb(48, 56, 54)),
    )
}

mod preview_assets;
use preview_assets::*;

mod preview_particles;
use preview_particles::*;

mod preview_grass;
use preview_grass::*;

mod preview_flags;
use preview_flags::*;

mod preview_wires;
use preview_wires::*;

mod preview_waves;
use preview_waves::*;

mod npc_accessories;
use npc_accessories::*;

#[allow(clippy::too_many_arguments)]
fn push_attached_preview_triangles(
    triangles: &mut Vec<PreviewTriangle>,
    accessory: &CachedAccessoryModelPreview,
    joint_matrix: J3dMatrix34,
    transform: Transform,
    model_index: usize,
    packet_base: usize,
    material_base: usize,
    material_count: usize,
    texture_count: usize,
) {
    for triangle in &accessory.preview.triangles {
        let vertices = transform_preview_vertices(
            triangle
                .vertices
                .map(|vertex| transform_j3d_matrix_point(joint_matrix, vertex)),
            transform,
        );
        if !triangle_vertices_are_finite(vertices) {
            continue;
        }
        triangles.push(PreviewTriangle {
            vertices,
            normals: triangle.normals.map(|normals| {
                transform_preview_normals(
                    normals.map(|normal| transform_j3d_matrix_normal(joint_matrix, normal)),
                    transform,
                )
            }),
            color_channels: triangle.color_channels,
            tex_coord_sets: triangle.tex_coord_sets,
            material_index: triangle.material_index.and_then(|index| {
                let global_index = material_base + index;
                (global_index < material_count).then_some(global_index)
            }),
            packet_index: packet_base + triangle.packet_index,
            model_index,
            render_layer: PreviewRenderLayer::Main,
            color: triangle.color,
            vertex_colors: triangle.vertex_colors,
            combine_mode: triangle.combine_mode,
            tex_coords: triangle.tex_coords,
            texture_index: triangle.texture_index.and_then(|index| {
                let global_index = accessory.texture_base + index;
                (global_index < texture_count).then_some(global_index)
            }),
            mask_tex_coords: triangle.mask_tex_coords,
            mask_texture_index: triangle.mask_texture_index.and_then(|index| {
                let global_index = accessory.texture_base + index;
                (global_index < texture_count).then_some(global_index)
            }),
            cull_mode: triangle.cull_mode,
            alpha_compare: triangle.alpha_compare,
            blend_mode: triangle.blend_mode,
            z_mode: triangle.z_mode,
            billboard: triangle.billboard.and_then(|billboard| {
                let joint_normals = triangle.normals.map(|normals| {
                    normals.map(|normal| transform_j3d_matrix_normal(joint_matrix, normal))
                });
                let billboard =
                    transform_j3d_billboard_matrix(billboard, joint_matrix, joint_normals)?;
                transform_j3d_billboard(
                    billboard,
                    transform,
                    joint_normals.map(|normals| transform_preview_normals(normals, transform)),
                )
            }),
            particle_type: None,
            particle_pivot: None,
            particle_direction: None,
            particle_color_mode: None,
            particle_environment_color: None,
        });
    }
}

fn j3d_identity_matrix() -> J3dMatrix34 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]
}

fn push_npc_circle_shadow(
    triangles: &mut Vec<PreviewTriangle>,
    transform: Transform,
    model_index: usize,
    packet_index: usize,
) {
    let radius = 60.0 * transform.scale[0].abs().max(transform.scale[2].abs());
    let center = [
        transform.translation[0],
        transform.translation[1] + 1.5,
        transform.translation[2],
    ];
    push_circle_shadow(triangles, center, radius, model_index, packet_index);
}

fn push_coin_circle_shadow(
    triangles: &mut Vec<PreviewTriangle>,
    transform: Transform,
    ground_y: f32,
    model_index: usize,
    packet_index: usize,
) {
    // TMapObjBase::initActorData derives mScaledBodyRadius from the coin's
    // TMapObjData value (50.0) and its X scale before TLiveActor submits the
    // type-0 circle-shadow request.
    let radius = 50.0 * transform.scale[0].abs();
    let center = [
        transform.translation[0],
        ground_y + 1.5,
        transform.translation[2],
    ];
    push_circle_shadow(triangles, center, radius, model_index, packet_index);
}

fn push_circle_shadow(
    triangles: &mut Vec<PreviewTriangle>,
    center: [f32; 3],
    radius: f32,
    model_index: usize,
    packet_index: usize,
) {
    const SEGMENTS: usize = 20;
    let color = [0, 0, 0, 82];
    for segment in 0..SEGMENTS {
        let angle0 = segment as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        let angle1 = (segment + 1) as f32 * std::f32::consts::TAU / SEGMENTS as f32;
        triangles.push(PreviewTriangle {
            vertices: [
                center,
                [
                    center[0] + angle0.cos() * radius,
                    center[1],
                    center[2] + angle0.sin() * radius,
                ],
                [
                    center[0] + angle1.cos() * radius,
                    center[1],
                    center[2] + angle1.sin() * radius,
                ],
            ],
            normals: Some([[0.0, 1.0, 0.0]; 3]),
            color_channels: [Some([color; 3]), None],
            tex_coord_sets: [None; 8],
            material_index: None,
            packet_index,
            model_index,
            render_layer: PreviewRenderLayer::Shadow,
            color: Some(color),
            vertex_colors: Some([color; 3]),
            combine_mode: J3dPreviewCombineMode::VertexOnly,
            tex_coords: None,
            texture_index: None,
            mask_tex_coords: None,
            mask_texture_index: None,
            cull_mode: Some(0),
            alpha_compare: None,
            blend_mode: Some(J3dBlendMode {
                mode: 1,
                src_factor: 4,
                dst_factor: 5,
                logic_op: 3,
            }),
            z_mode: Some(J3dZMode {
                compare_enable: 1,
                func: 3,
                update_enable: 0,
            }),
            billboard: None,
            particle_type: None,
            particle_pivot: None,
            particle_direction: None,
            particle_color_mode: None,
            particle_environment_color: None,
        });
    }
}

fn shadow_ground_height(position: [f32; 3], world_triangles: &[PreviewTriangle]) -> Option<f32> {
    world_triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::Main)
        .filter_map(|triangle| {
            vertical_triangle_height(position[0], position[2], triangle.vertices)
        })
        // TMBindShadowManager starts the retail ground query 30 units above
        // the actor and searches downward, so slightly embedded placements
        // still resolve to the surface above them.
        .filter(|height| *height <= position[1] + 30.0)
        .max_by(f32::total_cmp)
}

fn vertical_triangle_height(x: f32, z: f32, vertices: [[f32; 3]; 3]) -> Option<f32> {
    let [a, b, c] = vertices;
    let denominator = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if denominator.abs() <= f32::EPSILON {
        return None;
    }

    let a_weight = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / denominator;
    let b_weight = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / denominator;
    let c_weight = 1.0 - a_weight - b_weight;
    const EDGE_EPSILON: f32 = 0.0001;
    (a_weight >= -EDGE_EPSILON && b_weight >= -EDGE_EPSILON && c_weight >= -EDGE_EPSILON)
        .then_some(a_weight * a[1] + b_weight * b[1] + c_weight * c[1])
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
    for suffix in [
        "_crash", "crash", "_normal", "normal", "_offset", "offset", "_alpha", "alpha",
    ] {
        if let Some(base_stem) = stem.strip_suffix(suffix) {
            if !base_stem.is_empty() {
                candidates.push(format!("{directory}{base_stem}.bmt"));
            }
            break;
        }
    }
    let model_key = material_table_match_key(stem);
    let shared_table = if model_key.starts_with("miniwindmill")
        || model_key.starts_with("lampbianco")
        || model_key.starts_with("biabell")
        || model_key.starts_with("biawatermill")
        || model_key.starts_with("biadoor")
    {
        Some("bianco")
    } else if model_key.starts_with("riccoship")
        || model_key.starts_with("riccoyacht")
        || model_key.starts_with("riccoboat")
    {
        Some("riccoship")
    } else if model_key.starts_with("sandleafbase")
        || model_key.starts_with("sandbombbase")
        || model_key.starts_with("sandcastle")
    {
        Some("sandbombbase")
    } else {
        None
    };
    if let Some(shared_table) = shared_table {
        let candidate = format!("{directory}{shared_table}.bmt");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    candidates
}

fn material_table_match_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn material_table_asset_score(
    model_path: &str,
    model_textures: &[sms_formats::J3dTexturePreview],
    table_path: &str,
) -> Option<(u8, usize)> {
    let model_path = normalized_preview_asset_path(model_path);
    let table_path = normalized_preview_asset_path(table_path);
    let candidates = material_table_candidates_for_model(&model_path);
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate == &table_path)
    {
        return Some((3, candidates.len().saturating_sub(index)));
    }

    let (model_directory, model_file) = model_path.rsplit_once('/')?;
    let (table_directory, table_file) = table_path.rsplit_once('/')?;
    let same_archive = model_path
        .split_once("!/")
        .zip(table_path.split_once("!/"))
        .is_some_and(|((model_archive, _), (table_archive, _))| model_archive == table_archive);
    if model_directory != table_directory && !same_archive {
        return None;
    }

    let model_stem = model_file
        .rsplit_once('.')
        .map_or(model_file, |(stem, _)| stem);
    let table_stem = table_file
        .rsplit_once('.')
        .map_or(table_file, |(stem, _)| stem);
    let model_key = material_table_match_key(model_stem);
    let table_key = material_table_match_key(table_stem);
    if table_key.is_empty() {
        return None;
    }

    let dummy_texture_match = model_textures.iter().any(|texture| {
        let name = texture.name.to_ascii_lowercase();
        (name.contains("dummy") || name.contains("dammy"))
            && material_table_match_key(&name).contains(&table_key)
    });
    let dummy_model_match = model_directory == table_directory
        && model_textures.iter().any(|texture| {
            let name = texture.name.to_ascii_lowercase();
            name.contains("dummy") || name.contains("dammy")
        })
        && model_key.contains(&table_key);

    (dummy_texture_match || dummy_model_match).then_some((2, table_key.len()))
}

mod npc_materials;
use npc_materials::*;

mod preview_geometry;
use preview_geometry::*;

fn normalized_preview_asset_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn is_supported_object_preview_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    (path.ends_with(".bmd") || path.ends_with(".bdl"))
        && (path.contains("!/") || path.contains("/scene/"))
}

fn should_instance_object_preview_model(path: &str, world_model_paths: &BTreeSet<String>) -> bool {
    is_supported_object_preview_model_path(path)
        && !world_model_paths.contains(&normalized_preview_asset_path(path))
}

fn object_preview_model_path(
    object: &SceneObject,
    world_model_paths: &BTreeSet<String>,
) -> Option<String> {
    object_preview_model_path_for_role(object, world_model_paths, AssetRole::PreviewModel)
}

fn object_inferred_preview_model_path(
    object: &SceneObject,
    world_model_paths: &BTreeSet<String>,
) -> Option<String> {
    object_preview_model_path_for_role(object, world_model_paths, AssetRole::InferredPreviewModel)
}

fn object_preview_model_path_for_role(
    object: &SceneObject,
    world_model_paths: &BTreeSet<String>,
    role: AssetRole,
) -> Option<String> {
    object
        .asset_hints
        .iter()
        .find(|hint| hint.role == role)
        .map(|hint| hint.path.as_str())
        .filter(|path| should_instance_object_preview_model(path, world_model_paths))
        .map(str::to_owned)
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
        .raw_param("name")
        .map(str::to_owned)
        .unwrap_or_else(|| object.factory_name.clone())
}

fn labeled_text(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
    });
}

mod startup;
use startup::*;
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

fn clip_world_segment_to_near_plane(
    camera: CameraFrame,
    start: [f32; 3],
    end: [f32; 3],
    near: f32,
) -> Option<[[f32; 3]; 2]> {
    let mut clipped = [start, end];
    let mut depths = [
        vec3_dot(vec3_sub(start, camera.position), camera.forward),
        vec3_dot(vec3_sub(end, camera.position), camera.forward),
    ];
    if !near.is_finite() || depths.iter().any(|depth| !depth.is_finite()) {
        return None;
    }
    if depths[0] < near && depths[1] < near {
        return None;
    }

    for endpoint in 0..2 {
        if depths[endpoint] >= near {
            continue;
        }
        let other = 1 - endpoint;
        let depth_span = depths[other] - depths[endpoint];
        if depth_span.abs() <= f32::EPSILON {
            return None;
        }
        let t = ((near - depths[endpoint]) / depth_span).clamp(0.0, 1.0);
        clipped[endpoint] = vec3_add(
            clipped[endpoint],
            vec3_scale(vec3_sub(clipped[other], clipped[endpoint]), t),
        );
        depths[endpoint] = near;
    }

    Some(clipped)
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

fn expand_model_preview_bounds(
    preview: &mut ModelPreview,
    model_index: usize,
    triangle_ranges: &[std::ops::Range<usize>],
) {
    let mut bounds_min = [f32::INFINITY; 3];
    let mut bounds_max = [f32::NEG_INFINITY; 3];
    let mut found = false;
    for range in triangle_ranges {
        for triangle in preview.triangles[range.clone()].iter().filter(|triangle| {
            !matches!(
                triangle.render_layer,
                PreviewRenderLayer::Sky
                    | PreviewRenderLayer::MirrorScene
                    | PreviewRenderLayer::Heatwave
            )
        }) {
            for vertex in triangle.vertices {
                if !vertex.iter().all(|value| value.is_finite()) {
                    continue;
                }
                found = true;
                for axis in 0..3 {
                    bounds_min[axis] = bounds_min[axis].min(vertex[axis]);
                    bounds_max[axis] = bounds_max[axis].max(vertex[axis]);
                }
            }
        }
    }
    if !found {
        for point in preview
            .points
            .iter()
            .filter(|point| point.model_index == model_index)
        {
            if !point.position.iter().all(|value| value.is_finite()) {
                continue;
            }
            found = true;
            for axis in 0..3 {
                bounds_min[axis] = bounds_min[axis].min(point.position[axis]);
                bounds_max[axis] = bounds_max[axis].max(point.position[axis]);
            }
        }
    }
    if !found {
        return;
    }

    for axis in 0..3 {
        let padding = ((bounds_max[axis] - bounds_min[axis]) * 0.06).max(120.0);
        preview.bounds_min[axis] = preview.bounds_min[axis].min(bounds_min[axis] - padding);
        preview.bounds_max[axis] = preview.bounds_max[axis].max(bounds_max[axis] + padding);
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
    let has_world_triangles = triangles.iter().any(|triangle| {
        !matches!(
            triangle.render_layer,
            PreviewRenderLayer::Sky
                | PreviewRenderLayer::MirrorScene
                | PreviewRenderLayer::Heatwave
        )
    });
    if !has_world_triangles {
        for point in points {
            for (axis, value) in axes.iter_mut().zip(point.position) {
                if value.is_finite() {
                    axis.push(value);
                }
            }
        }
    } else {
        for triangle in triangles.iter().filter(|triangle| {
            !matches!(
                triangle.render_layer,
                PreviewRenderLayer::Sky
                    | PreviewRenderLayer::MirrorScene
                    | PreviewRenderLayer::Heatwave
            )
        }) {
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

mod software_renderer;
use software_renderer::*;

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
    if path_is_mirror_sky_helper_model_path(&path) {
        return false;
    }
    if path_is_sky_model_path(&path) {
        return true;
    }
    if path_is_sea_indirect_model_path(&path) {
        return show_environment_meshes;
    }
    if path_is_indirect_water_model_path(&path) {
        return false;
    }
    if path_is_mirror_surface_model_path(&path) || path_is_water_reflection_model_path(&path) {
        return show_environment_meshes;
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
    if path_is_shimmer_model_path(&path) {
        PreviewRenderLayer::Heatwave
    } else if path_is_mirror_sky_helper_model_path(&path) {
        PreviewRenderLayer::MirrorScene
    } else if path_is_sea_indirect_model_path(&path) {
        PreviewRenderLayer::IndirectWater
    } else if path_is_water_reflection_model_path(&path) {
        PreviewRenderLayer::MirrorScene
    } else if path_is_mirror_surface_model_path(&path) {
        PreviewRenderLayer::MirrorSurface
    } else if path_is_sky_model_path(&path) {
        PreviewRenderLayer::Sky
    } else if path_is_goop_model_path(&path) {
        PreviewRenderLayer::Goop
    } else if path_is_water_model_path(&path) {
        PreviewRenderLayer::Water
    } else {
        PreviewRenderLayer::Main
    }
}

fn path_is_shimmer_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/').next().is_some_and(|name| {
        matches!(
            name,
            "shimmerlow.bmd"
                | "shimmerlow.bdl"
                | "shimmerlowfar.bmd"
                | "shimmerlowfar.bdl"
                | "shimmerhi.bmd"
                | "shimmerhi.bdl"
                | "shimmerhifar.bmd"
                | "shimmerhifar.bdl"
        )
    })
}

fn shimmer_preview_transform(transform: Transform) -> Transform {
    // TShimmer::perform builds inverse(view) * translate(runtime position) *
    // scale. In normal outdoor preview state that runtime position is zero;
    // the placement translation and rotation are not part of the draw matrix.
    Transform {
        translation: [0.0; 3],
        rotation_degrees: [0.0; 3],
        scale: transform.scale,
    }
}

fn reset_fruit_preview_transform(
    object: &SceneObject,
    mut transform: Transform,
    registry: Option<&ObjectRegistry>,
) -> Transform {
    if object.factory_name != "ResetFruit" {
        return transform;
    }

    let Some(resource_name) = object
        .raw_param("actor_tail_string")
        .or_else(|| object.raw_param("stream_string_0"))
    else {
        return transform;
    };
    let Some(definition) = registry.and_then(|registry| {
        registry
            .find_map_obj_resource(resource_name)
            .and_then(|resource| registry.find_map_obj_ball_transform(resource.actor_type))
    }) else {
        return transform;
    };

    // TMapObjBall::initMapObj sets the fruit-specific body radius, then
    // TResetFruit::makeObjAppeared places the model at position.y + radius.
    // Banana and pineapple apply the additional matrix corrections below.
    let [rotation_x, rotation_y, rotation_z] = transform.rotation_degrees.map(f32::to_radians);
    let matrix_y_axis_y = transform.scale[1]
        * (rotation_x.sin() * rotation_y.sin() * rotation_z.sin()
            + rotation_z.cos() * rotation_x.cos());
    let mut y_offset = f32::from(definition.body_radius) * transform.scale[1];
    if let Some(correction) = definition.positive_y_axis_subtract {
        if matrix_y_axis_y > 0.0 {
            y_offset -= f32::from(correction) * matrix_y_axis_y;
        }
    } else if let Some(correction) = definition.one_minus_y_axis_subtract {
        y_offset -= f32::from(correction) * (1.0 - matrix_y_axis_y);
    }
    transform.translation[1] += y_offset;
    transform
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
    let model_name = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".bmd")
        .trim_end_matches(".bdl");
    matches!(model_name, "sea" | "biancoriver" | "monteriver")
        || path.contains("/map/map/water")
        || path.contains("/map/water/")
        || path.contains("/map/mirror/puddle")
        || path.contains("/map/map/puddle")
        || path.contains("/map/map/yogan")
        || path.contains("/map/map/lava")
}

fn path_is_water_reflection_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| matches!(name, "reflectparts.bmd" | "reflectparts.bdl"))
}

fn path_is_mirror_sky_helper_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| matches!(name, "reflectsky.bmd" | "reflectsky.bdl"))
}

fn path_is_mirror_surface_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if !path.contains("/map/mirror/") {
        return false;
    }
    path.rsplit('/').next().is_some_and(|name| {
        let stem = name.trim_end_matches(".bmd").trim_end_matches(".bdl");
        stem.strip_prefix("mirror")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
    })
}

fn mirror_cubes(document: &StageDocument) -> Vec<PreviewMirrorCube> {
    const MIRROR_CUBE_TABLE_NAME: &str = "鏡キューブテーブル";

    let mut cubes = Vec::new();
    for asset in document.assets.iter().filter(|asset| {
        asset.kind == StageAssetKind::Placement
            && asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/tables.bin")
    }) {
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(records) = parse_jdrama_object_records(&bytes) else {
            continue;
        };
        let mirror_tables = records
            .iter()
            .filter(|record| {
                record.type_name.rsplit("::").next() == Some("CubeGeneralInfoTable")
                    && record.object_name.as_deref() == Some(MIRROR_CUBE_TABLE_NAME)
            })
            .map(|record| record.offset..record.offset + record.size)
            .collect::<Vec<_>>();
        for cube in records.iter().filter(|record| {
            record.cube_general_info.is_some()
                && mirror_tables.iter().any(|table| {
                    record.offset > table.start && record.offset + record.size <= table.end
                })
        }) {
            let Some(info) = cube.cube_general_info else {
                continue;
            };
            let Ok(model_slot) = usize::try_from(info.data_no) else {
                continue;
            };
            let preview_cube = PreviewMirrorCube {
                center: info.center,
                rotation_degrees: info.rotation_degrees,
                dimensions: info.dimensions,
                model_slot,
            };
            if preview_cube
                .center
                .iter()
                .chain(preview_cube.rotation_degrees.iter())
                .chain(preview_cube.dimensions.iter())
                .all(|value| value.is_finite())
                && preview_cube
                    .dimensions
                    .iter()
                    .all(|dimension| *dimension > 0.0)
            {
                cubes.push(preview_cube);
            }
        }
    }
    cubes
}

#[cfg(test)]
fn active_mirror_model_slots(document: &StageDocument) -> BTreeSet<usize> {
    mirror_cubes(document)
        .into_iter()
        .map(|cube| cube.model_slot)
        .collect()
}

fn mirror_surface_model_is_active(
    stage_id: &str,
    path: &str,
    active_slots: &BTreeSet<usize>,
) -> bool {
    mirror_surface_model_slot(stage_id, path).is_none_or(|slot| active_slots.contains(&slot))
}

fn mirror_surface_model_slot(stage_id: &str, path: &str) -> Option<usize> {
    if !path_is_mirror_surface_model_path(path) {
        return None;
    }
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let stem = path
        .rsplit('/')
        .next()?
        .trim_end_matches(".bmd")
        .trim_end_matches(".bdl");
    let suffix = stem.strip_prefix("mirror")?;
    if stage_id.to_ascii_lowercase().starts_with("pinna") && suffix == "205" {
        // TMirrorModelManager maps Pinna Park's first mirror slot to mirror205.
        Some(0)
    } else {
        suffix.parse().ok()
    }
}

fn path_is_indirect_water_model_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("seaindirect") || path.contains("puddle_ind")
}

fn path_is_sea_indirect_model_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| matches!(name, "seaindirect.bmd" | "seaindirect.bdl"))
}

fn path_is_goop_model_path(path: &str) -> bool {
    path.contains("/map/pollution/") || path.contains("pollution")
}

fn map_static_model_is_active(document: &StageDocument, model_path: &str) -> bool {
    let Some(registry) = document.registry.as_ref() else {
        return true;
    };
    let model_path = normalized_preview_asset_path(model_path);
    let matching_definitions = registry
        .map_static_models
        .iter()
        .filter(|definition| map_static_definition_matches_asset(definition, &model_path))
        .collect::<Vec<_>>();
    if matching_definitions.is_empty() {
        return true;
    }
    if matching_definitions
        .iter()
        .any(|definition| definition.stage_bootstrap_created)
    {
        return true;
    }

    matching_definitions
        .iter()
        .any(|definition| map_static_definition_is_active(document, definition))
}

fn map_static_model_loader_flags(document: &StageDocument, model_path: &str) -> Option<u32> {
    let registry = document.registry.as_ref()?;
    let model_path = normalized_preview_asset_path(model_path);
    let flags = registry
        .map_static_models
        .iter()
        .filter(|definition| map_static_definition_matches_asset(definition, &model_path))
        .filter(|definition| map_static_definition_is_active(document, definition))
        .map(|definition| definition.load_flags)
        .collect::<BTreeSet<_>>();
    (flags.len() == 1).then(|| *flags.first().expect("one map-static loader flag"))
}

fn map_static_definition_matches_asset(
    definition: &sms_schema::MapStaticModelDefinition,
    model_path: &str,
) -> bool {
    definition
        .model_path
        .as_deref()
        .is_some_and(|runtime_path| runtime_model_path_matches_asset(runtime_path, model_path))
}

fn map_static_definition_is_active(
    document: &StageDocument,
    definition: &sms_schema::MapStaticModelDefinition,
) -> bool {
    definition.stage_bootstrap_created
        || document.objects.iter().any(|object| {
            object.factory_name == "MapStaticObj"
                && object
                    .raw_param("actor_tail_string")
                    .or_else(|| object.raw_param("stream_string_0"))
                    .is_some_and(|name| name == definition.actor_name)
        })
}

fn runtime_model_path_matches_asset(runtime_path: &str, asset_path: &str) -> bool {
    let runtime_path = runtime_path.replace('\\', "/").to_ascii_lowercase();
    let asset_path = asset_path.replace('\\', "/").to_ascii_lowercase();
    if asset_path.ends_with(&runtime_path) {
        return true;
    }
    let mounted_path = asset_path
        .split_once("!/")
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_else(|| asset_path.clone());
    runtime_path.ends_with(&mounted_path)
}

fn active_pollution_layer_count(document: &StageDocument) -> usize {
    document
        .assets
        .iter()
        .find(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("/map/ymap.ymp")
        })
        .and_then(|asset| read_stage_asset_bytes(&asset.path).ok())
        .and_then(|bytes| pollution_layer_count_from_bytes(&bytes))
        .unwrap_or(0) as usize
}

fn pollution_layer_count_from_bytes(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn pollution_layer_model_is_active(path: &str, active_layer_count: usize) -> bool {
    pollution_layer_model_index(path).is_none_or(|index| index < active_layer_count)
}

fn pollution_layer_model_index(path: &str) -> Option<usize> {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if !path.contains("/map/pollution/") {
        return None;
    }

    let stem = path
        .rsplit('/')
        .next()?
        .strip_suffix(".bmd")
        .or_else(|| path.rsplit('/').next()?.strip_suffix(".bdl"))?;
    let suffix = stem.strip_prefix("pollution")?;
    match suffix {
        "a" => Some(7),
        "b" => Some(8),
        digits if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            digits.parse().ok()
        }
        _ => None,
    }
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
        // TShimmer::load passes 0x11010000 so the authored indirect block is
        // retained and can displace the screen-copy texture.
        "shimmerlow" | "shimmerlowfar" | "shimmerhi" | "shimmerhifar" => 0x1101_0000,
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

fn actor_model_loader_flags(object: &SceneObject) -> Option<u32> {
    Some(match object.factory_name.as_str() {
        "NPCMonteM" | "NPCMonteMA" | "NPCMonteMC" | "NPCMonteW" | "NPCMonteWA" => 0x1030_0000,
        "NPCMonteMB" | "NPCMonteMD" | "NPCMonteMF" | "NPCMonteMG" | "NPCMonteMH" | "NPCMonteWB"
        | "NPCMonteWC" => 0x1021_0000,
        "NPCMonteME" => 0x1001_0000,
        // TMareMBaseManager/TMareWBaseManager::createModelData use these flags
        // for the shared Mare body models and their material programs.
        name if name.starts_with("NPCMareM") || name.starts_with("NPCMareW") => 0x1030_0000,
        _ => return None,
    })
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
        PreviewRenderLayer::Water | PreviewRenderLayer::MirrorSurface => {
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
        PreviewRenderLayer::Sky
        | PreviewRenderLayer::Main
        | PreviewRenderLayer::WaveFoam
        | PreviewRenderLayer::IndirectWater
        | PreviewRenderLayer::MirrorScene
        | PreviewRenderLayer::Shadow
        | PreviewRenderLayer::Heatwave
        | PreviewRenderLayer::Particle
        | PreviewRenderLayer::ParticleDistortion => color,
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

#[derive(Debug, Default, Clone, Copy)]
struct VectorDragResponse {
    changed: bool,
    started: bool,
    stopped: bool,
}

impl VectorDragResponse {
    fn merge(&mut self, other: Self) {
        self.changed |= other.changed;
        self.started |= other.started;
        self.stopped |= other.stopped;
    }
}

fn vector_drag(ui: &mut egui::Ui, values: &mut [f32; 3], speed: f32) -> VectorDragResponse {
    let mut edit = VectorDragResponse::default();
    ui.horizontal(|ui| {
        for (label, value) in ["X", "Y", "Z"].iter().zip(values.iter_mut()) {
            ui.label(*label);
            let response = ui.add(egui::DragValue::new(value).speed(speed));
            edit.changed |= response.changed();
            edit.started |= response.drag_started();
            edit.stopped |= response.drag_stopped();
        }
    });
    edit
}

fn runtime_yaw_degrees_per_frame(object: &SceneObject) -> f32 {
    let factory = object.factory_name.as_str();
    let class = object.class_name.as_deref().unwrap_or_default();

    // TItemManager::perform advances its shared item matrix by two degrees
    // every retail frame. TItem::calc copies that matrix into coins, including
    // the red, blue, flower, and joint-attached TCoin variants.
    let is_coin = is_coin_object(object);
    // TShine::control advances the normal live state by unk16C, whose
    // constructor default is also two degrees per retail frame.
    if is_coin || factory == "Shine" || class == "TShine" {
        2.0
    } else {
        0.0
    }
}

fn is_coin_object(object: &SceneObject) -> bool {
    let factory = object.factory_name.as_str();
    let class = object.class_name.as_deref().unwrap_or_default();
    let resource = object
        .raw_param("actor_tail_string")
        .or_else(|| object.raw_param("stream_string_0"))
        .map(str::to_ascii_lowercase);
    if resource.as_deref() == Some("invisible_coin") {
        return false;
    }

    matches!(
        factory,
        "coin" | "CoinRed" | "CoinBlue" | "coin_red" | "coin_blue" | "FlowerCoin" | "joint_coin"
    ) || class.starts_with("TCoin")
        || class == "Coin"
        || class == "CoinRed"
        || class == "CoinBlue"
        || class == "TFlowerCoin"
        || matches!(
            resource.as_deref(),
            Some("coin" | "coin_red" | "coin_blue" | "joint_coin")
        )
}

fn runtime_rotated_transform(
    mut transform: Transform,
    elapsed_seconds: f32,
    yaw_degrees_per_frame: f32,
) -> Transform {
    transform.rotation_degrees[1] = (transform.rotation_degrees[1]
        + elapsed_seconds.max(0.0) * SMS_ANIMATION_FRAMES_PER_SECOND * yaw_degrees_per_frame)
        .rem_euclid(360.0);
    transform
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
mod noki_render_test;

#[cfg(test)]
mod tests;
