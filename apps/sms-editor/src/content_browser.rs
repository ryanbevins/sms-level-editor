use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use sms_formats::{GameFileId, GameFileMetadata, GameResourceKind};

use super::*;
use crate::ui_panels::object_palette_display_name;

const SOURCE_TREE_COLLAPSE_WIDTH: f32 = 760.0;
const GRID_ROW_HEIGHT: f32 = 122.0;
const LIST_ROW_HEIGHT: f32 = 38.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ContentNode {
    #[default]
    All,
    Favorites,
    Recent,
    ProjectStages,
    ProjectModels,
    GameStages,
    GameObjects,
    GameSkyboxes,
    GameMusic,
    GameSounds,
    GameFiles,
}

impl ContentNode {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Favorites => "favorites",
            Self::Recent => "recent",
            Self::ProjectStages => "project-stages",
            Self::ProjectModels => "project-models",
            Self::GameStages => "game-stages",
            Self::GameObjects => "game-objects",
            Self::GameSkyboxes => "game-skyboxes",
            Self::GameMusic => "game-music",
            Self::GameSounds => "game-sounds",
            Self::GameFiles => "game-files",
        }
    }

    pub(super) fn from_key(key: &str) -> Self {
        match key {
            "favorites" => Self::Favorites,
            "recent" => Self::Recent,
            "project-stages" => Self::ProjectStages,
            "project-models" => Self::ProjectModels,
            "game-stages" => Self::GameStages,
            "game-objects" => Self::GameObjects,
            "game-skyboxes" => Self::GameSkyboxes,
            "game-music" => Self::GameMusic,
            "game-sounds" => Self::GameSounds,
            "game-files" => Self::GameFiles,
            _ => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All Content",
            Self::Favorites => "Favorites",
            Self::Recent => "Recent",
            Self::ProjectStages => "Stages",
            Self::ProjectModels => "Model Assets",
            Self::GameStages => "Stages",
            Self::GameObjects => "Objects",
            Self::GameSkyboxes => "Skyboxes",
            Self::GameMusic => "Music",
            Self::GameSounds => "Sounds",
            Self::GameFiles => "Game Files",
        }
    }

    fn breadcrumb(self) -> &'static str {
        match self {
            Self::All | Self::Favorites | Self::Recent => self.label(),
            Self::ProjectStages => "Project Content / Stages",
            Self::ProjectModels => "Project Content / Model Assets",
            Self::GameStages => "Game Content / Stages",
            Self::GameObjects => "Game Content / Objects",
            Self::GameSkyboxes => "Game Content / Skyboxes",
            Self::GameMusic => "Game Content / Music",
            Self::GameSounds => "Game Content / Sounds",
            Self::GameFiles => "Game Content / Game Files",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ContentKind {
    Folder,
    Stage,
    Object,
    Model,
    Skybox,
    Music,
    Sound,
    GameFile,
}

impl ContentKind {
    fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder",
            Self::Stage => "Stage",
            Self::Object => "Object",
            Self::Model => "Model",
            Self::Skybox => "Skybox",
            Self::Music => "Music",
            Self::Sound => "Sound",
            Self::GameFile => "Game File",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContentItemId {
    GameFolder(Vec<String>),
    Stage {
        stage_id: String,
        project_scope: Option<String>,
    },
    Object(String),
    Model(sms_authoring::AssetId),
    Skybox {
        stage_id: String,
        archive_path: String,
    },
    Music {
        bgm_id: u32,
        wave_scene_id: u32,
    },
    Sound(u32),
    GameFile(String),
}

impl ContentItemId {
    pub(super) fn stable_key(&self) -> String {
        match self {
            Self::GameFolder(path) => format!("game:folder:{}", path.join("/")),
            Self::Stage {
                stage_id,
                project_scope,
            } => match project_scope {
                Some(project) => {
                    format!("project:{project}:stage:{}", stage_id.to_ascii_lowercase())
                }
                None => format!("game:stage:{}", stage_id.to_ascii_lowercase()),
            },
            Self::Object(factory) => format!("game:object:{}", factory.to_ascii_lowercase()),
            Self::Model(id) => format!("project:model:{id}"),
            Self::Skybox {
                stage_id,
                archive_path,
            } => format!(
                "game:skybox:{}:{}",
                stage_id.to_ascii_lowercase(),
                normalize_virtual_path(archive_path)
            ),
            Self::Music {
                bgm_id,
                wave_scene_id,
            } => format!("game:music:{bgm_id:08x}:{wave_scene_id:x}"),
            Self::Sound(id) => format!("game:sound:{id:08x}"),
            Self::GameFile(id) => format!("game:file:{id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ContentCapabilities {
    open: bool,
    place: bool,
    apply: bool,
    assign: bool,
    preview: bool,
    inspect: bool,
    editable: bool,
}

impl ContentCapabilities {
    fn supports(self, capability: ContentCapability) -> bool {
        match capability {
            ContentCapability::Open => self.open,
            ContentCapability::Place => self.place,
            ContentCapability::Apply => self.apply,
            ContentCapability::Assign => self.assign,
            ContentCapability::Preview => self.preview,
            ContentCapability::Inspect => self.inspect,
            ContentCapability::Editable => self.editable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentCapability {
    Open,
    Place,
    Apply,
    Assign,
    Preview,
    Inspect,
    Editable,
}

impl ContentCapability {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Place => "Placeable",
            Self::Apply => "Applicable",
            Self::Assign => "Assignable",
            Self::Preview => "Previewable",
            Self::Inspect => "Inspectable",
            Self::Editable => "Editable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ContentSource {
    Project,
    Game,
}

#[derive(Debug, Clone)]
struct ContentItemSummary {
    id: ContentItemId,
    kind: ContentKind,
    source: ContentSource,
    title: String,
    subtitle: String,
    detail: String,
    search_text: String,
    capabilities: ContentCapabilities,
    selected_in_scene: bool,
}

impl ContentItemSummary {
    fn stable_key(&self) -> String {
        self.id.stable_key()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawFilterKey {
    revision: u64,
    node: ContentNode,
    query: String,
    kind: Option<GameResourceKind>,
    source: Option<ContentSource>,
    content_kind: Option<ContentKind>,
    capability: Option<ContentCapability>,
    sort: BrowserSortPreference,
}

#[derive(Debug)]
struct RawFilterCache {
    key: RawFilterKey,
    indices: Arc<[usize]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GameFileDirectoryEntry {
    Folder(Vec<String>),
    File(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawDirectoryKey {
    filter: RawFilterKey,
    path: Vec<String>,
}

#[derive(Debug)]
struct RawDirectoryCache {
    key: RawDirectoryKey,
    entries: Arc<[GameFileDirectoryEntry]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContentBrowserLocation {
    node: ContentNode,
    raw_kind_filter: Option<GameResourceKind>,
    game_file_path: Vec<String>,
    model_folder: Option<String>,
}

#[derive(Debug)]
pub(super) struct ContentBrowserState {
    pub(super) node: ContentNode,
    pub(super) query: String,
    pub(super) selected: Option<ContentItemId>,
    pub(super) inspector_active: bool,
    source_filter: Option<ContentSource>,
    type_filter: Option<ContentKind>,
    capability_filter: Option<ContentCapability>,
    raw_kind_filter: Option<GameResourceKind>,
    pub(super) settings: BrowserSettings,
    pub(super) show_import_settings: bool,
    pub(super) show_new_folder: bool,
    raw_filter_cache: Option<RawFilterCache>,
    raw_directory_cache: Option<RawDirectoryCache>,
    game_file_path: Vec<String>,
    location_back: Vec<ContentBrowserLocation>,
    location_forward: Vec<ContentBrowserLocation>,
    preview_item: Option<ContentItemId>,
}

impl Default for ContentBrowserState {
    fn default() -> Self {
        #[cfg(test)]
        let settings = BrowserSettings::default();
        #[cfg(not(test))]
        let (settings, _) = BrowserSettings::load_default();
        let (collection, raw_kind_filter) = settings
            .last_collection
            .split_once('/')
            .map_or((settings.last_collection.as_str(), None), |(node, kind)| {
                (node, game_resource_kind_from_key(kind))
            });
        Self {
            node: ContentNode::from_key(collection),
            query: String::new(),
            selected: None,
            inspector_active: false,
            source_filter: None,
            type_filter: None,
            capability_filter: None,
            raw_kind_filter,
            settings,
            show_import_settings: false,
            show_new_folder: false,
            raw_filter_cache: None,
            raw_directory_cache: None,
            game_file_path: Vec::new(),
            location_back: Vec::new(),
            location_forward: Vec::new(),
            preview_item: None,
        }
    }
}

#[derive(Debug, Clone)]
enum ContentBrowserCommand {
    OpenGameFolder(Vec<String>),
    OpenStage(String),
    ArmObject(String),
    SpawnObject(String),
    ArmModel(sms_authoring::AssetId),
    SpawnModel(sms_authoring::AssetId),
    EditModel(sms_authoring::AssetId),
    ApplySkybox(RetailSkyboxEntry),
    ApplySkyboxPath(String),
    PreviewMusic(u32),
    PreviewSound(u32),
    PreviewItem(ContentItemId),
    Inspect,
    AssignMusic(RetailMusicEntry),
    AssignSound(u32),
    PreviewGameFile(String),
    CopyText(String),
}

impl SmsEditorApp {
    pub(super) fn content_browser_panel(&mut self, ui: &mut egui::Ui) {
        self.content_browser_mouse_navigation(ui);
        let wide = ui.available_width() >= SOURCE_TREE_COLLAPSE_WIDTH;
        self.content_browser_toolbar(ui, !wide);
        ui.separator();

        if self.content_browser.node == ContentNode::All
            && self.content_browser.query.trim().is_empty()
            && self.content_browser.source_filter.is_none()
            && self.content_browser.type_filter.is_none()
            && self.content_browser.capability_filter.is_none()
        {
            if wide {
                self.content_browser_wide_body(ui, |app, ui| {
                    app.content_browser_landing(ui);
                });
            } else {
                self.content_browser_landing(ui);
            }
            return;
        }

        if self.content_browser.node == ContentNode::GameFiles {
            if wide {
                self.content_browser_wide_body(ui, |app, ui| {
                    app.content_browser_raw_results(ui);
                });
            } else {
                self.content_browser_raw_results(ui);
            }
            return;
        }

        if matches!(
            self.content_browser.node,
            ContentNode::All | ContentNode::Favorites | ContentNode::Recent
        ) {
            if wide {
                self.content_browser_wide_body(ui, |app, ui| {
                    app.content_browser_combined_results(ui);
                });
            } else {
                self.content_browser_combined_results(ui);
            }
            return;
        }

        let items = self.filtered_content_items();
        if wide {
            self.content_browser_wide_body(ui, |app, ui| {
                app.content_browser_results(ui, &items);
            });
        } else {
            self.content_browser_results(ui, &items);
        }
    }

    fn content_browser_wide_body(
        &mut self,
        ui: &mut egui::Ui,
        results: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let body_size = ui.available_size();
        ui.allocate_ui_with_layout(
            body_size,
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                self.content_browser_source_tree(ui);
                self.content_browser_tree_splitter(ui);

                let results_size = ui.available_size();
                ui.allocate_ui_with_layout(
                    results_size,
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| results(self, ui),
                );
            },
        );
    }

    fn content_browser_mouse_navigation(&mut self, ui: &egui::Ui) {
        let (inside, back, forward) = ui.input(|input| {
            let inside = input
                .pointer
                .hover_pos()
                .is_some_and(|position| ui.max_rect().contains(position));
            (
                inside,
                input.pointer.button_pressed(egui::PointerButton::Extra1),
                input.pointer.button_pressed(egui::PointerButton::Extra2),
            )
        });
        if !inside {
            return;
        }
        if back {
            self.navigate_content_back();
        } else if forward {
            self.navigate_content_forward();
        }
    }

    fn current_content_location(&self) -> ContentBrowserLocation {
        ContentBrowserLocation {
            node: self.content_browser.node,
            raw_kind_filter: self.content_browser.raw_kind_filter,
            game_file_path: self.content_browser.game_file_path.clone(),
            model_folder: self.model_folder_filter.clone(),
        }
    }

    fn restore_content_location(&mut self, location: ContentBrowserLocation) {
        self.content_browser.node = location.node;
        self.content_browser.raw_kind_filter = location.raw_kind_filter;
        self.content_browser.game_file_path = location.game_file_path;
        self.model_folder_filter = location.model_folder;
        self.content_browser.source_filter = None;
        self.content_browser.type_filter = None;
        self.content_browser.capability_filter = None;
        self.content_browser.raw_filter_cache = None;
        self.content_browser.raw_directory_cache = None;
        self.content_browser.settings.last_collection = self.content_browser_collection_key();
        self.persist_content_browser_settings();
    }

    fn navigate_content_to(&mut self, location: ContentBrowserLocation) {
        let current = self.current_content_location();
        if current == location {
            return;
        }
        self.content_browser.location_back.push(current);
        self.content_browser.location_forward.clear();
        self.restore_content_location(location);
    }

    fn navigate_game_files_to(&mut self, path: Vec<String>) {
        let mut location = self.current_content_location();
        location.node = ContentNode::GameFiles;
        location.game_file_path = path;
        self.navigate_content_to(location);
    }

    fn navigate_content_back(&mut self) {
        let Some(location) = self.content_browser.location_back.pop() else {
            return;
        };
        self.content_browser
            .location_forward
            .push(self.current_content_location());
        self.restore_content_location(location);
    }

    fn navigate_content_forward(&mut self) {
        let Some(location) = self.content_browser.location_forward.pop() else {
            return;
        };
        self.content_browser
            .location_back
            .push(self.current_content_location());
        self.restore_content_location(location);
    }

    fn content_browser_toolbar(&mut self, ui: &mut egui::Ui, compact: bool) {
        let breadcrumb = self.content_browser_breadcrumb();
        let settings_before = self.content_browser.settings.clone();
        let facets_before = (
            self.content_browser.source_filter,
            self.content_browser.type_filter,
            self.content_browser.capability_filter,
            self.content_browser.raw_kind_filter,
        );
        let mut next_node = None;
        let toolbar_height = ui.spacing().interact_size.y;
        // `with_layout` inherits the full remaining browser height. A vertical
        // separator inside that horizontal layout then expands to that height,
        // making the toolbar consume the entire dock and clipping the results.
        // `horizontal_wrapped` starts with one interaction row and grows only
        // when controls actually wrap.
        content_browser_toolbar_row(ui, |ui| {
            ui.label(egui::RichText::new("Content Browser").strong());
            ui.separator();
            if ui
                .add_enabled(
                    !self.content_browser.location_back.is_empty(),
                    egui::Button::new("Back"),
                )
                .on_hover_text("Previous Content Browser location (Mouse Back)")
                .clicked()
            {
                self.navigate_content_back();
            }
            if ui
                .add_enabled(
                    !self.content_browser.location_forward.is_empty(),
                    egui::Button::new("Forward"),
                )
                .on_hover_text("Next Content Browser location (Mouse Forward)")
                .clicked()
            {
                self.navigate_content_forward();
            }
            if self.content_browser.node == ContentNode::GameFiles
                && ui
                    .add_enabled(
                        !self.content_browser.game_file_path.is_empty(),
                        egui::Button::new("Up"),
                    )
                    .on_hover_text("Open the parent folder")
                    .clicked()
            {
                let mut parent = self.content_browser.game_file_path.clone();
                parent.pop();
                self.navigate_game_files_to(parent);
            }
            if compact {
                egui::ComboBox::from_id_salt("content-browser-node")
                    .selected_text(&breadcrumb)
                    .show_ui(ui, |ui| {
                        for node in all_content_nodes() {
                            if ui
                                .selectable_label(
                                    self.content_browser.node == node,
                                    node.breadcrumb(),
                                )
                                .clicked()
                            {
                                next_node = Some(node);
                            }
                        }
                    });
                if self.content_browser.node == ContentNode::ProjectModels {
                    egui::ComboBox::from_id_salt("content-browser-model-folder")
                        .selected_text(self.model_folder_filter.as_deref().unwrap_or("All Models"))
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.model_folder_filter.is_none(), "All Models")
                                .clicked()
                            {
                                let mut location = self.current_content_location();
                                location.model_folder = None;
                                self.navigate_content_to(location);
                            }
                            for folder in self.model_asset_folders() {
                                let selected = self.model_folder_filter.as_deref() == Some(&folder);
                                if ui.selectable_label(selected, &folder).clicked() {
                                    let mut location = self.current_content_location();
                                    location.model_folder = Some(folder);
                                    self.navigate_content_to(location);
                                }
                            }
                        });
                }
            } else {
                ui.add_sized(
                    [
                        breadcrumb.chars().count() as f32 * 7.0 + 8.0,
                        toolbar_height,
                    ],
                    egui::Label::new(
                        egui::RichText::new(&breadcrumb)
                            .color(egui::Color32::from_rgb(180, 189, 191)),
                    )
                    .truncate(),
                );
            }
            ui.add_space(4.0);
            ui.add_sized(
                [if compact { 180.0 } else { 280.0 }, toolbar_height],
                egui::TextEdit::singleline(&mut self.content_browser.query)
                    .hint_text("Search names, classes, stages, or paths"),
            );

            let active_facets = usize::from(self.content_browser.source_filter.is_some())
                + usize::from(self.content_browser.type_filter.is_some())
                + usize::from(self.content_browser.capability_filter.is_some());
            ui.menu_button(
                if active_facets == 0 {
                    "Filters".to_string()
                } else {
                    format!("Filters ({active_facets})")
                },
                |ui| {
                    ui.label(egui::RichText::new("SOURCE").small().strong());
                    ui.selectable_value(
                        &mut self.content_browser.source_filter,
                        None,
                        "Any source",
                    );
                    ui.selectable_value(
                        &mut self.content_browser.source_filter,
                        Some(ContentSource::Project),
                        "Project Content",
                    );
                    ui.selectable_value(
                        &mut self.content_browser.source_filter,
                        Some(ContentSource::Game),
                        "Game Content",
                    );
                    ui.separator();
                    ui.label(egui::RichText::new("TYPE").small().strong());
                    ui.selectable_value(&mut self.content_browser.type_filter, None, "Any type");
                    for kind in [
                        ContentKind::Stage,
                        ContentKind::Object,
                        ContentKind::Model,
                        ContentKind::Skybox,
                        ContentKind::Music,
                        ContentKind::Sound,
                        ContentKind::GameFile,
                    ] {
                        ui.selectable_value(
                            &mut self.content_browser.type_filter,
                            Some(kind),
                            kind.label(),
                        );
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("CAPABILITY").small().strong());
                    ui.selectable_value(
                        &mut self.content_browser.capability_filter,
                        None,
                        "Any capability",
                    );
                    for capability in [
                        ContentCapability::Open,
                        ContentCapability::Place,
                        ContentCapability::Apply,
                        ContentCapability::Assign,
                        ContentCapability::Preview,
                        ContentCapability::Inspect,
                        ContentCapability::Editable,
                    ] {
                        ui.selectable_value(
                            &mut self.content_browser.capability_filter,
                            Some(capability),
                            capability.label(),
                        );
                    }
                    if active_facets > 0 && ui.button("Clear filters").clicked() {
                        self.content_browser.source_filter = None;
                        self.content_browser.type_filter = None;
                        self.content_browser.capability_filter = None;
                    }
                },
            );

            if self.content_browser.node == ContentNode::GameFiles {
                egui::ComboBox::from_id_salt("content-browser-raw-kind")
                    .selected_text(
                        self.content_browser
                            .raw_kind_filter
                            .map_or("All file types", game_resource_kind_label),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.content_browser.raw_kind_filter,
                            None,
                            "All file types",
                        );
                        for kind in all_game_resource_kinds() {
                            ui.selectable_value(
                                &mut self.content_browser.raw_kind_filter,
                                Some(kind),
                                game_resource_kind_label(kind),
                            );
                        }
                    });
            }

            let key = self.content_browser_collection_key();
            let preference = self
                .content_browser
                .settings
                .collections
                .entry(key)
                .or_insert_with(|| BrowserCollectionPreference {
                    view: if self.content_browser.node == ContentNode::GameFiles {
                        BrowserViewPreference::List
                    } else {
                        BrowserViewPreference::Grid
                    },
                    sort: BrowserSortPreference::Name,
                });
            egui::ComboBox::from_id_salt("content-browser-sort")
                .selected_text(match preference.sort {
                    BrowserSortPreference::Name => "Name",
                    BrowserSortPreference::Type => "Type",
                    BrowserSortPreference::Source => "Source",
                    BrowserSortPreference::Recent => "Recent",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut preference.sort, BrowserSortPreference::Name, "Name");
                    ui.selectable_value(&mut preference.sort, BrowserSortPreference::Type, "Type");
                    ui.selectable_value(
                        &mut preference.sort,
                        BrowserSortPreference::Source,
                        "Source",
                    );
                    ui.selectable_value(
                        &mut preference.sort,
                        BrowserSortPreference::Recent,
                        "Recent",
                    );
                });
            ui.selectable_value(&mut preference.view, BrowserViewPreference::Grid, "Grid");
            ui.selectable_value(&mut preference.view, BrowserViewPreference::List, "List");
            if (self.audio_preview_is_active() || self.audio_preview_is_loading())
                && ui
                    .button("Stop Audio")
                    .on_hover_text("Stop or cancel the current Content Browser audio preview")
                    .clicked()
            {
                self.stop_audio_preview();
            }

            match self.content_browser.node {
                ContentNode::ProjectStages
                    if ui
                        .add_enabled(
                            self.current_project.is_some() && self.background_receiver.is_none(),
                            egui::Button::new("+ New Stage"),
                        )
                        .clicked() =>
                {
                    self.request_new_stage();
                }
                ContentNode::ProjectStages => {}
                ContentNode::ProjectModels => {
                    if ui
                        .add_enabled(
                            self.model_import_job.is_none() && self.background_receiver.is_none(),
                            egui::Button::new("Import Model"),
                        )
                        .clicked()
                    {
                        self.begin_model_import();
                    }
                    if ui.button("New Folder").clicked() {
                        self.content_browser.show_new_folder = true;
                    }
                    if ui.button("Import Settings").clicked() {
                        self.content_browser.show_import_settings =
                            !self.content_browser.show_import_settings;
                    }
                }
                ContentNode::GameFiles if ui.small_button("Refresh").clicked() => {
                    self.refresh_game_content_index();
                }
                ContentNode::GameFiles => {}
                _ => {}
            }
        });
        let facets_after = (
            self.content_browser.source_filter,
            self.content_browser.type_filter,
            self.content_browser.capability_filter,
            self.content_browser.raw_kind_filter,
        );
        if facets_before != facets_after {
            self.content_browser.raw_filter_cache = None;
            self.content_browser.raw_directory_cache = None;
            if facets_before.3 != facets_after.3 {
                self.content_browser.settings.last_collection =
                    self.content_browser_collection_key();
            }
        }
        if settings_before != self.content_browser.settings {
            self.persist_content_browser_settings();
        }
        if let Some(node) = next_node {
            self.select_content_node(node);
        }
    }

    fn content_browser_breadcrumb(&self) -> String {
        if self.content_browser.node != ContentNode::GameFiles
            || self.content_browser.game_file_path.is_empty()
        {
            return self.content_browser.node.breadcrumb().to_string();
        }
        format!(
            "{} / {}",
            self.content_browser.node.breadcrumb(),
            self.content_browser
                .game_file_path
                .iter()
                .map(|part| part.trim_end_matches('!'))
                .collect::<Vec<_>>()
                .join(" / ")
        )
    }

    fn content_browser_source_tree(&mut self, ui: &mut egui::Ui) {
        let width = self.content_browser.settings.tree_width.clamp(150.0, 360.0);
        let mut settings_changed = false;
        ui.allocate_ui_with_layout(
            egui::vec2(width, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("content-source-tree")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut next = None;
                        for node in [
                            ContentNode::Favorites,
                            ContentNode::Recent,
                            ContentNode::All,
                        ] {
                            if source_tree_node(ui, self.content_browser.node, node, 0.0) {
                                next = Some(node);
                            }
                        }

                        ui.add_space(8.0);
                        let project_expanded = self
                            .content_browser
                            .settings
                            .expanded_roots
                            .contains("project");
                        if source_tree_root_heading(
                            ui,
                            "PROJECT CONTENT",
                            project_expanded,
                            egui::Color32::from_rgb(48, 176, 190),
                        ) {
                            if !self
                                .content_browser
                                .settings
                                .expanded_roots
                                .remove("project")
                            {
                                self.content_browser
                                    .settings
                                    .expanded_roots
                                    .insert("project".to_string());
                            }
                            settings_changed = true;
                        }
                        if self
                            .content_browser
                            .settings
                            .expanded_roots
                            .contains("project")
                        {
                            for node in [ContentNode::ProjectStages, ContentNode::ProjectModels] {
                                if source_tree_node(ui, self.content_browser.node, node, 8.0) {
                                    next = Some(node);
                                }
                            }
                            if self.content_browser.node == ContentNode::ProjectModels {
                                if ui
                                    .selectable_label(
                                        self.model_folder_filter.is_none(),
                                        "      All Models",
                                    )
                                    .clicked()
                                {
                                    let mut location = self.current_content_location();
                                    location.model_folder = None;
                                    self.navigate_content_to(location);
                                }
                                for folder in self.model_asset_folders() {
                                    let selected =
                                        self.model_folder_filter.as_deref() == Some(&folder);
                                    if ui
                                        .selectable_label(selected, format!("      {folder}"))
                                        .clicked()
                                    {
                                        let mut location = self.current_content_location();
                                        location.model_folder = Some(folder);
                                        self.navigate_content_to(location);
                                    }
                                }
                            }
                        }

                        ui.add_space(8.0);
                        let game_expanded = self
                            .content_browser
                            .settings
                            .expanded_roots
                            .contains("game");
                        if source_tree_root_heading(
                            ui,
                            "GAME CONTENT",
                            game_expanded,
                            egui::Color32::from_rgb(232, 186, 72),
                        ) {
                            if !self.content_browser.settings.expanded_roots.remove("game") {
                                self.content_browser
                                    .settings
                                    .expanded_roots
                                    .insert("game".to_string());
                            }
                            settings_changed = true;
                        }
                        if self
                            .content_browser
                            .settings
                            .expanded_roots
                            .contains("game")
                        {
                            for node in [
                                ContentNode::GameStages,
                                ContentNode::GameObjects,
                                ContentNode::GameSkyboxes,
                                ContentNode::GameMusic,
                                ContentNode::GameSounds,
                                ContentNode::GameFiles,
                            ] {
                                if source_tree_node(ui, self.content_browser.node, node, 8.0) {
                                    next = Some(node);
                                }
                            }
                            if self.content_browser.node == ContentNode::GameFiles {
                                for kind in all_game_resource_kinds() {
                                    let count = self
                                        .game_content_index
                                        .kind_counts
                                        .get(&kind)
                                        .copied()
                                        .unwrap_or_default();
                                    if ui
                                        .selectable_label(
                                            self.content_browser.raw_kind_filter == Some(kind),
                                            format!(
                                                "      {}  {count}",
                                                game_resource_kind_label(kind)
                                            ),
                                        )
                                        .clicked()
                                    {
                                        let mut location = self.current_content_location();
                                        location.raw_kind_filter = Some(kind);
                                        location.game_file_path.clear();
                                        self.navigate_content_to(location);
                                        settings_changed = true;
                                    }
                                }
                            }
                        }
                        if let Some(node) = next {
                            self.select_content_node(node);
                        }
                    });
            },
        );
        if settings_changed {
            self.persist_content_browser_settings();
        }
    }
    fn content_browser_landing(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("content-browser-landing")
            .auto_shrink([false, false])
            .show(ui, |ui| self.content_browser_landing_body(ui));
    }

    fn content_browser_landing_body(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(ui.available_width());
        ui.heading("All Content");
        ui.label("Project authoring tools first, with every original-game resource still discoverable under Game Files.");
        ui.add_space(8.0);

        let counts = self.content_category_counts();
        let available_width = ui.available_width().max(180.0);
        let layout = content_browser_layout(available_width, counts.len());
        egui::Grid::new("content-browser-landing-grid")
            .num_columns(layout.columns)
            .min_col_width(layout.card_width)
            .max_col_width(layout.card_width)
            .spacing(egui::vec2(8.0, 8.0))
            .show(ui, |ui| {
                for (index, (node, count, subtitle)) in counts.into_iter().enumerate() {
                    let item = ContentItemSummary {
                        id: ContentItemId::GameFile(format!("collection:{}", node.key())),
                        kind: match node {
                            ContentNode::ProjectStages | ContentNode::GameStages => {
                                ContentKind::Stage
                            }
                            ContentNode::ProjectModels => ContentKind::Model,
                            ContentNode::GameObjects => ContentKind::Object,
                            ContentNode::GameSkyboxes => ContentKind::Skybox,
                            ContentNode::GameMusic => ContentKind::Music,
                            ContentNode::GameSounds => ContentKind::Sound,
                            _ => ContentKind::GameFile,
                        },
                        source: if matches!(
                            node,
                            ContentNode::ProjectStages | ContentNode::ProjectModels
                        ) {
                            ContentSource::Project
                        } else {
                            ContentSource::Game
                        },
                        title: node.breadcrumb().to_string(),
                        subtitle: subtitle.to_string(),
                        detail: format!("{count} item{}", if count == 1 { "" } else { "s" }),
                        search_text: String::new(),
                        capabilities: ContentCapabilities::default(),
                        selected_in_scene: false,
                    };
                    let response = content_grid_card(
                        ui,
                        egui::vec2(layout.card_width, 104.0),
                        false,
                        false,
                        &item,
                    );
                    if response.clicked() {
                        self.select_content_node(node);
                    }
                    if (index + 1) % layout.columns == 0 {
                        ui.end_row();
                    }
                }
            });

        ui.add_space(10.0);
        self.content_browser_landing_saved_items(ui, ContentNode::Favorites);
        self.content_browser_landing_saved_items(ui, ContentNode::Recent);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(format!(
                    "Favorites  {}",
                    self.content_browser.settings.favorites.len()
                ))
                .clicked()
            {
                self.select_content_node(ContentNode::Favorites);
            }
            if ui
                .button(format!(
                    "Recent  {}",
                    self.content_browser.settings.recent.len()
                ))
                .clicked()
            {
                self.select_content_node(ContentNode::Recent);
            }
            ui.separator();
            match self.game_content_index.phase {
                GameContentIndexPhase::Loading => {
                    ui.spinner();
                    ui.small(format!(
                        "Indexing original content: {} found",
                        self.game_content_index.entries.len()
                    ));
                }
                GameContentIndexPhase::Ready { from_cache } => {
                    ui.colored_label(egui::Color32::from_rgb(89, 203, 140), "Game Files ready");
                    if from_cache {
                        ui.small("metadata cache");
                    }
                }
                GameContentIndexPhase::Failed => {
                    ui.colored_label(
                        egui::Color32::from_rgb(241, 126, 104),
                        "Game Files unavailable",
                    );
                }
                GameContentIndexPhase::Idle => {
                    ui.small("Choose a project to index Game Files.");
                }
            }
        });
    }

    fn content_browser_landing_saved_items(&mut self, ui: &mut egui::Ui, node: ContentNode) {
        let labels = match node {
            ContentNode::Recent => self
                .content_browser
                .settings
                .recent
                .iter()
                .take(5)
                .map(|entry| entry.label.clone())
                .collect::<Vec<_>>(),
            ContentNode::Favorites => {
                let favorites = &self.content_browser.settings.favorites;
                if favorites.is_empty() {
                    return;
                }
                favorites
                    .iter()
                    .take(5)
                    .map(|stable_key| self.content_browser_favorite_label(stable_key))
                    .collect()
            }
            _ => Vec::new(),
        };
        if labels.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(node.label()).small().strong());
            for label in labels {
                if ui.small_button(label).clicked() {
                    self.select_content_node(node);
                }
            }
        });
    }

    fn content_browser_favorite_label(&self, stable_key: &str) -> String {
        if let Some(recent) = self
            .content_browser
            .settings
            .recent
            .iter()
            .find(|recent| recent.id == stable_key)
        {
            return recent.label.clone();
        }
        if let Some(entry) = stable_key
            .strip_prefix("game:file:")
            .and_then(|raw_id| self.game_content_index.by_stable_id.get(raw_id))
            .and_then(|index| self.game_content_index.entries.get(*index))
        {
            return entry
                .display_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&entry.display_path)
                .to_string();
        }
        if let Some(object) = stable_key.strip_prefix("game:object:").and_then(|factory| {
            self.registry.as_ref().and_then(|registry| {
                registry
                    .objects
                    .iter()
                    .find(|object| object.factory_name.eq_ignore_ascii_case(factory))
            })
        }) {
            return object_palette_display_name(object);
        }
        if let Some(entry) = stable_key.strip_prefix("project:model:").and_then(|id| {
            self.model_catalog_entries
                .iter()
                .find(|entry| entry.id.to_string() == id)
        }) {
            return entry.name.clone();
        }
        if let Some(entry) = stable_key
            .strip_prefix("game:music:")
            .and_then(|value| value.split(':').next())
            .and_then(|id| u32::from_str_radix(id, 16).ok())
            .and_then(|id| self.retail_music.iter().find(|entry| entry.bgm_id == id))
        {
            return entry.label.clone();
        }
        if let Some(entry) = stable_key
            .strip_prefix("game:sound:")
            .and_then(|id| u32::from_str_radix(id, 16).ok())
            .and_then(|id| self.retail_sounds.iter().find(|entry| entry.sound_id == id))
        {
            return entry.label.clone();
        }
        if let Some(value) = stable_key.strip_prefix("game:skybox:") {
            let stage_id = value.split(':').next().unwrap_or(value);
            return self
                .scene_labels
                .get(stage_id)
                .and_then(|label| label.stage_name.clone())
                .unwrap_or_else(|| stage_id.to_string());
        }
        let stage_id = stable_key
            .rsplit(":stage:")
            .next()
            .filter(|stage_id| *stage_id != stable_key)
            .or_else(|| stable_key.strip_prefix("game:stage:"));
        if let Some(stage_id) = stage_id {
            return self
                .scene_labels
                .get(stage_id)
                .and_then(|label| label.stage_name.clone())
                .unwrap_or_else(|| stage_id.to_string());
        }
        stable_key
            .rsplit(':')
            .next()
            .unwrap_or(stable_key)
            .to_string()
    }

    fn content_browser_results(&mut self, ui: &mut egui::Ui, items: &[ContentItemSummary]) {
        ui.set_min_width(ui.available_width());
        if self.content_browser.node == ContentNode::ProjectModels {
            self.refresh_model_catalog();
            if self.content_browser.show_new_folder {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_model_folder_draft)
                            .hint_text("Folder name")
                            .desired_width(220.0),
                    );
                    if ui
                        .add_enabled(
                            !self.new_model_folder_draft.trim().is_empty(),
                            egui::Button::new("Create"),
                        )
                        .clicked()
                    {
                        self.create_model_folder();
                        self.content_browser.show_new_folder = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.content_browser.show_new_folder = false;
                    }
                });
            }
            if self.content_browser.show_import_settings {
                egui::CollapsingHeader::new("Model Import Settings")
                    .default_open(true)
                    .show(ui, |ui| self.model_import_options_panel(ui));
            }
        }

        ui.horizontal(|ui| {
            ui.small(format!(
                "{} matching item{}",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            ));
            if let Some(selected) = self.content_browser.selected.clone() {
                if let Some(item) = items.iter().find(|item| item.id == selected) {
                    ui.separator();
                    ui.label(egui::RichText::new(&item.title).strong());
                    if let Some(command) = compact_primary_action(ui, item, self) {
                        self.run_content_browser_command(command);
                    }
                }
            }
        });
        ui.separator();

        if items.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No matching content").strong());
                ui.small(match self.content_browser.node {
                    ContentNode::Favorites => "Favorite items from any source appear here.",
                    ContentNode::Recent => {
                        "Items appear after you open, place, apply, or preview them."
                    }
                    ContentNode::GameFiles => {
                        "The read-only game index is still loading or contains no matching files."
                    }
                    _ => "Try clearing the search or choosing another source.",
                });
            });
            return;
        }

        let view = self.content_browser_view();
        match view {
            BrowserViewPreference::Grid => self.content_browser_grid(ui, items),
            BrowserViewPreference::List => self.content_browser_list(ui, items),
        }
    }
}

