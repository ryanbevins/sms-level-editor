use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use sms_formats::{mount_scene_archive, SceneArchiveInfo, StageAsset, StageAssetKind};
use sms_scene::{BlankStageSkyboxPreset, SourceFreeStageArchive};

use super::*;
use crate::document_commands::ensure_sky_placement;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetailSkyboxEntry {
    pub(super) stage_id: String,
    pub(super) archive_path: PathBuf,
    pub(super) resource_count: usize,
}

pub(super) struct RetailSkyboxSelection {
    pub(super) base_root: String,
    pub(super) target_stage_id: String,
    pub(super) source_stage_id: String,
    pub(super) preset: BlankStageSkyboxPreset,
}

pub(super) fn index_retail_skyboxes(
    archives: &[SceneArchiveInfo],
) -> (Vec<RetailSkyboxEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for archive in archives.iter().filter(|archive| archive.path.is_file()) {
        let assets = match mount_scene_archive(&archive.path) {
            Ok(assets) => assets,
            Err(error) => {
                warnings.push(format!(
                    "Could not index skybox resources in '{}': {error}",
                    archive.path.display()
                ));
                continue;
            }
        };
        let resource_count = assets
            .iter()
            .filter_map(|asset| archive_resource_path(&asset.path))
            .filter(|path| is_skybox_resource_path(path))
            .count();
        let has_model = assets
            .iter()
            .filter_map(|asset| archive_resource_path(&asset.path))
            .any(|path| path.eq_ignore_ascii_case("map/map/sky.bmd"));
        if has_model {
            entries.push(RetailSkyboxEntry {
                stage_id: archive.stage_id.clone(),
                archive_path: archive.path.clone(),
                resource_count,
            });
        }
    }
    entries.sort_by(|left, right| {
        left.stage_id
            .cmp(&right.stage_id)
            .then_with(|| left.archive_path.cmp(&right.archive_path))
    });
    (entries, warnings)
}

fn archive_resource_path(path: &Path) -> Option<String> {
    path.to_string_lossy()
        .split_once("!/")
        .map(|(_, resource)| resource.replace('\\', "/"))
}

fn is_skybox_resource_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let Some((directory, file_name)) = normalized.rsplit_once('/') else {
        return false;
    };
    directory.eq_ignore_ascii_case("map/map")
        && file_name.split_once('.').is_some_and(|(stem, extension)| {
            !extension.is_empty()
                && (stem.eq_ignore_ascii_case("sky") || stem.eq_ignore_ascii_case("reflectsky"))
        })
}

fn skybox_asset_kind(raw_path: &[u8]) -> StageAssetKind {
    match Path::new(String::from_utf8_lossy(raw_path).as_ref())
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "bmd" | "bdl" => StageAssetKind::Model,
        "bmt" => StageAssetKind::MaterialTable,
        "bck" | "btp" | "btk" | "brk" | "bas" => StageAssetKind::Animation,
        _ => StageAssetKind::Other,
    }
}

