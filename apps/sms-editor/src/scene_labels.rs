use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sms_formats::{
    mount_scene_archive, parse_jdrama_scenario_archive_entries, read_stage_asset_bytes, BmgFile,
    BmgMessageToken, JDramaScenarioArchiveEntry, SceneArchiveInfo,
};
use sms_schema::{extract_stage_name_tables, StageNameTables};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SceneArchiveLabel {
    pub(super) stage_name: Option<String>,
    pub(super) scenario_names: Vec<String>,
}

pub(super) fn load_scene_archive_labels(
    base_root: &Path,
    repo_root: &Path,
    archives: &[SceneArchiveInfo],
) -> Result<BTreeMap<String, SceneArchiveLabel>, String> {
    let data_dir = scene_data_directory(base_root, archives)
        .ok_or_else(|| "could not locate the extracted game's data directory".to_string())?;
    let stage_arc = find_case_insensitive_file(&data_dir, &["stageArc.bin"])
        .ok_or_else(|| format!("{} has no stageArc.bin", data_dir.display()))?;
    let common_archive = find_case_insensitive_file(&data_dir, &["common.szs", "common.arc"])
        .ok_or_else(|| format!("{} has no common.szs or common.arc", data_dir.display()))?;

    let stage_entries = parse_jdrama_scenario_archive_entries(
        &fs::read(&stage_arc).map_err(|error| format!("read {}: {error}", stage_arc.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", stage_arc.display()))?;
    let stage_names = load_message_strings(&common_archive, "stagename.bmg")?;
    let scenario_names = load_message_strings(&common_archive, "scenarioname.bmg")?;
    let tables = extract_stage_name_tables(repo_root).map_err(|error| error.to_string())?;

    let mut labels =
        associate_scene_archive_labels(&stage_entries, &tables, &stage_names, &scenario_names);
    let archive_ids = archives
        .iter()
        .map(|archive| archive.stage_id.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    labels.retain(|stage_id, _| archive_ids.contains(stage_id));
    Ok(labels)
}

fn scene_data_directory(base_root: &Path, archives: &[SceneArchiveInfo]) -> Option<PathBuf> {
    archives
        .first()
        .and_then(|archive| archive.path.parent())
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| {
            [
                base_root.join("files/data"),
                base_root.join("data"),
                base_root.to_path_buf(),
            ]
            .into_iter()
            .find(|path| path.is_dir())
        })
}

fn find_case_insensitive_file(directory: &Path, candidates: &[&str]) -> Option<PathBuf> {
    fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            entry.path().is_file()
                && candidates
                    .iter()
                    .any(|candidate| name == candidate.to_ascii_lowercase())
        })
        .map(|entry| entry.path())
}

fn load_message_strings(archive: &Path, file_name: &str) -> Result<Vec<String>, String> {
    let asset = mount_scene_archive(archive)
        .map_err(|error| format!("mount {}: {error}", archive.display()))?
        .into_iter()
        .find(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .ends_with(&format!("/{file_name}"))
        })
        .ok_or_else(|| format!("{} has no {file_name}", archive.display()))?;
    let bytes = read_stage_asset_bytes(&asset.path)
        .map_err(|error| format!("read {}: {error}", asset.path.display()))?;
    let messages = BmgFile::parse(bytes)
        .map_err(|error| format!("parse {}: {error}", asset.path.display()))?;
    Ok(messages
        .entries
        .iter()
        .map(|entry| {
            entry
                .message
                .tokens
                .iter()
                .filter_map(|token| match token {
                    BmgMessageToken::Text(text) => Some(text.as_str()),
                    BmgMessageToken::Control(_) => None,
                })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect())
}

trait StageNameLookup {
    fn stage_name_index(&self, area_index: u32) -> Option<usize>;
    fn normal_scenario_name_index(&self, area_index: u32, scenario_index: u32) -> Option<usize>;
    fn normal_scenario_count(&self, area_index: u32) -> Option<usize>;
    fn ex_scenario_name_index(&self, area_index: u32) -> Option<usize>;
}

impl StageNameLookup for StageNameTables {
    fn stage_name_index(&self, area_index: u32) -> Option<usize> {
        StageNameTables::stage_name_index(self, area_index)
    }

    fn normal_scenario_name_index(&self, area_index: u32, scenario_index: u32) -> Option<usize> {
        StageNameTables::normal_scenario_name_index(self, area_index, scenario_index)
    }

    fn normal_scenario_count(&self, area_index: u32) -> Option<usize> {
        StageNameTables::normal_scenario_count(self, area_index)
    }

    fn ex_scenario_name_index(&self, area_index: u32) -> Option<usize> {
        StageNameTables::ex_scenario_name_index(self, area_index)
    }
}

fn associate_scene_archive_labels(
    entries: &[JDramaScenarioArchiveEntry],
    tables: &impl StageNameLookup,
    stage_names: &[String],
    scenario_names: &[String],
) -> BTreeMap<String, SceneArchiveLabel> {
    let mut area_entry_counts = BTreeMap::<u32, usize>::new();
    for entry in entries {
        *area_entry_counts.entry(entry.area_index).or_default() += 1;
    }

    let mut labels = BTreeMap::<String, SceneArchiveLabel>::new();
    for entry in entries {
        let Some(stage_id) = Path::new(&entry.archive_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        let label = labels.entry(stage_id).or_default();
        if label.stage_name.is_none() {
            label.stage_name = tables
                .stage_name_index(entry.area_index)
                .and_then(|index| stage_names.get(index))
                .filter(|name| !name.is_empty())
                .cloned();
        }

        let normal_scenarios_match_archive_table = tables
            .normal_scenario_count(entry.area_index)
            .is_some_and(|count| {
                count > 1 && area_entry_counts.get(&entry.area_index) == Some(&count)
            });
        let scenario_name_index = tables.ex_scenario_name_index(entry.area_index).or_else(|| {
            normal_scenarios_match_archive_table.then(|| {
                tables.normal_scenario_name_index(entry.area_index, entry.scenario_index)
            })?
        });
        if let Some(name) = scenario_name_index
            .and_then(|index| scenario_names.get(index))
            .filter(|name| !name.is_empty())
        {
            if !label.scenario_names.contains(name) {
                label.scenario_names.push(name.clone());
            }
        }
    }
    labels.retain(|_, label| label.stage_name.is_some() || !label.scenario_names.is_empty());
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lookup;

    impl StageNameLookup for Lookup {
        fn stage_name_index(&self, area_index: u32) -> Option<usize> {
            (area_index == 2).then_some(0)
        }

        fn normal_scenario_name_index(
            &self,
            area_index: u32,
            scenario_index: u32,
        ) -> Option<usize> {
            (area_index == 2).then_some(usize::try_from(scenario_index).ok()?)
        }

        fn normal_scenario_count(&self, area_index: u32) -> Option<usize> {
            (area_index == 2).then_some(2)
        }

        fn ex_scenario_name_index(&self, _area_index: u32) -> Option<usize> {
            None
        }
    }

    #[test]
    fn archive_labels_follow_stage_arc_order_and_merge_shared_archives() {
        let entries = vec![
            JDramaScenarioArchiveEntry {
                area_index: 2,
                scenario_index: 0,
                archive_name: "bianco0.arc".to_string(),
            },
            JDramaScenarioArchiveEntry {
                area_index: 2,
                scenario_index: 1,
                archive_name: "bianco0.arc".to_string(),
            },
        ];
        let labels = associate_scene_archive_labels(
            &entries,
            &Lookup,
            &["BIANCO HILLS".to_string()],
            &["Episode One".to_string(), "Episode Two".to_string()],
        );

        assert_eq!(
            labels["bianco0"].stage_name.as_deref(),
            Some("BIANCO HILLS")
        );
        assert_eq!(
            labels["bianco0"].scenario_names,
            ["Episode One", "Episode Two"]
        );
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with an extracted game and the neighboring SMS decomp"]
    fn loads_localized_names_from_an_extracted_game() {
        let base_root = PathBuf::from(std::env::var("SMS_BASE_ROOT").expect("SMS_BASE_ROOT"));
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let archives = sms_formats::discover_scene_archives(&base_root).unwrap();
        let labels = load_scene_archive_labels(&base_root, &repo_root, &archives).unwrap();

        let bianco = &labels["bianco0"];
        assert!(bianco
            .stage_name
            .as_ref()
            .is_some_and(|name| !name.is_empty()));
        assert!(!bianco.scenario_names.is_empty());
    }
}