fn content_browser_toolbar_row<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.horizontal_wrapped(add_contents)
}

impl SmsEditorApp {
    fn content_browser_grid(&mut self, ui: &mut egui::Ui, items: &[ContentItemSummary]) {
        let layout = content_browser_layout(ui.available_width(), items.len());
        let rows = items.len().div_ceil(layout.columns);
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt(("unified-content-grid", self.content_browser.node.key()))
            .auto_shrink([false, false])
            .show_rows(ui, GRID_ROW_HEIGHT, rows, |ui, visible_rows| {
                for row in visible_rows {
                    ui.horizontal(|ui| {
                        let start = row * layout.columns;
                        let end = (start + layout.columns).min(items.len());
                        for item in &items[start..end] {
                            let selected = self.content_browser.selected.as_ref() == Some(&item.id);
                            let favorite = self
                                .content_browser
                                .settings
                                .favorites
                                .contains(&item.stable_key());
                            let thumbnail = self.content_item_thumbnail(item, false);
                            let response = content_grid_card(
                                ui,
                                egui::vec2(layout.card_width, GRID_ROW_HEIGHT - 8.0),
                                selected,
                                favorite,
                                item,
                            );
                            if let Some(texture) = thumbnail {
                                paint_content_thumbnail(ui, response.rect, &texture, true);
                            }
                            if item.capabilities.place {
                                match &item.id {
                                    ContentItemId::Object(factory_name) => {
                                        response.dnd_set_drag_payload(ObjectPaletteDragPayload {
                                            factory_name: factory_name.clone(),
                                        });
                                    }
                                    ContentItemId::Model(asset_id) => {
                                        response.dnd_set_drag_payload(ModelAssetDragPayload {
                                            asset_id: *asset_id,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            let action = content_item_action(&response, item, favorite);
                            if action.select || action.toggle_favorite || action.command.is_some() {
                                actions.push((item.clone(), action));
                            }
                        }
                    });
                }
            });
        self.apply_content_actions(actions);
    }

    fn content_browser_list(&mut self, ui: &mut egui::Ui, items: &[ContentItemSummary]) {
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt(("unified-content-list", self.content_browser.node.key()))
            .auto_shrink([false, false])
            .show_rows(ui, LIST_ROW_HEIGHT, items.len(), |ui, range| {
                for item in &items[range] {
                    let selected = self.content_browser.selected.as_ref() == Some(&item.id);
                    let favorite = self
                        .content_browser
                        .settings
                        .favorites
                        .contains(&item.stable_key());
                    let thumbnail = self.content_item_thumbnail(item, false);
                    let response = content_list_row(ui, selected, favorite, item);
                    if let Some(texture) = thumbnail {
                        paint_content_thumbnail(ui, response.rect, &texture, false);
                    }
                    if item.capabilities.place {
                        match &item.id {
                            ContentItemId::Object(factory_name) => {
                                response.dnd_set_drag_payload(ObjectPaletteDragPayload {
                                    factory_name: factory_name.clone(),
                                });
                            }
                            ContentItemId::Model(asset_id) => {
                                response.dnd_set_drag_payload(ModelAssetDragPayload {
                                    asset_id: *asset_id,
                                });
                            }
                            _ => {}
                        }
                    }
                    let action = content_item_action(&response, item, favorite);
                    if action.select || action.toggle_favorite || action.command.is_some() {
                        actions.push((item.clone(), action));
                    }
                }
            });
        self.apply_content_actions(actions);
    }

    fn apply_content_actions(&mut self, actions: Vec<(ContentItemSummary, ContentItemAction)>) {
        for (item, action) in actions {
            if action.select {
                self.content_browser.selected = Some(item.id.clone());
                self.content_browser.inspector_active = true;
            }
            if action.toggle_favorite {
                self.content_browser
                    .settings
                    .toggle_favorite(&item.stable_key());
                self.persist_content_browser_settings();
            }
            if let Some(command) = action.command {
                self.run_content_browser_command(command);
                self.remember_recent_item(&item);
            }
        }
    }

    fn filtered_content_items(&self) -> Vec<ContentItemSummary> {
        let mut items = self.all_content_items();
        let query = self.content_browser.query.trim().to_ascii_lowercase();
        items.retain(|item| {
            self.content_item_in_current_node(item)
                && content_item_matches_facets(
                    item,
                    self.content_browser.source_filter,
                    self.content_browser.type_filter,
                    self.content_browser.capability_filter,
                )
                && query_matches(&item.search_text, &query)
        });

        let recent = self
            .content_browser
            .settings
            .recent
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        match self.content_browser_sort() {
            BrowserSortPreference::Name => items.sort_by(compare_content_name),
            BrowserSortPreference::Type => items.sort_by(|left, right| {
                left.kind
                    .cmp(&right.kind)
                    .then_with(|| compare_content_name(left, right))
            }),
            BrowserSortPreference::Source => items.sort_by(|left, right| {
                left.source
                    .cmp(&right.source)
                    .then_with(|| compare_content_name(left, right))
            }),
            BrowserSortPreference::Recent => items.sort_by(|left, right| {
                recent
                    .get(left.stable_key().as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
                    .cmp(
                        &recent
                            .get(right.stable_key().as_str())
                            .copied()
                            .unwrap_or(usize::MAX),
                    )
                    .then_with(|| compare_content_name(left, right))
            }),
        }
        items
    }

    fn all_content_items(&self) -> Vec<ContentItemSummary> {
        let mut items = Vec::new();
        let project_scope = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.project_id.clone());

        for archive in &self.scene_archives {
            let project = archive.size_bytes == 0;
            let localized = self
                .scene_labels
                .get(&archive.stage_id.to_ascii_lowercase());
            let stage_name = localized.and_then(|label| label.stage_name.as_deref());
            let scenarios = localized
                .map(|label| label.scenario_names.join(" / "))
                .unwrap_or_default();
            items.push(ContentItemSummary {
                id: ContentItemId::Stage {
                    stage_id: archive.stage_id.clone(),
                    project_scope: project.then(|| {
                        project_scope
                            .clone()
                            .unwrap_or_else(|| "unsaved-project".to_string())
                    }),
                },
                kind: ContentKind::Stage,
                source: if project {
                    ContentSource::Project
                } else {
                    ContentSource::Game
                },
                title: stage_name.unwrap_or(&archive.stage_id).to_string(),
                subtitle: if stage_name.is_some() {
                    archive.stage_id.clone()
                } else {
                    format!("{} stage", archive.group)
                },
                detail: if scenarios.is_empty() {
                    if project {
                        "Authored stage".to_string()
                    } else {
                        format_bytes_short(archive.size_bytes)
                    }
                } else {
                    scenarios.clone()
                },
                search_text: format!(
                    "{} {} {} {} {}",
                    archive.stage_id,
                    archive.group,
                    archive.relative_path.display(),
                    stage_name.unwrap_or_default(),
                    scenarios
                )
                .to_ascii_lowercase(),
                capabilities: ContentCapabilities {
                    open: true,
                    inspect: true,
                    editable: project,
                    ..ContentCapabilities::default()
                },
                selected_in_scene: self.stage_id.eq_ignore_ascii_case(&archive.stage_id),
            });
        }

        for entry in &self.model_catalog_entries {
            if self.content_browser.node == ContentNode::ProjectModels
                && self
                    .model_folder_filter
                    .as_ref()
                    .is_some_and(|folder| entry.relative_path.parent() != Some(Path::new(folder)))
            {
                continue;
            }
            items.push(ContentItemSummary {
                id: ContentItemId::Model(entry.id),
                kind: ContentKind::Model,
                source: ContentSource::Project,
                title: entry.name.clone(),
                subtitle: entry.relative_path.to_string_lossy().replace('\\', "/"),
                detail: format!(
                    "{} mesh{} / {} material{}{}",
                    entry.mesh_count,
                    if entry.mesh_count == 1 { "" } else { "es" },
                    entry.material_count,
                    if entry.material_count == 1 { "" } else { "s" },
                    if entry.has_collision {
                        " / collision"
                    } else {
                        ""
                    }
                ),
                search_text: format!(
                    "{} {} {}",
                    entry.name,
                    entry.relative_path.display(),
                    entry.id
                )
                .to_ascii_lowercase(),
                capabilities: ContentCapabilities {
                    place: self.document.is_some(),
                    preview: true,
                    inspect: true,
                    editable: true,
                    ..ContentCapabilities::default()
                },
                selected_in_scene: self.selected_model_asset == Some(entry.id)
                    || self
                        .active_placement
                        .as_ref()
                        .and_then(ActivePlacement::model_asset)
                        == Some(entry.id),
            });
        }

        if let Some(registry) = &self.registry {
            for object in registry
                .objects
                .iter()
                .filter(|object| !object.hidden)
                .filter(|object| self.can_spawn_factory(&object.factory_name))
            {
                let placeable = true;
                let title = object_palette_display_name(object);
                let retail_template = self.object_authoring_catalog.find(&object.factory_name);
                items.push(ContentItemSummary {
                    id: ContentItemId::Object(object.factory_name.clone()),
                    kind: ContentKind::Object,
                    source: ContentSource::Game,
                    title,
                    subtitle: object.factory_name.clone(),
                    detail: format!("{} / {}", object.category, object.class_name),
                    search_text: format!(
                        "{} {} {} {}",
                        object.factory_name,
                        object.class_name,
                        object.category,
                        object.display_name.as_deref().unwrap_or_default()
                    )
                    .to_ascii_lowercase(),
                    capabilities: ContentCapabilities {
                        place: placeable,
                        preview: retail_template
                            .and_then(|template| template.preview_resource_path.as_ref())
                            .is_some(),
                        inspect: true,
                        ..ContentCapabilities::default()
                    },
                    selected_in_scene: self
                        .active_placement
                        .as_ref()
                        .and_then(ActivePlacement::object_factory)
                        == Some(&object.factory_name),
                });
            }
        }

        for entry in &self.retail_skyboxes {
            let stage_name = self
                .scene_labels
                .get(&entry.stage_id.to_ascii_lowercase())
                .and_then(|label| label.stage_name.as_deref());
            items.push(ContentItemSummary {
                id: ContentItemId::Skybox {
                    stage_id: entry.stage_id.clone(),
                    archive_path: entry.archive_path.to_string_lossy().into_owned(),
                },
                kind: ContentKind::Skybox,
                source: ContentSource::Game,
                title: stage_name.unwrap_or(&entry.stage_id).to_string(),
                subtitle: format!("{} skybox bundle", entry.stage_id),
                detail: format!("{} linked resources", entry.resource_count),
                search_text: format!(
                    "{} {} {}",
                    entry.stage_id,
                    stage_name.unwrap_or_default(),
                    entry.archive_path.display()
                )
                .to_ascii_lowercase(),
                capabilities: ContentCapabilities {
                    apply: self.document.is_some(),
                    preview: true,
                    inspect: true,
                    ..ContentCapabilities::default()
                },
                selected_in_scene: false,
            });
        }

        for entry in &self.retail_music {
            items.push(ContentItemSummary {
                id: ContentItemId::Music {
                    bgm_id: entry.bgm_id,
                    wave_scene_id: entry.wave_scene_id,
                },
                kind: ContentKind::Music,
                source: ContentSource::Game,
                title: entry.label.clone(),
                subtitle: format!("BGM 0x{:08X}", entry.bgm_id),
                detail: format!("Wave scene 0x{:X}", entry.wave_scene_id),
                search_text: format!(
                    "{} {:08x} {:x}",
                    entry.label, entry.bgm_id, entry.wave_scene_id
                )
                .to_ascii_lowercase(),
                capabilities: ContentCapabilities {
                    assign: self.document.is_some() && self.current_project.is_some(),
                    preview: true,
                    inspect: true,
                    ..ContentCapabilities::default()
                },
                selected_in_scene: self
                    .current_stage_music()
                    .is_some_and(|music| music.bgm_id == entry.bgm_id),
            });
        }

        for entry in &self.retail_sounds {
            items.push(ContentItemSummary {
                id: ContentItemId::Sound(entry.sound_id),
                kind: ContentKind::Sound,
                source: ContentSource::Game,
                title: entry.label.clone(),
                subtitle: entry.symbol.clone(),
                detail: format!("Sound ID 0x{:08X}", entry.sound_id),
                search_text: format!("{} {} {:08x}", entry.label, entry.symbol, entry.sound_id)
                    .to_ascii_lowercase(),
                capabilities: ContentCapabilities {
                    assign: self.can_assign_sound_to_selected_helper(),
                    preview: true,
                    inspect: true,
                    ..ContentCapabilities::default()
                },
                selected_in_scene: false,
            });
        }
        items
    }

    fn content_item_in_current_node(&self, item: &ContentItemSummary) -> bool {
        match self.content_browser.node {
            ContentNode::All => true,
            ContentNode::Favorites => self
                .content_browser
                .settings
                .favorites
                .contains(&item.stable_key()),
            ContentNode::Recent => self
                .content_browser
                .settings
                .recent
                .iter()
                .any(|recent| recent.id == item.stable_key()),
            ContentNode::ProjectStages => {
                item.kind == ContentKind::Stage && item.source == ContentSource::Project
            }
            ContentNode::ProjectModels => item.kind == ContentKind::Model,
            ContentNode::GameStages => {
                item.kind == ContentKind::Stage && item.source == ContentSource::Game
            }
            ContentNode::GameObjects => item.kind == ContentKind::Object,
            ContentNode::GameSkyboxes => item.kind == ContentKind::Skybox,
            ContentNode::GameMusic => item.kind == ContentKind::Music,
            ContentNode::GameSounds => item.kind == ContentKind::Sound,
            ContentNode::GameFiles => false,
        }
    }

    fn content_category_counts(&self) -> Vec<(ContentNode, usize, &'static str)> {
        let project_stages = self
            .scene_archives
            .iter()
            .filter(|archive| archive.size_bytes == 0)
            .count();
        vec![
            (
                ContentNode::ProjectStages,
                project_stages,
                "Authored stages and scenarios",
            ),
            (
                ContentNode::ProjectModels,
                self.model_catalog_entries.len(),
                "Editable imported models",
            ),
            (
                ContentNode::GameStages,
                self.scene_archives.len().saturating_sub(project_stages),
                "Original stage archives",
            ),
            (
                ContentNode::GameObjects,
                self.object_authoring_catalog.len()
                    + usize::from(
                        self.object_authoring_catalog.find("Mario").is_none()
                            && self.can_spawn_factory("Mario"),
                    )
                    + usize::from(
                        self.object_authoring_catalog.find("Sky").is_none()
                            && self.can_spawn_factory("Sky"),
                    ),
                "Safely placeable templates",
            ),
            (
                ContentNode::GameSkyboxes,
                self.retail_skyboxes.len(),
                "Complete retail bundles",
            ),
            (
                ContentNode::GameMusic,
                self.retail_music.len(),
                "Stage music choices",
            ),
            (
                ContentNode::GameSounds,
                self.retail_sounds.len(),
                "Exact sound identifiers",
            ),
            (
                ContentNode::GameFiles,
                self.game_content_index.entries.len(),
                "Complete read-only inventory",
            ),
        ]
    }

    fn select_content_node(&mut self, node: ContentNode) {
        let mut location = self.current_content_location();
        location.node = node;
        location.model_folder = None;
        if node == ContentNode::GameFiles {
            location.raw_kind_filter = None;
        } else {
            location.game_file_path.clear();
        }
        self.navigate_content_to(location);
    }

    fn content_browser_view(&self) -> BrowserViewPreference {
        self.content_browser
            .settings
            .collections
            .get(&self.content_browser_collection_key())
            .map_or_else(
                || {
                    if self.content_browser.node == ContentNode::GameFiles {
                        BrowserViewPreference::List
                    } else {
                        BrowserViewPreference::Grid
                    }
                },
                |preference| preference.view,
            )
    }

    fn content_browser_sort(&self) -> BrowserSortPreference {
        if self.content_browser.node == ContentNode::Recent {
            return BrowserSortPreference::Recent;
        }
        self.content_browser
            .settings
            .collections
            .get(&self.content_browser_collection_key())
            .map_or(BrowserSortPreference::Name, |preference| preference.sort)
    }

    pub(super) fn persist_content_browser_settings(&mut self) {
        if let Err(error) = self.content_browser.settings.save_default() {
            self.log
                .push(format!("Could not save Content Browser settings: {error}"));
        }
    }

    fn remember_recent_item(&mut self, item: &ContentItemSummary) {
        self.content_browser
            .settings
            .touch_recent(&item.stable_key(), &item.title);
        self.persist_content_browser_settings();
    }

    fn raw_filter_key(&self) -> RawFilterKey {
        RawFilterKey {
            revision: self.game_content_index.revision,
            node: self.content_browser.node,
            query: self.content_browser.query.trim().to_ascii_lowercase(),
            kind: self.content_browser.raw_kind_filter,
            source: self.content_browser.source_filter,
            content_kind: self.content_browser.type_filter,
            capability: self.content_browser.capability_filter,
            sort: self.content_browser_sort(),
        }
    }

    fn filtered_raw_indices(&mut self) -> Arc<[usize]> {
        let key = self.raw_filter_key();
        if let Some(cache) = &self.content_browser.raw_filter_cache {
            if cache.key == key {
                return Arc::clone(&cache.indices);
            }
        }
        let recent = self
            .content_browser
            .settings
            .recent
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect::<BTreeMap<_, _>>();
        let mut indices = self
            .game_content_index
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| key.kind.is_none_or(|kind| entry.kind == kind))
            .filter(|(_, _)| {
                key.source
                    .is_none_or(|source| source == ContentSource::Game)
            })
            .filter(|(_, _)| {
                key.content_kind
                    .is_none_or(|kind| kind == ContentKind::GameFile)
            })
            .filter(|(_, entry)| {
                key.capability
                    .is_none_or(|capability| raw_content_capabilities(entry).supports(capability))
            })
            .filter(|(_, entry)| {
                query_matches(&entry.display_path.to_ascii_lowercase(), &key.query)
            })
            .filter(|(_, entry)| {
                let stable = format!("game:file:{}", raw_game_file_id(&entry.id));
                match key.node {
                    ContentNode::Favorites => {
                        self.content_browser.settings.favorites.contains(&stable)
                    }
                    ContentNode::Recent => recent.contains_key(&stable),
                    ContentNode::All | ContentNode::GameFiles => true,
                    _ => false,
                }
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by(|left, right| {
            let left_entry = &self.game_content_index.entries[*left];
            let right_entry = &self.game_content_index.entries[*right];
            match key.sort {
                BrowserSortPreference::Name | BrowserSortPreference::Source => left_entry
                    .display_path
                    .to_ascii_lowercase()
                    .cmp(&right_entry.display_path.to_ascii_lowercase()),
                BrowserSortPreference::Type => left_entry
                    .kind
                    .cmp(&right_entry.kind)
                    .then_with(|| left_entry.display_path.cmp(&right_entry.display_path)),
                BrowserSortPreference::Recent => recent
                    .get(&format!("game:file:{}", raw_game_file_id(&left_entry.id)))
                    .copied()
                    .unwrap_or(usize::MAX)
                    .cmp(
                        &recent
                            .get(&format!("game:file:{}", raw_game_file_id(&right_entry.id)))
                            .copied()
                            .unwrap_or(usize::MAX),
                    ),
            }
        });
        let indices = Arc::<[usize]>::from(indices);
        self.content_browser.raw_filter_cache = Some(RawFilterCache {
            key,
            indices: Arc::clone(&indices),
        });
        indices
    }

    fn game_file_directory_entries(&mut self) -> Arc<[GameFileDirectoryEntry]> {
        let folder_mode = self.content_browser.node == ContentNode::GameFiles;
        let key = RawDirectoryKey {
            filter: self.raw_filter_key(),
            path: if folder_mode {
                self.content_browser.game_file_path.clone()
            } else {
                Vec::new()
            },
        };
        if let Some(cache) = &self.content_browser.raw_directory_cache {
            if cache.key == key {
                return Arc::clone(&cache.entries);
            }
        }

        let filtered = self.filtered_raw_indices();
        if !folder_mode {
            let entries = filtered
                .iter()
                .copied()
                .map(GameFileDirectoryEntry::File)
                .collect::<Vec<_>>();
            let entries = Arc::<[GameFileDirectoryEntry]>::from(entries);
            self.content_browser.raw_directory_cache = Some(RawDirectoryCache {
                key,
                entries: Arc::clone(&entries),
            });
            return entries;
        }
        let current = &self.content_browser.game_file_path;
        let mut folders = BTreeSet::new();
        let mut files = Vec::new();
        for &index in filtered.iter() {
            let Some(metadata) = self.game_content_index.entries.get(index) else {
                continue;
            };
            let components = game_file_virtual_components(metadata);
            if !components.starts_with(current) {
                continue;
            }
            let remaining = &components[current.len()..];
            let Some(first) = remaining.first() else {
                continue;
            };
            if remaining.len() == 1 && !first.ends_with('!') {
                files.push(GameFileDirectoryEntry::File(index));
            } else {
                let mut path = current.clone();
                path.push(first.clone());
                folders.insert(path);
            }
        }

        let entries = folders
            .into_iter()
            .map(GameFileDirectoryEntry::Folder)
            .chain(files)
            .collect::<Vec<_>>();
        let entries = Arc::<[GameFileDirectoryEntry]>::from(entries);
        self.content_browser.raw_directory_cache = Some(RawDirectoryCache {
            key,
            entries: Arc::clone(&entries),
        });
        entries
    }

    fn content_browser_raw_results(&mut self, ui: &mut egui::Ui) {
        let entries = self.game_file_directory_entries();
        ui.horizontal(|ui| match self.game_content_index.phase {
            GameContentIndexPhase::Loading => {
                ui.spinner();
                ui.small(format!(
                    "{} original-game resources indexed so far",
                    self.game_content_index.entries.len()
                ));
            }
            GameContentIndexPhase::Ready { from_cache } => {
                ui.small(format!(
                    "{} item{} here / {} indexed{}",
                    entries.len(),
                    if entries.len() == 1 { "" } else { "s" },
                    self.game_content_index.entries.len(),
                    if from_cache { " (metadata cache)" } else { "" }
                ));
            }
            GameContentIndexPhase::Failed => {
                ui.colored_label(
                    egui::Color32::from_rgb(241, 126, 104),
                    "Game Files index incomplete",
                );
                if let Some(error) = &self.game_content_index.error {
                    ui.small(error);
                }
            }
            GameContentIndexPhase::Idle => {
                ui.small("Choose a project to index the extracted game's files/ and sys/ roots.");
            }
        });
        if !self.game_content_index.warnings.is_empty() {
            ui.collapsing(
                format!(
                    "{} indexing warning{}",
                    self.game_content_index.warnings.len(),
                    if self.game_content_index.warnings.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
                |ui| {
                    for warning in self.game_content_index.warnings.iter().take(12) {
                        ui.colored_label(
                            egui::Color32::from_rgb(241, 126, 104),
                            format!("{} — {}", warning.relative_path.display(), warning.message),
                        );
                    }
                    if self.game_content_index.warnings.len() > 12 {
                        ui.small(format!(
                            "{} additional warnings are retained in the index.",
                            self.game_content_index.warnings.len() - 12
                        ));
                    }
                },
            );
        }
        if entries.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label("No matching read-only Game Files.");
                ui.small("Partial results remain usable while the background index is loading.");
            });
            return;
        }
        let has_folders = entries
            .iter()
            .any(|entry| matches!(entry, GameFileDirectoryEntry::Folder(_)));
        let view = if self.content_browser.node == ContentNode::GameFiles && has_folders {
            BrowserViewPreference::Grid
        } else {
            self.content_browser_view()
        };
        match view {
            BrowserViewPreference::Grid => self.content_browser_raw_grid(ui, &entries),
            BrowserViewPreference::List => self.content_browser_raw_list(ui, &entries),
        }
    }

    fn content_browser_raw_list(&mut self, ui: &mut egui::Ui, entries: &[GameFileDirectoryEntry]) {
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("raw-content-list")
            .auto_shrink([false, false])
            .show_rows(ui, LIST_ROW_HEIGHT, entries.len(), |ui, range| {
                for entry in &entries[range] {
                    let (item, thumbnail) = match entry {
                        GameFileDirectoryEntry::Folder(path) => {
                            (game_folder_content_item(path), None)
                        }
                        GameFileDirectoryEntry::File(index) => {
                            let Some(metadata) =
                                self.game_content_index.entries.get(*index).cloned()
                            else {
                                continue;
                            };
                            let thumbnail = self.visible_raw_thumbnail(&metadata);
                            (raw_content_item(&metadata), thumbnail)
                        }
                    };
                    let selected = self.content_browser.selected.as_ref() == Some(&item.id);
                    let favorite = self
                        .content_browser
                        .settings
                        .favorites
                        .contains(&item.stable_key());
                    let response = content_list_row(ui, selected, favorite, &item);
                    if let Some(texture) = thumbnail {
                        paint_content_thumbnail(ui, response.rect, &texture, false);
                    }
                    let action = content_item_action(&response, &item, favorite);
                    if action.select || action.toggle_favorite || action.command.is_some() {
                        actions.push((item, action));
                    }
                }
            });
        self.apply_content_actions(actions);
    }

    fn content_browser_raw_grid(&mut self, ui: &mut egui::Ui, entries: &[GameFileDirectoryEntry]) {
        let layout = content_browser_layout(ui.available_width(), entries.len());
        let rows = entries.len().div_ceil(layout.columns);
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("raw-content-grid")
            .auto_shrink([false, false])
            .show_rows(ui, GRID_ROW_HEIGHT, rows, |ui, visible_rows| {
                for row in visible_rows {
                    ui.horizontal(|ui| {
                        let start = row * layout.columns;
                        let end = (start + layout.columns).min(entries.len());
                        for entry in &entries[start..end] {
                            let (item, thumbnail) = match entry {
                                GameFileDirectoryEntry::Folder(path) => {
                                    (game_folder_content_item(path), None)
                                }
                                GameFileDirectoryEntry::File(index) => {
                                    let Some(metadata) =
                                        self.game_content_index.entries.get(*index).cloned()
                                    else {
                                        continue;
                                    };
                                    let thumbnail = self.visible_raw_thumbnail(&metadata);
                                    (raw_content_item(&metadata), thumbnail)
                                }
                            };
                            let selected = self.content_browser.selected.as_ref() == Some(&item.id);
                            let favorite = self
                                .content_browser
                                .settings
                                .favorites
                                .contains(&item.stable_key());
                            let response = content_grid_card(
                                ui,
                                egui::vec2(layout.card_width, GRID_ROW_HEIGHT - 8.0),
                                selected,
                                favorite,
                                &item,
                            );
                            if let Some(texture) = thumbnail {
                                paint_content_thumbnail(ui, response.rect, &texture, true);
                            }
                            let action = content_item_action(&response, &item, favorite);
                            if action.select || action.toggle_favorite || action.command.is_some() {
                                actions.push((item, action));
                            }
                        }
                    });
                }
            });
        self.apply_content_actions(actions);
    }

    fn run_content_browser_command(&mut self, command: ContentBrowserCommand) {
        match command {
            ContentBrowserCommand::OpenGameFolder(path) => self.navigate_game_files_to(path),
            ContentBrowserCommand::OpenStage(stage_id) => self.request_open_stage(stage_id),
            ContentBrowserCommand::ArmObject(factory_name) => {
                if self.can_spawn_factory(&factory_name) {
                    self.active_placement = Some(ActivePlacement::Object { factory_name });
                    self.tool = EditorTool::Place;
                }
            }
            ContentBrowserCommand::SpawnObject(factory_name) => {
                self.spawn_object_at(factory_name, self.default_spawn_position());
                self.content_browser.inspector_active = false;
            }
            ContentBrowserCommand::ArmModel(asset_id) => self.arm_model_placement(asset_id),
            ContentBrowserCommand::SpawnModel(asset_id) => {
                self.spawn_model_instance_at(asset_id, self.default_spawn_position());
                self.content_browser.inspector_active = false;
            }
            ContentBrowserCommand::EditModel(asset_id) => {
                self.select_model_asset(asset_id);
                self.content_browser.inspector_active = false;
            }
            ContentBrowserCommand::ApplySkybox(entry) => self.request_retail_skybox(entry),
            ContentBrowserCommand::ApplySkyboxPath(archive_path) => {
                let entry = self
                    .retail_skyboxes
                    .iter()
                    .find(|entry| {
                        entry.archive_path.to_string_lossy().as_ref() == archive_path.as_str()
                    })
                    .cloned();
                if let Some(entry) = entry {
                    self.request_retail_skybox(entry);
                }
            }
            ContentBrowserCommand::PreviewMusic(bgm_id) => self.preview_bgm_now(bgm_id),
            ContentBrowserCommand::PreviewSound(sound_id) => self.preview_sound_now(sound_id),
            ContentBrowserCommand::PreviewItem(id) => {
                if let Some(item) = self.resolve_content_item(&id) {
                    self.content_browser.preview_item = Some(id);
                    if self.content_item_thumbnail(&item, true).is_none() {
                        self.log.push(format!(
                            "A real-time preview is not available yet for '{}'; showing its type artwork.",
                            item.title
                        ));
                    }
                }
            }
            ContentBrowserCommand::Inspect => {}
            ContentBrowserCommand::AssignMusic(entry) => {
                let current = self.current_stage_music();
                self.set_current_stage_music(Some(ProjectStageMusic {
                    bgm_id: entry.bgm_id,
                    wave_scene_id: entry.wave_scene_id,
                    secondary_bgm_id: current.and_then(|music| music.secondary_bgm_id),
                    secondary_wave_scene_id: current
                        .and_then(|music| music.secondary_wave_scene_id),
                }));
            }
            ContentBrowserCommand::AssignSound(sound_id) => {
                self.assign_sound_to_selected_helper(sound_id);
            }
            ContentBrowserCommand::PreviewGameFile(id) => {
                self.content_browser.preview_item = Some(ContentItemId::GameFile(id.clone()));
                self.request_raw_content_preview(&id);
            }
            ContentBrowserCommand::CopyText(text) => {
                self.log.push(format!("Copied virtual path: {text}"));
            }
        }
    }

    pub(super) fn content_browser_inspector_panel(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.content_browser.inspector_active {
            return false;
        }
        let Some(selected) = self.content_browser.selected.clone() else {
            return false;
        };
        let Some(item) = self.resolve_content_item(&selected) else {
            ui.heading("Content");
            ui.colored_label(
                egui::Color32::from_rgb(241, 126, 104),
                "This item is no longer available in the current project or game index.",
            );
            ui.monospace(selected.stable_key());
            if ui.button("Clear Selection").clicked() {
                self.content_browser.selected = None;
                self.content_browser.inspector_active = false;
            }
            return true;
        };

        let preview_texture = self.content_item_thumbnail(&item, true);
        let (preview_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 82.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(preview_rect, 8.0, egui::Color32::from_rgb(36, 40, 41));
        paint_content_icon(
            ui.painter(),
            egui::Rect::from_center_size(preview_rect.center(), egui::vec2(58.0, 58.0)),
            item.kind,
            content_source_accent(item.source),
        );
        if let Some(texture) = preview_texture {
            paint_inspector_thumbnail(ui, preview_rect, &texture);
        }
        ui.heading(&item.title);
        ui.label(&item.subtitle);
        ui.small(&item.detail);
        ui.horizontal_wrapped(|ui| {
            for badge in content_badges(&item) {
                ui.label(
                    egui::RichText::new(badge)
                        .small()
                        .color(content_source_accent(item.source))
                        .background_color(egui::Color32::from_rgb(38, 43, 44)),
                );
            }
        });
        ui.separator();
        ui.label(egui::RichText::new("Provenance").strong());
        ui.label(match item.source {
            ContentSource::Project => "Project Content — editable and project-scoped",
            ContentSource::Game => "Original Game Content — immutable retail data",
        });
        ui.monospace(item.stable_key());

        if let ContentItemId::GameFile(raw_id) = &item.id {
            if let Some(metadata) = self.raw_metadata_by_id(raw_id).cloned() {
                ui.separator();
                ui.label(egui::RichText::new("File Metadata").strong());
                ui.label(format!("Type: {}", game_resource_kind_label(metadata.kind)));
                ui.label(format!("Size: {}", format_bytes_short(metadata.size_bytes)));
                ui.label(format!(
                    "Physical source: {}",
                    metadata.physical_relative_path.display()
                ));
                ui.label(format!("Virtual path: {}", metadata.display_path));
                for warning in self
                    .game_content_index
                    .warnings
                    .iter()
                    .filter(|warning| warning.relative_path == metadata.physical_relative_path)
                {
                    ui.colored_label(
                        egui::Color32::from_rgb(241, 126, 104),
                        format!("Index warning: {}", warning.message),
                    );
                }
                if let Some(archive) = &metadata.archive_entry {
                    ui.label(format!("RARC flags: 0x{:02X}", archive.flags));
                    ui.collapsing("Exact archive identity", |ui| {
                        ui.monospace(hex_bytes(&archive.raw_path));
                        ui.small("Display decoding is never used as resource identity.");
                    });
                }
                self.raw_content_preview_panel(ui, raw_id);
            }
        }

        ui.separator();
        let mut command = None;
        match &item.id {
            ContentItemId::GameFolder(path) => {
                if ui.button("Open Folder").clicked() {
                    command = Some(ContentBrowserCommand::OpenGameFolder(path.clone()));
                }
            }
            ContentItemId::Stage { stage_id, .. } => {
                if ui.button("Open Stage").clicked() {
                    command = Some(ContentBrowserCommand::OpenStage(stage_id.clone()));
                }
            }
            ContentItemId::Object(factory_name) => {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(item.capabilities.place, egui::Button::new("Place"))
                        .on_hover_text("Arm placement mode; click the viewport to spawn")
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::ArmObject(factory_name.clone()));
                    }
                    if ui
                        .add_enabled(
                            item.capabilities.place,
                            egui::Button::new("Add at Camera Focus"),
                        )
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::SpawnObject(factory_name.clone()));
                    }
                });
            }
            ContentItemId::Model(asset_id) => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Edit Asset").clicked() {
                        command = Some(ContentBrowserCommand::EditModel(*asset_id));
                    }
                    if ui
                        .add_enabled(item.capabilities.place, egui::Button::new("Place"))
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::ArmModel(*asset_id));
                    }
                    if ui
                        .add_enabled(
                            item.capabilities.place,
                            egui::Button::new("Add at Camera Focus"),
                        )
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::SpawnModel(*asset_id));
                    }
                });
            }
            ContentItemId::Skybox { archive_path, .. } => {
                let entry = self
                    .retail_skyboxes
                    .iter()
                    .find(|entry| entry.archive_path.to_string_lossy() == archive_path.as_str())
                    .cloned();
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Preview").clicked() {
                        command = Some(ContentBrowserCommand::PreviewItem(item.id.clone()));
                    }
                    if ui
                        .add_enabled(
                            item.capabilities.apply && entry.is_some(),
                            egui::Button::new(format!("Apply to {}", self.stage_id)),
                        )
                        .clicked()
                    {
                        command = entry.map(ContentBrowserCommand::ApplySkybox);
                    }
                });
            }
            ContentItemId::Music {
                bgm_id,
                wave_scene_id,
            } => {
                let entry = RetailMusicEntry {
                    bgm_id: *bgm_id,
                    wave_scene_id: *wave_scene_id,
                    label: item.title.clone(),
                };
                ui.horizontal_wrapped(|ui| {
                    let playing = self.bgm_preview_is_active(*bgm_id);
                    let loading =
                        self.audio_preview_target_is_loading(AudioPreviewTarget::Music(*bgm_id));
                    if ui
                        .add_enabled(
                            !playing && !loading,
                            egui::Button::new(if loading { "Loading..." } else { "Preview" }),
                        )
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::PreviewMusic(*bgm_id));
                    }
                    if ui
                        .add_enabled(playing || loading, egui::Button::new("Stop"))
                        .clicked()
                    {
                        self.stop_audio_preview();
                    }
                    if ui
                        .add_enabled(
                            item.capabilities.assign,
                            egui::Button::new("Use as Stage Music"),
                        )
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::AssignMusic(entry));
                    }
                });
            }
            ContentItemId::Sound(sound_id) => {
                ui.horizontal_wrapped(|ui| {
                    let playing = self.sound_preview_is_active(*sound_id);
                    let loading =
                        self.audio_preview_target_is_loading(AudioPreviewTarget::Sound(*sound_id));
                    if ui
                        .add_enabled(
                            !playing && !loading,
                            egui::Button::new(if loading { "Loading..." } else { "Preview" }),
                        )
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::PreviewSound(*sound_id));
                    }
                    if ui
                        .add_enabled(playing || loading, egui::Button::new("Stop"))
                        .clicked()
                    {
                        self.stop_audio_preview();
                    }
                    if ui
                        .add_enabled(
                            item.capabilities.assign,
                            egui::Button::new("Assign to Selected Audio Helper"),
                        )
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::AssignSound(*sound_id));
                    }
                });
                if !item.capabilities.assign {
                    ui.small("Select a compatible point or rail audio helper first.");
                }
            }
            ContentItemId::GameFile(raw_id) => {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(item.capabilities.preview, egui::Button::new("Preview"))
                        .clicked()
                    {
                        command = Some(ContentBrowserCommand::PreviewGameFile(raw_id.clone()));
                    }
                    if ui.button("Copy Virtual Path").clicked() {
                        if let Some(metadata) = self.raw_metadata_by_id(raw_id) {
                            ui.ctx().copy_text(metadata.display_path.clone());
                            command = Some(ContentBrowserCommand::CopyText(
                                metadata.display_path.clone(),
                            ));
                        }
                    }
                });
                ui.small("Retail files are inspect-only; they cannot be edited, exported, imported, or dragged into the scene.");
            }
        }
        let favorite = self
            .content_browser
            .settings
            .favorites
            .contains(&item.stable_key());
        if ui
            .button(if favorite {
                "Remove from Favorites"
            } else {
                "Add to Favorites"
            })
            .clicked()
        {
            self.content_browser
                .settings
                .toggle_favorite(&item.stable_key());
            self.persist_content_browser_settings();
        }
        if let Some(command) = command {
            self.run_content_browser_command(command);
            self.remember_recent_item(&item);
        }
        true
    }

    pub(super) fn content_browser_preview_window(&mut self, ctx: &egui::Context) {
        let Some(preview_id) = self.content_browser.preview_item.clone() else {
            return;
        };
        let Some(item) = self.resolve_content_item(&preview_id) else {
            self.content_browser.preview_item = None;
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Preview — {}", item.title))
            .id(egui::Id::new("content-browser-preview-window"))
            .open(&mut open)
            .default_size(egui::vec2(720.0, 500.0))
            .min_size(egui::vec2(320.0, 240.0))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(&item.title).strong());
                    ui.separator();
                    ui.small(&item.subtitle);
                });
                ui.separator();

                if let Some(texture) = self.content_item_thumbnail(&item, true) {
                    let source = texture.size_vec2();
                    let available = ui.available_size().max(egui::vec2(64.0, 64.0));
                    let scale = (available.x / source.x)
                        .min(available.y / source.y)
                        .min(1.0);
                    ui.centered_and_justified(|ui| {
                        ui.image((texture.id(), source * scale));
                    });
                } else if let ContentItemId::GameFile(raw_id) = &item.id {
                    self.raw_content_preview_panel(ui, raw_id);
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Preparing preview…");
                        });
                    });
                }
            });
        if !open {
            self.content_browser.preview_item = None;
        }
    }

    fn resolve_content_item(&self, id: &ContentItemId) -> Option<ContentItemSummary> {
        if let ContentItemId::GameFolder(path) = id {
            return Some(game_folder_content_item(path));
        }
        if let ContentItemId::GameFile(raw_id) = id {
            return self.raw_metadata_by_id(raw_id).map(raw_content_item);
        }
        self.all_content_items()
            .into_iter()
            .find(|item| &item.id == id)
    }

    fn content_item_thumbnail(
        &mut self,
        item: &ContentItemSummary,
        prioritized: bool,
    ) -> Option<egui::TextureHandle> {
        match &item.id {
            ContentItemId::Model(_) if !prioritized => None,
            ContentItemId::Model(asset_id) => {
                let key = self.queue_project_model_thumbnail(*asset_id, prioritized)?;
                self.content_thumbnail_texture_by_key(&key)
            }
            ContentItemId::Object(_) if !prioritized => None,
            ContentItemId::Object(factory_name) => {
                let metadata = self.object_preview_metadata(factory_name)?;
                if prioritized {
                    self.queue_selected_content_thumbnail(metadata.clone());
                } else {
                    self.queue_raw_content_thumbnail(metadata.clone());
                }
                self.content_thumbnail_texture_for_metadata(&metadata)
            }
            ContentItemId::Skybox { archive_path, .. } => {
                let metadata = self.skybox_preview_metadata(Path::new(archive_path))?;
                let key = self.queue_skybox_content_thumbnail(metadata, prioritized);
                self.content_thumbnail_texture_by_key(&key)
            }
            ContentItemId::GameFile(raw_id) if item.capabilities.preview => {
                let metadata = self.raw_metadata_by_id(raw_id)?.clone();
                if prioritized {
                    self.queue_selected_content_thumbnail(metadata.clone());
                } else {
                    self.queue_raw_content_thumbnail(metadata.clone());
                }
                self.content_thumbnail_texture_for_metadata(&metadata)
            }
            _ => None,
        }
    }

    fn visible_raw_thumbnail(
        &mut self,
        metadata: &GameFileMetadata,
    ) -> Option<egui::TextureHandle> {
        if metadata.kind != GameResourceKind::Texture {
            return None;
        }
        self.queue_raw_content_thumbnail(metadata.clone());
        self.content_thumbnail_texture_for_metadata(metadata)
    }

    fn object_preview_metadata(&self, factory_name: &str) -> Option<GameFileMetadata> {
        let template = self.object_authoring_catalog.find(factory_name)?;
        let raw_path = template.preview_resource_path.as_deref()?;
        let archive_path = self
            .scene_archives
            .iter()
            .find(|archive| {
                archive
                    .stage_id
                    .eq_ignore_ascii_case(&template.source_stage)
            })
            .map(|archive| archive.path.as_path())?;
        self.archive_preview_metadata(archive_path, |candidate| candidate == raw_path)
    }

    fn skybox_preview_metadata(&self, archive_path: &Path) -> Option<GameFileMetadata> {
        self.archive_preview_metadata(archive_path, |candidate| {
            String::from_utf8_lossy(candidate)
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with("map/map/sky.bmd")
        })
    }

    fn archive_preview_metadata(
        &self,
        archive_path: &Path,
        raw_path_matches: impl Fn(&[u8]) -> bool,
    ) -> Option<GameFileMetadata> {
        let base_root = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.base_game_root.as_path())
            .unwrap_or_else(|| Path::new(self.base_root.trim()));
        if let Ok(relative_path) = archive_path.strip_prefix(base_root) {
            if let Some(indices) = self.game_content_index.by_physical_path.get(relative_path) {
                return indices.iter().find_map(|index| {
                    let metadata = self.game_content_index.entries.get(*index)?;
                    let archive = metadata.archive_entry.as_ref()?;
                    raw_path_matches(&archive.raw_path).then(|| metadata.clone())
                });
            }
        }
        self.game_content_index
            .entries
            .iter()
            .find(|metadata| {
                let Some(archive) = &metadata.archive_entry else {
                    return false;
                };
                same_physical_path(
                    &base_root.join(&metadata.physical_relative_path),
                    archive_path,
                ) && raw_path_matches(&archive.raw_path)
            })
            .cloned()
    }

    fn raw_metadata_by_id(&self, raw_id: &str) -> Option<&GameFileMetadata> {
        self.game_content_index
            .by_stable_id
            .get(raw_id)
            .and_then(|index| self.game_content_index.entries.get(*index))
    }
}