impl SmsEditorApp {
    pub(super) fn request_retail_skybox(&mut self, entry: RetailSkyboxEntry) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        let Some(document) = self.document.as_ref() else {
            self.log
                .push("Open a stage before choosing a game skybox.".to_string());
            return;
        };
        if self.model_instances.iter().any(|instance| {
            instance.stage_id.eq_ignore_ascii_case(&document.stage_id)
                && instance.placement.export_mode == sms_authoring::ModelInstanceExportMode::Skybox
        }) {
            self.log.push(
                "The current stage already has an authored model assigned as Stage skybox. Change that model's Stage export role before selecting a game skybox."
                    .to_string(),
            );
            return;
        }
        let base_root = self.base_root.trim().to_string();
        let target_stage_id = document.stage_id.clone();
        let source_stage_id = entry.stage_id.clone();
        let archive_path = entry.archive_path.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> Result<RetailSkyboxSelection, String> {
                let bytes = fs::read(&archive_path).map_err(|error| {
                    format!(
                        "could not read retail stage archive '{}': {error}",
                        archive_path.display()
                    )
                })?;
                let archive = SourceFreeStageArchive::parse(&bytes).map_err(|error| {
                    format!(
                        "could not import retail stage archive '{}': {error}",
                        archive_path.display()
                    )
                })?;
                let preset = BlankStageSkyboxPreset::from_archive(&archive).map_err(|error| {
                    format!("could not index {source_stage_id} skybox: {error}")
                })?;
                Ok(RetailSkyboxSelection {
                    base_root,
                    target_stage_id,
                    source_stage_id,
                    preset,
                })
            })();
            let _ = sender.send(BackgroundResult::RetailSkybox(result));
        });
        self.background_receiver = Some(receiver);
        self.background_label = Some(format!("Loading {} skybox", entry.stage_id));
        self.log.push(format!(
            "Loading complete skybox bundle from retail stage '{}'...",
            entry.stage_id
        ));
    }

    pub(super) fn apply_retail_skybox(&mut self, selection: RetailSkyboxSelection) {
        if self.base_root.trim() != selection.base_root
            || self.stage_id != selection.target_stage_id
        {
            self.log.push(format!(
                "Discarded retail skybox '{}' because the open stage changed.",
                selection.source_stage_id
            ));
            return;
        }
        let Some(current) = self.document.as_ref() else {
            return;
        };
        let mut document = current.clone();
        let Some(archive) = document.stage_archive.as_ref() else {
            self.log
                .push("The open stage has no detached semantic archive.".to_string());
            return;
        };
        let has_sky_actor = document
            .objects
            .iter()
            .any(|object| object.factory_name == "Sky");

        let selected_paths = selection
            .preset
            .resources()
            .iter()
            .map(|resource| resource.raw_path.clone())
            .collect::<BTreeSet<_>>();
        let mut existing_paths = archive
            .resources()
            .iter()
            .filter(|resource| {
                is_skybox_resource_path(&String::from_utf8_lossy(&resource.raw_path))
            })
            .map(|resource| resource.raw_path.clone())
            .collect::<BTreeSet<_>>();
        existing_paths.extend(
            document
                .archive_edits
                .resources
                .iter()
                .filter(|resource| {
                    is_skybox_resource_path(&String::from_utf8_lossy(&resource.raw_resource_path))
                })
                .map(|resource| resource.raw_resource_path.clone()),
        );
        for stale_path in existing_paths.difference(&selected_paths) {
            document.archive_edits.remove_resource(stale_path.clone());
        }
        for resource in selection.preset.resources() {
            document
                .archive_edits
                .upsert_resource(resource.raw_path.clone(), resource.document.clone());
        }

        let source_path = document
            .stage_archive_source_path
            .clone()
            .expect("a detached stage archive has a source identity");
        document.assets.retain(|asset| {
            archive_resource_path(&asset.path)
                .filter(|path| is_skybox_resource_path(path))
                .is_none_or(|path| {
                    selected_paths
                        .iter()
                        .any(|selected| selected.eq_ignore_ascii_case(path.as_bytes()))
                })
        });
        for resource in selection.preset.resources() {
            let path = PathBuf::from(format!(
                "{}!/{}",
                source_path.display(),
                String::from_utf8_lossy(&resource.raw_path)
            ));
            if !document.assets.iter().any(|asset| asset.path == path) {
                document.assets.push(StageAsset {
                    path,
                    kind: skybox_asset_kind(&resource.raw_path),
                });
            }
        }
        document
            .assets
            .sort_by(|left, right| left.path.cmp(&right.path));

        if !has_sky_actor {
            let address = match ensure_sky_placement(&mut document, [0.0; 3]) {
                Ok(address) => address,
                Err(error) => {
                    self.log
                        .push(format!("Could not author the typed Sky actor: {error}"));
                    return;
                }
            };
            let mut sky =
                SceneObject::new(format!("{}-sky", sanitize_id(&document.stage_id)), "Sky");
            sky.class_name = Some("TSky".to_string());
            sky.placement = Some(sms_scene::PlacementBinding::Existing(address));
            document.objects.push(sky);
        }
        if let Err(error) = document.queue_editor_overlay_change() {
            self.log
                .push(format!("Could not persist the retail skybox edit: {error}"));
            return;
        }

        self.document = Some(document);
        self.document_dirty = true;
        self.render_scene = self.document.as_ref().map(RenderScene::from_document);
        self.rebuild_model_preview_from_document();
        self.selected_object_id = self
            .document
            .as_ref()
            .and_then(|document| {
                document
                    .objects
                    .iter()
                    .find(|object| object.factory_name == "Sky")
            })
            .map(|object| object.id.clone());
        self.log.push(format!(
            "Applied retail skybox '{}' with {} typed resource(s) to stage '{}'.",
            selection.source_stage_id,
            selection.preset.resources().len(),
            selection.target_stage_id
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skybox_resource_index_is_exact_to_map_map_sky_stem() {
        assert!(is_skybox_resource_path("map/map/sky.bmd"));
        assert!(is_skybox_resource_path("MAP/MAP/SKY.BTK"));
        assert!(is_skybox_resource_path("map/map/reflectsky.bmd"));
        assert!(is_skybox_resource_path("MAP/MAP/REFLECTSKY.BTK"));
        assert!(!is_skybox_resource_path("mapobj/sky.bmd"));
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn retail_index_discovers_complete_skybox_bundles() {
        let base_root = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let archives = sms_formats::discover_scene_archives(base_root).unwrap();
        let (skyboxes, warnings) = index_retail_skyboxes(&archives);
        assert!(warnings.is_empty(), "{warnings:#?}");
        assert!(!skyboxes.is_empty());
        assert!(skyboxes.iter().all(|entry| entry.resource_count >= 1));
        assert!(skyboxes
            .iter()
            .any(|entry| entry.stage_id.eq_ignore_ascii_case("dolpic0")));
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn retail_bundle_applies_to_an_authored_scene_and_rebuilds() {
        let base_root = PathBuf::from(std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT"));
        let archives = sms_formats::discover_scene_archives(&base_root).unwrap();
        let source = archives
            .iter()
            .find(|archive| archive.stage_id.eq_ignore_ascii_case("dolpic0"))
            .unwrap();
        let source_archive =
            SourceFreeStageArchive::parse(&fs::read(&source.path).unwrap()).unwrap();
        let preset = BlankStageSkyboxPreset::from_archive(&source_archive).unwrap();

        let bootstrap = sms_scene::BlankStageBootstrapManifest::from_authored_bytes(
            sms_scene::BLANK_STAGE_BOOTSTRAP_REQUIREMENTS
                .map(|requirement| {
                    let proxy = sms_authoring::built_in_blank_stage_proxy(requirement.raw_path);
                    let bytes = match requirement.kind {
                        sms_scene::BlankStageBootstrapKind::Model => proxy.compile_bmd().unwrap(),
                        sms_scene::BlankStageBootstrapKind::Collision => {
                            proxy.collision.as_ref().unwrap().to_col_bytes().unwrap()
                        }
                    };
                    sms_scene::BlankStageBootstrapResource {
                        raw_path: requirement.raw_path.to_vec(),
                        bytes,
                    }
                })
                .to_vec(),
        )
        .unwrap();
        let archive = sms_scene::BlankStagePreset {
            target_slot: "custom_sky_test".to_string(),
            ..sms_scene::BlankStagePreset::default()
        }
        .build(bootstrap)
        .unwrap();
        let document =
            StageDocument::from_authored_archive(&base_root, "custom_sky_test", archive).unwrap();
        let mut app = SmsEditorApp {
            base_root: base_root.to_string_lossy().into_owned(),
            stage_id: "custom_sky_test".to_string(),
            document: Some(document),
            ..SmsEditorApp::default()
        };
        app.apply_retail_skybox(RetailSkyboxSelection {
            base_root: base_root.to_string_lossy().into_owned(),
            target_stage_id: "custom_sky_test".to_string(),
            source_stage_id: "dolpic0".to_string(),
            preset: preset.clone(),
        });

        let document = app.document.as_ref().unwrap();
        assert!(document
            .objects
            .iter()
            .any(|object| object.factory_name == "Sky"));
        let rebuilt = document
            .build_stage_archive_with_edits(&document.archive_edits)
            .unwrap();
        let reopened = SourceFreeStageArchive::parse(&rebuilt).unwrap();
        let reopened = BlankStageSkyboxPreset::from_archive(&reopened).unwrap();
        assert_eq!(
            reopened
                .resources()
                .iter()
                .map(|resource| resource.raw_path.as_slice())
                .collect::<Vec<_>>(),
            preset
                .resources()
                .iter()
                .map(|resource| resource.raw_path.as_slice())
                .collect::<Vec<_>>()
        );
        for (actual, expected) in reopened.resources().iter().zip(preset.resources()) {
            assert_eq!(
                actual.document.to_bytes().unwrap(),
                expected.document.to_bytes().unwrap()
            );
        }
    }
}