#[derive(Default)]
struct ContentItemAction {
    select: bool,
    toggle_favorite: bool,
    command: Option<ContentBrowserCommand>,
}

fn content_item_action(
    response: &egui::Response,
    item: &ContentItemSummary,
    favorite: bool,
) -> ContentItemAction {
    let favorite_hit = response
        .interact_pointer_pos()
        .is_some_and(|position| favorite_rect(response.rect).contains(position));
    let primary_clicked = response.clicked();
    let mut action = ContentItemAction {
        select: (primary_clicked && !favorite_hit) || response.secondary_clicked(),
        toggle_favorite: primary_clicked && favorite_hit,
        command: None,
    };
    if !favorite_hit {
        action.command = content_activation_command(item, response.double_clicked());
    }
    response.context_menu(|ui| {
        if let Some(command) = safe_default_command(item) {
            if ui.button(default_action_label(item)).clicked() {
                action.command = Some(command);
                ui.close();
            }
        }
        if let Some(command) = explicit_content_command(item) {
            if ui.button(explicit_action_label(item)).clicked() {
                action.command = Some(command);
                ui.close();
            }
        }
        if let ContentItemId::GameFile(raw_id) = &item.id {
            if ui.button("Copy Virtual Path").clicked() {
                ui.ctx().copy_text(item.subtitle.clone());
                action.command = Some(ContentBrowserCommand::CopyText(item.subtitle.clone()));
                let _ = raw_id;
                ui.close();
            }
        }
        ui.separator();
        if ui
            .button(if favorite {
                "Remove from Favorites"
            } else {
                "Add to Favorites"
            })
            .clicked()
        {
            action.toggle_favorite = true;
            ui.close();
        }
    });
    action
}

fn content_activation_command(
    item: &ContentItemSummary,
    double_clicked: bool,
) -> Option<ContentBrowserCommand> {
    double_clicked.then(|| safe_default_command(item)).flatten()
}

fn safe_default_command(item: &ContentItemSummary) -> Option<ContentBrowserCommand> {
    match &item.id {
        ContentItemId::GameFolder(path) => {
            Some(ContentBrowserCommand::OpenGameFolder(path.clone()))
        }
        ContentItemId::Stage { stage_id, .. } => {
            Some(ContentBrowserCommand::OpenStage(stage_id.clone()))
        }
        ContentItemId::Object(factory_name) if item.capabilities.place => {
            Some(ContentBrowserCommand::ArmObject(factory_name.clone()))
        }
        ContentItemId::Model(asset_id) if item.capabilities.place => {
            Some(ContentBrowserCommand::ArmModel(*asset_id))
        }
        ContentItemId::Skybox { .. } => Some(ContentBrowserCommand::PreviewItem(item.id.clone())),
        ContentItemId::Music { bgm_id, .. } => Some(ContentBrowserCommand::PreviewMusic(*bgm_id)),
        ContentItemId::Sound(sound_id) => Some(ContentBrowserCommand::PreviewSound(*sound_id)),
        ContentItemId::GameFile(raw_id) if item.capabilities.preview => {
            Some(ContentBrowserCommand::PreviewGameFile(raw_id.clone()))
        }
        ContentItemId::GameFile(_) => Some(ContentBrowserCommand::Inspect),
        _ => None,
    }
}

fn explicit_content_command(item: &ContentItemSummary) -> Option<ContentBrowserCommand> {
    match &item.id {
        ContentItemId::Object(factory_name) if item.capabilities.place => {
            Some(ContentBrowserCommand::SpawnObject(factory_name.clone()))
        }
        ContentItemId::Model(asset_id) if item.capabilities.place => {
            Some(ContentBrowserCommand::SpawnModel(*asset_id))
        }
        ContentItemId::Skybox { archive_path, .. } if item.capabilities.apply => {
            Some(ContentBrowserCommand::ApplySkyboxPath(archive_path.clone()))
        }
        ContentItemId::Music {
            bgm_id,
            wave_scene_id,
        } if item.capabilities.assign => {
            Some(ContentBrowserCommand::AssignMusic(RetailMusicEntry {
                bgm_id: *bgm_id,
                wave_scene_id: *wave_scene_id,
                label: item.title.clone(),
            }))
        }
        ContentItemId::Sound(sound_id) if item.capabilities.assign => {
            Some(ContentBrowserCommand::AssignSound(*sound_id))
        }
        _ => None,
    }
}

fn compact_primary_action(
    ui: &mut egui::Ui,
    item: &ContentItemSummary,
    _app: &SmsEditorApp,
) -> Option<ContentBrowserCommand> {
    let command = safe_default_command(item)?;
    ui.small_button(default_action_label(item))
        .clicked()
        .then_some(command)
}

fn default_action_label(item: &ContentItemSummary) -> &'static str {
    match item.kind {
        ContentKind::Folder => "Open",
        ContentKind::Stage => "Open",
        ContentKind::Object | ContentKind::Model => "Place",
        ContentKind::Music | ContentKind::Sound | ContentKind::Skybox => "Preview",
        ContentKind::GameFile if item.capabilities.preview => "Preview",
        ContentKind::GameFile => "Inspect",
    }
}

fn explicit_action_label(item: &ContentItemSummary) -> &'static str {
    match item.kind {
        ContentKind::Folder => "Open",
        ContentKind::Object | ContentKind::Model => "Add at Camera Focus",
        ContentKind::Skybox => "Apply to Stage",
        ContentKind::Music => "Use as Stage Music",
        ContentKind::Sound => "Assign to Selected Audio Helper",
        _ => "Use",
    }
}

fn all_content_nodes() -> [ContentNode; 11] {
    [
        ContentNode::All,
        ContentNode::Favorites,
        ContentNode::Recent,
        ContentNode::ProjectStages,
        ContentNode::ProjectModels,
        ContentNode::GameStages,
        ContentNode::GameObjects,
        ContentNode::GameSkyboxes,
        ContentNode::GameMusic,
        ContentNode::GameSounds,
        ContentNode::GameFiles,
    ]
}

fn source_tree_root_heading(
    ui: &mut egui::Ui,
    label: &str,
    expanded: bool,
    color: egui::Color32,
) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(format!("{} {label}", if expanded { "▾" } else { "▸" }))
                .small()
                .color(color),
        )
        .frame(false),
    )
    .clicked()
}

fn source_tree_node(
    ui: &mut egui::Ui,
    selected: ContentNode,
    node: ContentNode,
    indent: f32,
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        clicked = ui
            .selectable_label(selected == node, node.label())
            .clicked();
    });
    clicked
}

fn compare_content_name(
    left: &ContentItemSummary,
    right: &ContentItemSummary,
) -> std::cmp::Ordering {
    left.title
        .to_ascii_lowercase()
        .cmp(&right.title.to_ascii_lowercase())
        .then_with(|| left.subtitle.cmp(&right.subtitle))
}

fn query_matches(search_text: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|term| search_text.contains(term))
}

fn content_source_accent(source: ContentSource) -> egui::Color32 {
    match source {
        ContentSource::Project => egui::Color32::from_rgb(48, 176, 190),
        ContentSource::Game => egui::Color32::from_rgb(232, 186, 72),
    }
}

fn content_grid_card(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    selected: bool,
    favorite: bool,
    item: &ContentItemSummary,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let accent = content_source_accent(item.source);
    let fill = if selected {
        accent.gamma_multiply(0.18)
    } else if response.hovered() {
        egui::Color32::from_rgb(48, 53, 54)
    } else {
        egui::Color32::from_rgb(37, 41, 42)
    };
    ui.painter().rect_filled(rect, 7.0, fill);
    ui.painter().rect_stroke(
        rect,
        7.0,
        egui::Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                accent
            } else {
                egui::Color32::from_rgb(62, 68, 69)
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().rect_filled(
        egui::Rect::from_min_max(
            rect.left_top(),
            egui::pos2(rect.left() + 4.0, rect.bottom()),
        ),
        7.0,
        accent,
    );

    let inner = rect.shrink2(egui::vec2(10.0, 8.0));
    let icon_rect = egui::Rect::from_min_size(inner.left_top(), egui::vec2(42.0, 42.0));
    paint_content_icon(ui.painter(), icon_rect, item.kind, accent);
    paint_star(
        ui.painter(),
        favorite_rect(rect).center(),
        7.0,
        if favorite {
            egui::Color32::from_rgb(241, 199, 77)
        } else {
            egui::Color32::from_rgb(112, 119, 120)
        },
        favorite,
    );

    let text_left = icon_rect.right() + 9.0;
    let text_right = favorite_rect(rect).left() - 4.0;
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(text_left, inner.top()),
            egui::pos2(text_right, inner.top() + 21.0),
        ),
        egui::RichText::new(&item.title).strong(),
    );
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(text_left, inner.top() + 22.0),
            egui::pos2(inner.right(), inner.top() + 42.0),
        ),
        egui::RichText::new(&item.subtitle).small().weak(),
    );
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(inner.left(), inner.top() + 52.0),
            egui::pos2(inner.right(), inner.top() + 72.0),
        ),
        egui::RichText::new(&item.detail).small(),
    );
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(inner.left(), inner.bottom() - 20.0),
            inner.right_bottom(),
        ),
        egui::RichText::new(content_badges(item).join("  "))
            .small()
            .color(accent),
    );
    response.on_hover_text(format!(
        "{}\n{}\nSingle-click selects. Double-click runs the safe default.",
        item.subtitle, item.detail
    ))
}

fn content_list_row(
    ui: &mut egui::Ui,
    selected: bool,
    favorite: bool,
    item: &ContentItemSummary,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LIST_ROW_HEIGHT - 3.0),
        egui::Sense::click_and_drag(),
    );
    let accent = content_source_accent(item.source);
    ui.painter().rect_filled(
        rect,
        4.0,
        if selected {
            accent.gamma_multiply(0.16)
        } else if response.hovered() {
            egui::Color32::from_rgb(47, 52, 53)
        } else {
            egui::Color32::from_rgb(34, 38, 39)
        },
    );
    if selected {
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.5, accent),
            egui::StrokeKind::Inside,
        );
    }

    let inner = rect.shrink2(egui::vec2(7.0, 4.0));
    let icon_rect = egui::Rect::from_min_size(inner.left_top(), egui::vec2(29.0, 29.0));
    paint_content_icon(ui.painter(), icon_rect, item.kind, accent);
    paint_star(
        ui.painter(),
        favorite_rect(rect).center(),
        7.0,
        if favorite {
            egui::Color32::from_rgb(241, 199, 77)
        } else {
            egui::Color32::from_rgb(112, 119, 120)
        },
        favorite,
    );

    let left = icon_rect.right() + 8.0;
    let badge_left = (inner.right() - 120.0).max(left + 60.0);
    let detail_left = (badge_left - 180.0).max(left + 80.0);
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(left, inner.top()),
            egui::pos2(detail_left - 8.0, inner.top() + 18.0),
        ),
        egui::RichText::new(&item.title).strong(),
    );
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(left, inner.top() + 18.0),
            egui::pos2(detail_left - 8.0, inner.bottom()),
        ),
        egui::RichText::new(&item.subtitle).small().weak(),
    );
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(detail_left, inner.top() + 7.0),
            egui::pos2(badge_left - 8.0, inner.bottom()),
        ),
        egui::RichText::new(&item.detail).small(),
    );
    paint_truncated(
        ui,
        egui::Rect::from_min_max(
            egui::pos2(badge_left, inner.top() + 7.0),
            egui::pos2(favorite_rect(rect).left() - 5.0, inner.bottom()),
        ),
        egui::RichText::new(content_badges(item).join(" "))
            .small()
            .color(accent),
    );
    response.on_hover_text(format!("{}\n{}", item.subtitle, item.detail))
}

fn favorite_rect(card_rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(card_rect.right() - 29.0, card_rect.top() + 5.0),
        egui::vec2(24.0, 24.0),
    )
}

fn paint_truncated(ui: &egui::Ui, rect: egui::Rect, text: egui::RichText) {
    if rect.width() <= 1.0 {
        return;
    }
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    ui.painter()
        .galley(rect.left_top(), galley, ui.visuals().text_color());
}

fn paint_content_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    kind: ContentKind,
    color: egui::Color32,
) {
    let rect = rect.shrink(4.0);
    let center = rect.center();
    let stroke = egui::Stroke::new((rect.width() / 18.0).clamp(1.2, 2.2), color);
    match kind {
        ContentKind::Folder => {
            let body = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + rect.height() * 0.28),
                rect.right_bottom(),
            );
            painter.rect_filled(body, 3.0, color.gamma_multiply(0.22));
            painter.rect_stroke(body, 3.0, stroke, egui::StrokeKind::Inside);
            let tab = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 3.0, rect.top() + rect.height() * 0.14),
                egui::pos2(center.x + 2.0, rect.top() + rect.height() * 0.34),
            );
            painter.rect_filled(tab, 2.0, color.gamma_multiply(0.22));
            painter.rect_stroke(tab, 2.0, stroke, egui::StrokeKind::Inside);
        }
        ContentKind::Stage => {
            painter.circle_stroke(center, rect.width() * 0.43, stroke);
            painter.circle_filled(
                egui::pos2(
                    center.x + rect.width() * 0.18,
                    center.y - rect.height() * 0.16,
                ),
                rect.width() * 0.08,
                color,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left(), rect.bottom() - 4.0),
                    egui::pos2(center.x - 3.0, center.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x - 3.0, center.y),
                    egui::pos2(rect.right(), rect.bottom() - 4.0),
                ],
                stroke,
            );
        }
        ContentKind::Object => {
            painter.circle_stroke(
                egui::pos2(center.x, rect.top() + rect.height() * 0.22),
                rect.width() * 0.15,
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, rect.top() + rect.height() * 0.37),
                    egui::pos2(center.x, rect.bottom() - 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 4.0, center.y),
                    egui::pos2(rect.right() - 4.0, center.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, rect.bottom() - 5.0),
                    rect.left_bottom(),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, rect.bottom() - 5.0),
                    rect.right_bottom(),
                ],
                stroke,
            );
        }
        ContentKind::Model => {
            let top = egui::pos2(center.x, rect.top());
            let right = egui::pos2(rect.right(), center.y - 3.0);
            let bottom = egui::pos2(center.x, rect.bottom());
            let left = egui::pos2(rect.left(), center.y - 3.0);
            for edge in [
                [top, right],
                [right, bottom],
                [bottom, left],
                [left, top],
                [top, center],
                [center, right],
                [center, bottom],
                [center, left],
            ] {
                painter.line_segment(edge, stroke);
            }
        }
        ContentKind::Skybox => {
            painter.circle_stroke(center, rect.width() * 0.43, stroke);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 2.0, center.y),
                    egui::pos2(rect.right() - 2.0, center.y),
                ],
                stroke,
            );
            painter.circle_filled(
                egui::pos2(
                    center.x + rect.width() * 0.15,
                    center.y - rect.height() * 0.18,
                ),
                rect.width() * 0.08,
                color,
            );
        }
        ContentKind::Music => {
            let stem_x = center.x + rect.width() * 0.12;
            painter.line_segment(
                [
                    egui::pos2(stem_x, rect.top()),
                    egui::pos2(stem_x, rect.bottom() - 7.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(stem_x, rect.top()),
                    egui::pos2(rect.right(), rect.top() + 4.0),
                ],
                stroke,
            );
            painter.circle_filled(
                egui::pos2(stem_x - 5.0, rect.bottom() - 5.0),
                rect.width() * 0.15,
                color,
            );
        }
        ContentKind::Sound => {
            painter.line_segment(
                [
                    egui::pos2(rect.left(), center.y - 4.0),
                    egui::pos2(center.x - 5.0, center.y - 4.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y - 4.0),
                    egui::pos2(center.x + 3.0, rect.top() + 4.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x + 3.0, rect.top() + 4.0),
                    egui::pos2(center.x + 3.0, rect.bottom() - 4.0),
                ],
                stroke,
            );
            painter.circle_stroke(
                egui::pos2(center.x + 3.0, center.y),
                rect.width() * 0.31,
                stroke,
            );
        }
        ContentKind::GameFile => {
            let page = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 5.0, rect.top()),
                egui::pos2(rect.right() - 4.0, rect.bottom()),
            );
            painter.rect_stroke(page, 2.0, stroke, egui::StrokeKind::Inside);
            for offset in [0.42, 0.62, 0.8] {
                painter.line_segment(
                    [
                        egui::pos2(page.left() + 5.0, page.top() + page.height() * offset),
                        egui::pos2(page.right() - 4.0, page.top() + page.height() * offset),
                    ],
                    stroke,
                );
            }
        }
    }
}

fn paint_star(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: egui::Color32,
    filled: bool,
) {
    let mut points = Vec::with_capacity(10);
    for index in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::PI / 5.0;
        let point_radius = if index % 2 == 0 {
            radius
        } else {
            radius * 0.42
        };
        points.push(center + egui::vec2(angle.cos(), angle.sin()) * point_radius);
    }
    if filled {
        painter.add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    } else {
        painter.add(egui::Shape::closed_line(
            points,
            egui::Stroke::new(1.2, color),
        ));
    }
}

fn content_badges(item: &ContentItemSummary) -> Vec<&'static str> {
    let mut badges = vec![match item.source {
        ContentSource::Project => "PROJECT",
        ContentSource::Game => "GAME",
    }];
    badges.push(item.kind.label());
    if item.source == ContentSource::Game && !item.capabilities.editable {
        badges.push("READ ONLY");
    }
    if item.capabilities.open {
        badges.push("OPEN");
    }
    if item.capabilities.place {
        badges.push("PLACEABLE");
    }
    if item.capabilities.editable {
        badges.push("EDITABLE");
    }
    if item.capabilities.preview {
        badges.push("PREVIEW");
    }
    if item.capabilities.inspect && !item.capabilities.preview {
        badges.push("INSPECT");
    }
    if item.selected_in_scene {
        badges.push("ACTIVE");
    }
    badges
}

fn content_item_matches_facets(
    item: &ContentItemSummary,
    source: Option<ContentSource>,
    kind: Option<ContentKind>,
    capability: Option<ContentCapability>,
) -> bool {
    source.is_none_or(|source| item.source == source)
        && kind.is_none_or(|kind| item.kind == kind)
        && capability.is_none_or(|capability| item.capabilities.supports(capability))
}

fn raw_content_capabilities(metadata: &GameFileMetadata) -> ContentCapabilities {
    let path = metadata.display_path.to_ascii_lowercase();
    ContentCapabilities {
        preview: path.ends_with(".bti")
            || path.ends_with(".bmp")
            || path.ends_with(".bmd")
            || path.ends_with(".bdl"),
        inspect: true,
        ..ContentCapabilities::default()
    }
}

fn game_file_virtual_components(metadata: &GameFileMetadata) -> Vec<String> {
    if let Some(archive_entry) = &metadata.archive_entry {
        let physical = metadata
            .display_path
            .split_once("!/")
            .map_or(metadata.display_path.as_str(), |(physical, _)| physical);
        let mut components = physical
            .split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if let Some(archive_name) = components.last_mut() {
            archive_name.push('!');
        }
        components.extend(
            archive_entry
                .display_path
                .split(['/', '\\'])
                .filter(|component| !component.is_empty())
                .map(str::to_string),
        );
        return components;
    }

    let mut components = metadata
        .display_path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if metadata.kind == GameResourceKind::Archive {
        if let Some(archive_name) = components.last_mut() {
            archive_name.push('!');
        }
    }
    components
}

fn game_folder_content_item(path: &[String]) -> ContentItemSummary {
    let title = path
        .last()
        .map(|part| part.trim_end_matches('!'))
        .unwrap_or("Game Files")
        .to_string();
    let archive = path.last().is_some_and(|part| part.ends_with('!'));
    ContentItemSummary {
        id: ContentItemId::GameFolder(path.to_vec()),
        kind: ContentKind::Folder,
        source: ContentSource::Game,
        title,
        subtitle: path
            .iter()
            .map(|part| part.trim_end_matches('!'))
            .collect::<Vec<_>>()
            .join("/"),
        detail: if archive {
            "Archive contents".to_string()
        } else {
            "Folder".to_string()
        },
        search_text: path.join("/").to_ascii_lowercase(),
        capabilities: ContentCapabilities {
            open: true,
            inspect: true,
            ..ContentCapabilities::default()
        },
        selected_in_scene: false,
    }
}

fn raw_content_item(metadata: &GameFileMetadata) -> ContentItemSummary {
    let title = metadata
        .display_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&metadata.display_path)
        .to_string();
    ContentItemSummary {
        id: ContentItemId::GameFile(raw_game_file_id(&metadata.id)),
        kind: ContentKind::GameFile,
        source: ContentSource::Game,
        title,
        subtitle: metadata.display_path.clone(),
        detail: format!(
            "{} / {}",
            game_resource_kind_label(metadata.kind),
            format_bytes_short(metadata.size_bytes)
        ),
        search_text: metadata.display_path.to_ascii_lowercase(),
        capabilities: raw_content_capabilities(metadata),
        selected_in_scene: false,
    }
}

pub(super) fn raw_game_file_id(id: &GameFileId) -> String {
    match id {
        GameFileId::Physical { relative_path } => {
            format!("physical:{}", path_identity(relative_path))
        }
        GameFileId::ArchiveEntry {
            archive_relative_path,
            raw_entry_path,
        } => format!(
            "archive:{}:{}",
            path_identity(archive_relative_path),
            hex_bytes(raw_entry_path)
        ),
    }
}

fn path_identity(path: &Path) -> String {
    use std::fmt::Write as _;

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let mut result = String::new();
        for unit in path.as_os_str().encode_wide() {
            let _ = write!(result, "{unit:04x}");
        }
        result
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        hex_bytes(path.as_os_str().as_bytes())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut result = String::new();
        for character in path.to_string_lossy().chars() {
            let _ = write!(result, "{:08x}", u32::from(character));
        }
        result
    }
}

fn normalize_virtual_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn all_game_resource_kinds() -> [GameResourceKind; 15] {
    [
        GameResourceKind::Archive,
        GameResourceKind::Model,
        GameResourceKind::MaterialTable,
        GameResourceKind::Texture,
        GameResourceKind::Animation,
        GameResourceKind::Particle,
        GameResourceKind::Collision,
        GameResourceKind::Message,
        GameResourceKind::PlacementData,
        GameResourceKind::ParameterData,
        GameResourceKind::Script,
        GameResourceKind::Audio,
        GameResourceKind::Video,
        GameResourceKind::System,
        GameResourceKind::Other,
    ]
}

fn game_resource_kind_label(kind: GameResourceKind) -> &'static str {
    match kind {
        GameResourceKind::Archive => "Archives",
        GameResourceKind::Model => "Models",
        GameResourceKind::MaterialTable => "Material Tables",
        GameResourceKind::Texture => "Textures",
        GameResourceKind::Animation => "Animations",
        GameResourceKind::Particle => "Particles",
        GameResourceKind::Collision => "Collision",
        GameResourceKind::Message => "Messages",
        GameResourceKind::PlacementData => "Placement Data",
        GameResourceKind::ParameterData => "Parameter Data",
        GameResourceKind::Script => "Scripts",
        GameResourceKind::Audio => "Audio",
        GameResourceKind::Video => "Video",
        GameResourceKind::System => "System Files",
        GameResourceKind::Other => "Other",
    }
}

impl SmsEditorApp {
    pub(super) fn content_browser_keyboard(&mut self, ctx: &egui::Context) {
        if self.bottom_tab != BottomTab::Content || !self.content_browser.inspector_active {
            return;
        }
        let Some(id) = self.content_browser.selected.clone() else {
            return;
        };
        let Some(item) = self.resolve_content_item(&id) else {
            return;
        };
        let command = if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
            safe_default_command(&item)
        } else if ctx.input(|input| input.key_pressed(egui::Key::Space)) {
            match &item.id {
                ContentItemId::Music { bgm_id, .. } => {
                    Some(ContentBrowserCommand::PreviewMusic(*bgm_id))
                }
                ContentItemId::Sound(sound_id) => {
                    Some(ContentBrowserCommand::PreviewSound(*sound_id))
                }
                ContentItemId::Object(_)
                | ContentItemId::Model(_)
                | ContentItemId::Skybox { .. }
                    if item.capabilities.preview =>
                {
                    Some(ContentBrowserCommand::PreviewItem(item.id.clone()))
                }
                ContentItemId::GameFile(raw_id) if item.capabilities.preview => {
                    Some(ContentBrowserCommand::PreviewGameFile(raw_id.clone()))
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(command) = command {
            self.run_content_browser_command(command);
            self.remember_recent_item(&item);
        }
    }
}

impl SmsEditorApp {
    fn content_browser_combined_results(&mut self, ui: &mut egui::Ui) {
        let items = self.filtered_content_items();
        ui.label(
            egui::RichText::new("CURATED AUTHORING CONTENT")
                .small()
                .strong(),
        );
        let height = if items.is_empty() {
            62.0
        } else {
            ui.available_height().min(168.0)
        };
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| self.content_browser_results(ui, &items),
        );
        ui.separator();
        ui.label(
            egui::RichText::new("READ-ONLY GAME FILES")
                .small()
                .strong()
                .color(egui::Color32::from_rgb(232, 186, 72)),
        );
        self.content_browser_raw_results(ui);
    }
}

impl SmsEditorApp {
    fn content_browser_tree_splitter(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(6.0, ui.available_height().max(80.0)),
            egui::Sense::drag(),
        );
        let color = if response.hovered() || response.dragged() {
            egui::Color32::from_rgb(48, 176, 190)
        } else {
            egui::Color32::from_rgb(60, 66, 67)
        };
        ui.painter().line_segment(
            [rect.center_top(), rect.center_bottom()],
            egui::Stroke::new(1.5, color),
        );
        if response.dragged() {
            let delta = ui.input(|input| input.pointer.delta().x);
            self.content_browser.settings.tree_width =
                (self.content_browser.settings.tree_width + delta).clamp(150.0, 360.0);
            ui.ctx().request_repaint();
        }
        if response.drag_stopped() {
            self.persist_content_browser_settings();
        }
    }
}

impl SmsEditorApp {
    fn content_browser_collection_key(&self) -> String {
        if self.content_browser.node == ContentNode::GameFiles {
            if let Some(kind) = self.content_browser.raw_kind_filter {
                return format!(
                    "{}/{}",
                    self.content_browser.node.key(),
                    game_resource_kind_key(kind)
                );
            }
        }
        self.content_browser.node.key().to_string()
    }
}

fn game_resource_kind_key(kind: GameResourceKind) -> &'static str {
    match kind {
        GameResourceKind::Archive => "archives",
        GameResourceKind::Model => "models",
        GameResourceKind::MaterialTable => "materials",
        GameResourceKind::Texture => "textures",
        GameResourceKind::Animation => "animations",
        GameResourceKind::Particle => "particles",
        GameResourceKind::Collision => "collision",
        GameResourceKind::Message => "messages",
        GameResourceKind::PlacementData => "placement",
        GameResourceKind::ParameterData => "parameters",
        GameResourceKind::Script => "scripts",
        GameResourceKind::Audio => "audio",
        GameResourceKind::Video => "video",
        GameResourceKind::System => "system",
        GameResourceKind::Other => "other",
    }
}

fn game_resource_kind_from_key(key: &str) -> Option<GameResourceKind> {
    all_game_resource_kinds()
        .into_iter()
        .find(|kind| game_resource_kind_key(*kind) == key)
}

fn same_physical_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(right.to_string_lossy().as_ref())
    } else {
        left == right
    }
}

fn paint_inspector_thumbnail(
    ui: &egui::Ui,
    preview_rect: egui::Rect,
    texture: &egui::TextureHandle,
) {
    let source = texture.size_vec2();
    let scale = (preview_rect.width() / source.x)
        .min(preview_rect.height() / source.y)
        .min(1.0);
    let image_rect = egui::Rect::from_center_size(preview_rect.center(), source * scale);
    ui.painter().image(
        texture.id(),
        image_rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

fn paint_content_thumbnail(
    ui: &egui::Ui,
    card_rect: egui::Rect,
    texture: &egui::TextureHandle,
    grid: bool,
) {
    let inner = card_rect.shrink2(egui::vec2(10.0, if grid { 8.0 } else { 4.0 }));
    let target = egui::Rect::from_min_size(
        inner.left_top(),
        if grid {
            egui::vec2(42.0, 42.0)
        } else {
            egui::vec2(29.0, 29.0)
        },
    );
    let source = texture.size_vec2();
    let scale = (target.width() / source.x).min(target.height() / source.y);
    let size = source * scale;
    let image_rect = egui::Rect::from_center_size(target.center(), size);
    ui.painter()
        .rect_filled(target, 3.0, egui::Color32::from_rgb(22, 26, 27));
    ui.painter().image(
        texture.id(),
        image_rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
mod unified_browser_tests {
    use super::*;

    #[test]
    fn project_stage_ids_are_project_scoped() {
        let first = ContentItemId::Stage {
            stage_id: "custom0".to_string(),
            project_scope: Some("project-a".to_string()),
        };
        let second = ContentItemId::Stage {
            stage_id: "custom0".to_string(),
            project_scope: Some("project-b".to_string()),
        };
        assert_ne!(first.stable_key(), second.stable_key());
    }

    #[test]
    fn physical_file_ids_preserve_case_distinctions() {
        let upper = GameFileId::Physical {
            relative_path: "files/A.arc".into(),
        };
        let lower = GameFileId::Physical {
            relative_path: "files/a.arc".into(),
        };
        assert_ne!(raw_game_file_id(&upper), raw_game_file_id(&lower));
    }

    #[test]
    fn raw_non_utf8_paths_have_distinct_stable_ids() {
        let first = GameFileId::ArchiveEntry {
            archive_relative_path: "files/test.arc".into(),
            raw_entry_path: vec![b'a', 0x80, b'.', b'b'],
        };
        let second = GameFileId::ArchiveEntry {
            archive_relative_path: "files/test.arc".into(),
            raw_entry_path: vec![b'a', 0x81, b'.', b'b'],
        };
        assert_ne!(raw_game_file_id(&first), raw_game_file_id(&second));
        assert!(raw_game_file_id(&first).ends_with("61802e62"));
    }

    #[test]
    fn global_search_requires_every_term() {
        assert!(query_matches("bianco hills scene archive", "hills archive"));
        assert!(!query_matches(
            "bianco hills scene archive",
            "hills texture"
        ));
    }

    fn test_item(
        id: ContentItemId,
        kind: ContentKind,
        capabilities: ContentCapabilities,
    ) -> ContentItemSummary {
        ContentItemSummary {
            id,
            kind,
            source: ContentSource::Game,
            title: "Test".to_string(),
            subtitle: "test/path".to_string(),
            detail: String::new(),
            search_text: "test".to_string(),
            capabilities,
            selected_in_scene: false,
        }
    }

    #[test]
    fn single_click_never_dispatches_a_content_action() {
        let item = test_item(
            ContentItemId::Object("Coin".to_string()),
            ContentKind::Object,
            ContentCapabilities {
                place: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(content_activation_command(&item, false).is_none());
        assert!(matches!(
            content_activation_command(&item, true),
            Some(ContentBrowserCommand::ArmObject(factory)) if factory == "Coin"
        ));
    }

    #[test]
    fn safe_defaults_do_not_run_explicit_mutations() {
        let asset_id = sms_authoring::AssetId::new();
        let model = test_item(
            ContentItemId::Model(asset_id),
            ContentKind::Model,
            ContentCapabilities {
                place: true,
                editable: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(matches!(
            safe_default_command(&model),
            Some(ContentBrowserCommand::ArmModel(id)) if id == asset_id
        ));
        assert!(matches!(
            explicit_content_command(&model),
            Some(ContentBrowserCommand::SpawnModel(id)) if id == asset_id
        ));

        let skybox = test_item(
            ContentItemId::Skybox {
                stage_id: "dolpic0".to_string(),
                archive_path: "files/scene/map/map/sky.bmd".to_string(),
            },
            ContentKind::Skybox,
            ContentCapabilities {
                apply: true,
                preview: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(matches!(
            safe_default_command(&skybox),
            Some(ContentBrowserCommand::PreviewItem(ContentItemId::Skybox { stage_id, .. }))
                if stage_id == "dolpic0"
        ));
        assert!(matches!(
            explicit_content_command(&skybox),
            Some(ContentBrowserCommand::ApplySkyboxPath(path))
                if path == "files/scene/map/map/sky.bmd"
        ));

        let music = test_item(
            ContentItemId::Music {
                bgm_id: 0x8001_0001,
                wave_scene_id: 3,
            },
            ContentKind::Music,
            ContentCapabilities {
                assign: true,
                preview: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(matches!(
            safe_default_command(&music),
            Some(ContentBrowserCommand::PreviewMusic(0x8001_0001))
        ));
        assert!(matches!(
            explicit_content_command(&music),
            Some(ContentBrowserCommand::AssignMusic(entry))
                if entry.bgm_id == 0x8001_0001 && entry.wave_scene_id == 3
        ));

        let sound = test_item(
            ContentItemId::Sound(0x1234),
            ContentKind::Sound,
            ContentCapabilities {
                assign: true,
                preview: true,
                inspect: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(matches!(
            safe_default_command(&sound),
            Some(ContentBrowserCommand::PreviewSound(0x1234))
        ));
        assert!(matches!(
            explicit_content_command(&sound),
            Some(ContentBrowserCommand::AssignSound(0x1234))
        ));
    }

    #[test]
    fn raw_safe_default_respects_preview_capability() {
        let previewable = test_item(
            ContentItemId::GameFile("files/test.bti".to_string()),
            ContentKind::GameFile,
            ContentCapabilities {
                preview: true,
                inspect: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(matches!(
            safe_default_command(&previewable),
            Some(ContentBrowserCommand::PreviewGameFile(path)) if path == "files/test.bti"
        ));

        let inspect_only = test_item(
            ContentItemId::GameFile("sys/main.dol".to_string()),
            ContentKind::GameFile,
            ContentCapabilities {
                inspect: true,
                ..ContentCapabilities::default()
            },
        );
        assert!(matches!(
            safe_default_command(&inspect_only),
            Some(ContentBrowserCommand::Inspect)
        ));
    }

    #[test]
    fn stable_ids_cover_each_source_specific_identity() {
        let model = sms_authoring::AssetId::new();
        assert!(ContentItemId::Model(model)
            .stable_key()
            .contains(&model.to_string()));
        assert_ne!(
            ContentItemId::Object("Coin".to_string()).stable_key(),
            ContentItemId::Object("Shine".to_string()).stable_key()
        );
        assert_ne!(
            ContentItemId::Skybox {
                stage_id: "dolpic0".to_string(),
                archive_path: "files/a.arc".to_string(),
            }
            .stable_key(),
            ContentItemId::Skybox {
                stage_id: "dolpic0".to_string(),
                archive_path: "files/b.arc".to_string(),
            }
            .stable_key()
        );
        assert_ne!(
            ContentItemId::Music {
                bgm_id: 1,
                wave_scene_id: 2,
            }
            .stable_key(),
            ContentItemId::Music {
                bgm_id: 1,
                wave_scene_id: 3,
            }
            .stable_key()
        );
        assert_ne!(
            ContentItemId::Sound(1).stable_key(),
            ContentItemId::Sound(2).stable_key()
        );
    }

    #[test]
    fn source_type_and_capability_facets_compose() {
        let item = ContentItemSummary {
            source: ContentSource::Project,
            ..test_item(
                ContentItemId::Model(sms_authoring::AssetId::new()),
                ContentKind::Model,
                ContentCapabilities {
                    place: true,
                    preview: true,
                    editable: true,
                    ..ContentCapabilities::default()
                },
            )
        };
        assert!(content_item_matches_facets(
            &item,
            Some(ContentSource::Project),
            Some(ContentKind::Model),
            Some(ContentCapability::Preview),
        ));
        assert!(!content_item_matches_facets(
            &item,
            Some(ContentSource::Game),
            Some(ContentKind::Model),
            Some(ContentCapability::Preview),
        ));
        assert!(!content_item_matches_facets(
            &item,
            Some(ContentSource::Project),
            Some(ContentKind::Stage),
            None,
        ));
        assert!(!content_item_matches_facets(
            &item,
            None,
            None,
            Some(ContentCapability::Apply),
        ));
    }

    #[test]
    fn raw_preview_capability_matches_available_decoders() {
        let metadata = |path: &str, kind| GameFileMetadata {
            id: GameFileId::Physical {
                relative_path: path.into(),
            },
            display_path: path.to_string(),
            physical_relative_path: path.into(),
            kind,
            size_bytes: 32,
            modified_unix_nanos: None,
            archive_entry: None,
        };
        for (path, kind) in [
            ("files/test.bti", GameResourceKind::Texture),
            ("files/test.bmp", GameResourceKind::Texture),
            ("files/test.bmd", GameResourceKind::Model),
            ("files/test.bdl", GameResourceKind::Model),
        ] {
            assert!(raw_content_capabilities(&metadata(path, kind)).preview);
        }
        assert!(
            !raw_content_capabilities(&metadata("files/test.tpl", GameResourceKind::Texture,))
                .preview
        );
    }

    #[test]
    fn every_raw_resource_kind_remains_non_mutating() {
        for (index, kind) in all_game_resource_kinds().into_iter().enumerate() {
            let metadata = GameFileMetadata {
                id: GameFileId::Physical {
                    relative_path: format!("files/{index}.bin").into(),
                },
                display_path: format!("files/{index}.bin"),
                physical_relative_path: format!("files/{index}.bin").into(),
                kind,
                size_bytes: 32,
                modified_unix_nanos: None,
                archive_entry: None,
            };
            let capabilities = raw_content_item(&metadata).capabilities;
            assert!(capabilities.inspect);
            assert!(!capabilities.open);
            assert!(!capabilities.place);
            assert!(!capabilities.apply);
            assert!(!capabilities.assign);
            assert!(!capabilities.editable);
        }
    }

    #[test]
    fn resource_kind_collection_keys_round_trip() {
        for kind in all_game_resource_kinds() {
            assert_eq!(
                game_resource_kind_from_key(game_resource_kind_key(kind)),
                Some(kind)
            );
        }
    }

    #[test]
    fn retail_file_capabilities_are_always_read_only() {
        let metadata = GameFileMetadata {
            id: GameFileId::Physical {
                relative_path: "files/test.bti".into(),
            },
            display_path: "files/test.bti".to_string(),
            physical_relative_path: "files/test.bti".into(),
            kind: GameResourceKind::Texture,
            size_bytes: 32,
            modified_unix_nanos: None,
            archive_entry: None,
        };
        let item = raw_content_item(&metadata);
        assert!(item.capabilities.preview);
        assert!(item.capabilities.inspect);
        assert!(!item.capabilities.open);
        assert!(!item.capabilities.place);
        assert!(!item.capabilities.apply);
        assert!(!item.capabilities.assign);
        assert!(!item.capabilities.editable);
    }

    #[test]
    fn game_file_paths_form_physical_and_archive_folders() {
        let physical_archive = GameFileMetadata {
            id: GameFileId::Physical {
                relative_path: "files/Archives/scene.szs".into(),
            },
            display_path: "files/Archives/scene.szs".to_string(),
            physical_relative_path: "files/Archives/scene.szs".into(),
            kind: GameResourceKind::Archive,
            size_bytes: 64,
            modified_unix_nanos: None,
            archive_entry: None,
        };
        assert_eq!(
            game_file_virtual_components(&physical_archive),
            ["files", "Archives", "scene.szs!"]
        );

        let archive_child = GameFileMetadata {
            id: GameFileId::ArchiveEntry {
                archive_relative_path: "files/Archives/scene.szs".into(),
                raw_entry_path: b"map/map/sky.bmd".to_vec(),
            },
            display_path: "files/Archives/scene.szs!/map/map/sky.bmd".to_string(),
            physical_relative_path: "files/Archives/scene.szs".into(),
            kind: GameResourceKind::Model,
            size_bytes: 32,
            modified_unix_nanos: None,
            archive_entry: Some(sms_formats::GameArchiveEntryMetadata {
                raw_path: b"map/map/sky.bmd".to_vec(),
                display_path: "map/map/sky.bmd".to_string(),
                flags: 0x11,
                uncompressed_size: 32,
            }),
        };
        assert_eq!(
            game_file_virtual_components(&archive_child),
            ["files", "Archives", "scene.szs!", "map", "map", "sky.bmd"]
        );
    }

    #[test]
    fn game_file_history_round_trips_back_and_forward() {
        let mut app = SmsEditorApp::default();
        app.content_browser.node = ContentNode::GameFiles;
        app.navigate_game_files_to(vec!["files".to_string()]);
        app.navigate_game_files_to(vec!["files".to_string(), "Archives".to_string()]);
        app.navigate_content_back();
        assert_eq!(app.content_browser.game_file_path, ["files"]);
        app.navigate_content_forward();
        assert_eq!(app.content_browser.game_file_path, ["files", "Archives"]);
    }

    #[test]
    fn collection_history_round_trips_left_navigation() {
        let mut app = SmsEditorApp::default();
        app.select_content_node(ContentNode::GameSkyboxes);
        app.select_content_node(ContentNode::GameSounds);
        app.navigate_content_back();
        assert_eq!(app.content_browser.node, ContentNode::GameSkyboxes);
        app.navigate_content_forward();
        assert_eq!(app.content_browser.node, ContentNode::GameSounds);
    }

    #[test]
    fn game_file_directory_results_are_grouped_and_cached() {
        let mut app = SmsEditorApp::default();
        app.content_browser.node = ContentNode::GameFiles;
        app.game_content_index.revision = 4;
        app.game_content_index.entries = (0..512)
            .map(|index| {
                let path = format!("files/Archives/group{}/file{index}.bti", index % 8);
                GameFileMetadata {
                    id: GameFileId::Physical {
                        relative_path: path.clone().into(),
                    },
                    display_path: path.clone(),
                    physical_relative_path: path.into(),
                    kind: GameResourceKind::Texture,
                    size_bytes: 32,
                    modified_unix_nanos: None,
                    archive_entry: None,
                }
            })
            .collect();

        let root = app.game_file_directory_entries();
        assert_eq!(
            root.as_ref(),
            &[GameFileDirectoryEntry::Folder(vec!["files".to_string()])]
        );
        let cached = app.game_file_directory_entries();
        assert!(Arc::ptr_eq(&root, &cached));

        app.navigate_game_files_to(vec!["files".to_string(), "Archives".to_string()]);
        let archives = app.game_file_directory_entries();
        assert_eq!(archives.len(), 8);
        assert!(archives
            .iter()
            .all(|entry| matches!(entry, GameFileDirectoryEntry::Folder(_))));
    }

    #[test]
    fn toolbar_row_does_not_claim_the_browser_body_height() {
        egui::__run_test_ui(|ui| {
            let available_height = ui.available_height();
            assert!(available_height > 100.0);

            let toolbar = content_browser_toolbar_row(ui, |ui| {
                ui.label("Content Browser");
                ui.separator();
                ui.button("Back")
            });

            assert!(toolbar.response.rect.height() < available_height * 0.25);
        });
    }
}
