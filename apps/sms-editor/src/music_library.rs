use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use regex::Regex;
use sms_formats::parse_jdrama_scenario_archive_entries;

use crate::project::ProjectStageMusic;
use crate::{SceneArchiveLabel, SmsEditorApp};

const BGM_BASE: u32 = 0x8001_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetailMusicEntry {
    pub(super) bgm_id: u32,
    pub(super) wave_scene_id: u32,
    pub(super) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetailSoundEntry {
    pub(super) sound_id: u32,
    pub(super) symbol: String,
    pub(super) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetailStageAudioProfile {
    pub(super) stage_id: String,
    pub(super) primary_bgm_id: Option<u32>,
    pub(super) wave_scene_id: Option<u32>,
    pub(super) secondary_bgm_id: Option<u32>,
    pub(super) fade_event: u8,
}

pub(super) fn index_retail_music(
    repo_root: &Path,
    base_root: &Path,
    labels: &BTreeMap<String, SceneArchiveLabel>,
) -> Result<Vec<RetailMusicEntry>, String> {
    let bgm_source_path = repo_root.join("src/MSound/MSoundBGM.cpp");
    let stage_source_path = repo_root.join("src/System/MSoundMainSide.cpp");
    let bgm_source = fs::read_to_string(&bgm_source_path)
        .map_err(|error| format!("read {}: {error}", bgm_source_path.display()))?;
    let stage_source = fs::read_to_string(&stage_source_path)
        .map_err(|error| format!("read {}: {error}", stage_source_path.display()))?;
    let scene_by_bgm = extract_bgm_wave_scenes(&bgm_source)?;
    let areas_by_bgm = extract_primary_stage_music(&stage_source)?;
    let archive_by_area = load_archive_stems_by_area(base_root).unwrap_or_default();
    let names_by_bgm = load_retail_bgm_names(base_root).unwrap_or_default();

    let mut entries = Vec::new();
    for (bgm_id, wave_scene_id) in scene_by_bgm {
        if bgm_id & 0xffff_0000 != BGM_BASE || wave_scene_id == u32::MAX {
            continue;
        }
        let mut sources = areas_by_bgm
            .get(&bgm_id)
            .into_iter()
            .flatten()
            .filter_map(|area| archive_by_area.get(area))
            .filter_map(|stage_id| {
                labels
                    .get(stage_id)
                    .and_then(|label| label.stage_name.as_deref())
                    .or(Some(stage_id.as_str()))
            })
            .map(str::to_string)
            .collect::<Vec<_>>();
        sources.sort();
        sources.dedup();
        let label = names_by_bgm
            .get(&bgm_id)
            .map(|name| friendly_bgm_name(name))
            .or_else(|| (!sources.is_empty()).then(|| sources.join(" / ")))
            .unwrap_or_else(|| format!("BGM 0x{bgm_id:08X}"));
        entries.push(RetailMusicEntry {
            bgm_id,
            wave_scene_id,
            label,
        });
    }
    entries.sort_by(|left, right| left.label.cmp(&right.label));
    if entries.is_empty() {
        return Err("the decomp did not expose any valid BGM-to-wave-scene mappings".to_string());
    }
    Ok(entries)
}

fn extract_bgm_wave_scenes(source: &str) -> Result<BTreeMap<u32, u32>, String> {
    let mapping = Regex::new(r"(?s)case\s+(0x[0-9A-Fa-f]+)\s*:\s*return\s+(0x[0-9A-Fa-f]+)\s*;")
        .expect("static BGM mapping regex is valid");
    let mut result = BTreeMap::new();
    for captures in mapping.captures_iter(source) {
        let bgm_id = parse_hex(&captures[1])?;
        let wave_scene_id = parse_hex(&captures[2])?;
        if result.insert(bgm_id, wave_scene_id).is_some() {
            return Err(format!(
                "decomp contains duplicate BGM mapping 0x{bgm_id:08X}"
            ));
        }
    }
    Ok(result)
}

fn extract_primary_stage_music(source: &str) -> Result<BTreeMap<u32, BTreeSet<u32>>, String> {
    let map_case =
        Regex::new(r"^\s*case\s+(\d+)\s*:").expect("static stage-map case regex is valid");
    let assignment = Regex::new(r"MSStageInfo::stageBgm\s*=\s*base\s*\+\s*(0x[0-9A-Fa-f]+)")
        .expect("static stage BGM assignment regex is valid");
    let switch_start = source
        .find("switch (map)")
        .ok_or_else(|| "decomp MSound stage setup has no switch (map)".to_string())?;
    let body = &source[switch_start..];
    let mut depth = 0_i32;
    let mut entered = false;
    let mut current_areas = Vec::new();
    let mut current_has_music = false;
    let mut result = BTreeMap::<u32, BTreeSet<u32>>::new();
    for line in body.lines() {
        if entered && depth == 1 {
            if let Some(captures) = map_case.captures(line) {
                if current_has_music {
                    current_areas.clear();
                    current_has_music = false;
                }
                current_areas.push(
                    captures[1]
                        .parse::<u32>()
                        .map_err(|error| format!("parse map case {}: {error}", &captures[1]))?,
                );
            }
        }
        if entered && !current_has_music {
            if let Some(captures) = assignment.captures(line) {
                let bgm_id = BGM_BASE
                    .checked_add(parse_hex(&captures[1])?)
                    .ok_or_else(|| "decomp BGM identifier overflows u32".to_string())?;
                result
                    .entry(bgm_id)
                    .or_default()
                    .extend(current_areas.iter().copied());
                current_has_music = true;
            }
        }
        let opens = line.bytes().filter(|byte| *byte == b'{').count() as i32;
        let closes = line.bytes().filter(|byte| *byte == b'}').count() as i32;
        if !entered && opens > 0 {
            entered = true;
        }
        if entered {
            depth += opens - closes;
            if depth <= 0 {
                break;
            }
        }
    }
    Ok(result)
}

fn load_archive_stems_by_area(base_root: &Path) -> Result<BTreeMap<u32, String>, String> {
    let candidates = [
        base_root.join("files/data/stageArc.bin"),
        base_root.join("data/stageArc.bin"),
        base_root.join("stageArc.bin"),
    ];
    let path = candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "could not locate the extracted stageArc.bin".to_string())?;
    let entries = parse_jdrama_scenario_archive_entries(
        &fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let mut result = BTreeMap::new();
    for entry in entries {
        let Some(stem) = Path::new(&entry.archive_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
        else {
            continue;
        };
        result
            .entry(entry.area_index)
            .or_insert_with(|| stem.to_ascii_lowercase());
    }
    Ok(result)
}

fn load_retail_bgm_names(base_root: &Path) -> Result<BTreeMap<u32, String>, String> {
    let bytes = load_retail_sound_assignment_bytes(base_root)?;
    extract_retail_bgm_names(&String::from_utf8_lossy(&bytes))
}

pub(super) fn index_retail_sounds(base_root: &Path) -> Result<Vec<RetailSoundEntry>, String> {
    let bytes = load_retail_sound_assignment_bytes(base_root)?;
    extract_retail_sound_entries(&bytes)
}

pub(super) fn index_retail_stage_audio_profiles(
    repo_root: &Path,
    base_root: &Path,
) -> Result<Vec<RetailStageAudioProfile>, String> {
    let bgm_source_path = repo_root.join("src/MSound/MSoundBGM.cpp");
    let stage_source_path = repo_root.join("src/System/MSoundMainSide.cpp");
    let scene_by_bgm = extract_bgm_wave_scenes(
        &fs::read_to_string(&bgm_source_path)
            .map_err(|error| format!("read {}: {error}", bgm_source_path.display()))?,
    )?;
    let stage_source = fs::read_to_string(&stage_source_path)
        .map_err(|error| format!("read {}: {error}", stage_source_path.display()))?;
    let entries = load_stage_archive_entries(base_root)?;
    let mut profiles = Vec::new();
    for entry in entries {
        let Some(stage_id) = Path::new(&entry.archive_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
        else {
            continue;
        };
        let Some(state) =
            evaluate_stage_audio_source(&stage_source, entry.area_index, entry.scenario_index)
        else {
            continue;
        };
        profiles.push(RetailStageAudioProfile {
            stage_id: stage_id.to_ascii_lowercase(),
            primary_bgm_id: state.primary_bgm_id,
            wave_scene_id: state
                .primary_bgm_id
                .and_then(|bgm_id| scene_by_bgm.get(&bgm_id).copied()),
            secondary_bgm_id: state.secondary_bgm_id,
            fade_event: state.fade_event,
        });
    }
    profiles.sort_by(|left, right| left.stage_id.cmp(&right.stage_id));
    profiles.dedup_by(|left, right| left.stage_id == right.stage_id);
    Ok(profiles)
}

fn load_stage_archive_entries(
    base_root: &Path,
) -> Result<Vec<sms_formats::JDramaScenarioArchiveEntry>, String> {
    let candidates = [
        base_root.join("files/data/stageArc.bin"),
        base_root.join("data/stageArc.bin"),
        base_root.join("stageArc.bin"),
    ];
    let path = candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "could not locate the extracted stageArc.bin".to_string())?;
    parse_jdrama_scenario_archive_entries(
        &fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", path.display()))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EvaluatedStageAudio {
    primary_bgm_id: Option<u32>,
    secondary_bgm_id: Option<u32>,
    fade_event: u8,
}

fn evaluate_stage_audio_source(source: &str, map: u32, area: u32) -> Option<EvaluatedStageAudio> {
    let function = source.find("void MSMainProc::setMSoundEnterStage")?;
    let open = source[function..].find('{')? + function;
    let close = matching_delimiter(source, open, b'{', b'}')?;
    let mut state = EvaluatedStageAudio::default();
    evaluate_audio_block(&source[open + 1..close], map, area, &mut state);
    Some(state)
}

fn evaluate_audio_block(block: &str, map: u32, area: u32, state: &mut EvaluatedStageAudio) -> bool {
    let mut cursor = 0;
    while cursor < block.len() {
        cursor = skip_cpp_space_and_comments(block, cursor);
        if cursor >= block.len() {
            break;
        }
        if keyword_at(block, cursor, "break") {
            return true;
        }
        if keyword_at(block, cursor, "case") || keyword_at(block, cursor, "default") {
            cursor = block[cursor..]
                .find(':')
                .map_or(block.len(), |offset| cursor + offset + 1);
            continue;
        }
        if keyword_at(block, cursor, "switch") {
            let Some(paren) = block[cursor..].find('(').map(|offset| cursor + offset) else {
                break;
            };
            let Some(paren_end) = matching_delimiter(block, paren, b'(', b')') else {
                break;
            };
            let selector = block[paren + 1..paren_end].trim();
            let body_start = skip_cpp_space_and_comments(block, paren_end + 1);
            if block.as_bytes().get(body_start) != Some(&b'{') {
                cursor = paren_end + 1;
                continue;
            }
            let Some(body_end) = matching_delimiter(block, body_start, b'{', b'}') else {
                break;
            };
            let value = match selector {
                "map" => Some(map),
                "area" => Some(area),
                _ => None,
            };
            if let Some(value) = value {
                if let Some(case_start) = switch_case_start(&block[body_start + 1..body_end], value)
                {
                    evaluate_audio_block(
                        &block[body_start + 1 + case_start..body_end],
                        map,
                        area,
                        state,
                    );
                }
            }
            cursor = body_end + 1;
            continue;
        }
        if keyword_at(block, cursor, "if") {
            let Some(paren) = block[cursor..].find('(').map(|offset| cursor + offset) else {
                break;
            };
            let Some(paren_end) = matching_delimiter(block, paren, b'(', b')') else {
                break;
            };
            let mut condition = evaluate_area_condition(&block[paren + 1..paren_end], area);
            let mut branch_start = skip_cpp_space_and_comments(block, paren_end + 1);
            let Some((branch, mut after_branch)) = cpp_statement(block, branch_start) else {
                break;
            };
            let mut selected = false;
            if condition == Some(true) {
                evaluate_audio_block(branch, map, area, state);
                selected = true;
            }
            loop {
                let else_at = skip_cpp_space_and_comments(block, after_branch);
                if !keyword_at(block, else_at, "else") {
                    cursor = after_branch;
                    break;
                }
                branch_start = skip_cpp_space_and_comments(block, else_at + 4);
                if keyword_at(block, branch_start, "if") {
                    let Some(next_paren) = block[branch_start..]
                        .find('(')
                        .map(|offset| branch_start + offset)
                    else {
                        cursor = branch_start + 2;
                        break;
                    };
                    let Some(next_paren_end) = matching_delimiter(block, next_paren, b'(', b')')
                    else {
                        cursor = next_paren + 1;
                        break;
                    };
                    condition =
                        evaluate_area_condition(&block[next_paren + 1..next_paren_end], area);
                    branch_start = skip_cpp_space_and_comments(block, next_paren_end + 1);
                } else {
                    condition = Some(true);
                }
                let Some((next_branch, next_after)) = cpp_statement(block, branch_start) else {
                    cursor = branch_start;
                    break;
                };
                if !selected && condition == Some(true) {
                    evaluate_audio_block(next_branch, map, area, state);
                    selected = true;
                }
                after_branch = next_after;
            }
            continue;
        }
        let end = block[cursor..]
            .find(';')
            .map_or(block.len(), |offset| cursor + offset + 1);
        apply_audio_statement(&block[cursor..end], state);
        cursor = end;
    }
    false
}

fn apply_audio_statement(statement: &str, state: &mut EvaluatedStageAudio) {
    let bgm_assignment =
        Regex::new(r"base\s*\+\s*(0x[0-9A-Fa-f]+)").expect("static BGM assignment regex is valid");
    for (field, destination) in [
        (
            "stageBgmSilent",
            &mut state.secondary_bgm_id as &mut Option<u32>,
        ),
        ("stageBgm", &mut state.primary_bgm_id),
    ] {
        let marker = format!("MSStageInfo::{field}");
        let Some(start) = statement.find(&marker) else {
            continue;
        };
        if statement
            .as_bytes()
            .get(start + marker.len())
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            continue;
        }
        let Some(equal) = statement[start + marker.len()..].find('=') else {
            continue;
        };
        let rhs = statement[start + marker.len() + equal + 1..].trim();
        *destination = if rhs.starts_with("cMSBgmNone") {
            None
        } else {
            bgm_assignment
                .captures(rhs)
                .and_then(|captures| parse_hex(&captures[1]).ok())
                .map(|offset| BGM_BASE + offset)
        };
        return;
    }
    if let Some(start) = statement.find("MSStageInfo::fadeEvent") {
        if let Some(equal) = statement[start..].find('=') {
            let rhs = statement[start + equal + 1..]
                .trim()
                .trim_end_matches(';')
                .trim();
            if let Ok(value) = rhs.parse::<u8>() {
                state.fade_event = value;
            }
        }
    }
}

fn evaluate_area_condition(condition: &str, area: u32) -> Option<bool> {
    let comparison = Regex::new(r"^\s*area\s*(==|!=)\s*(\d+)\s*$")
        .expect("static area condition regex is valid");
    let captures = comparison.captures(condition)?;
    let expected = captures[2].parse::<u32>().ok()?;
    Some(if &captures[1] == "==" {
        area == expected
    } else {
        area != expected
    })
}

fn cpp_statement(source: &str, start: usize) -> Option<(&str, usize)> {
    if source.as_bytes().get(start) == Some(&b'{') {
        let end = matching_delimiter(source, start, b'{', b'}')?;
        Some((&source[start + 1..end], end + 1))
    } else {
        let end = source[start..].find(';')? + start;
        Some((&source[start..end + 1], end + 1))
    }
}

fn switch_case_start(block: &str, value: u32) -> Option<usize> {
    let case = Regex::new(r"(?m)^\s*case\s+(\d+)\s*:").expect("static switch case regex is valid");
    let mut depth = 0_i32;
    for captures in case.captures_iter(block) {
        let matched = captures.get(0)?;
        for byte in block.as_bytes()[..matched.start()].iter().rev() {
            match byte {
                b'}' => depth += 1,
                b'{' => depth -= 1,
                _ => {}
            }
        }
        if depth == 0 && captures[1].parse::<u32>().ok()? == value {
            return Some(matched.end());
        }
        depth = 0;
    }
    None
}

fn keyword_at(source: &str, offset: usize, keyword: &str) -> bool {
    source.get(offset..offset + keyword.len()) == Some(keyword)
        && source
            .as_bytes()
            .get(offset + keyword.len())
            .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
}

fn skip_cpp_space_and_comments(source: &str, mut offset: usize) -> usize {
    loop {
        while source
            .as_bytes()
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            offset += 1;
        }
        if source
            .get(offset..)
            .is_some_and(|tail| tail.starts_with("//"))
        {
            offset = source[offset..]
                .find('\n')
                .map_or(source.len(), |end| offset + end + 1);
            continue;
        }
        if source
            .get(offset..)
            .is_some_and(|tail| tail.starts_with("/*"))
        {
            offset = source[offset + 2..]
                .find("*/")
                .map_or(source.len(), |end| offset + 2 + end + 2);
            continue;
        }
        return offset;
    }
}

fn matching_delimiter(source: &str, open: usize, open_byte: u8, close_byte: u8) -> Option<usize> {
    let mut depth = 0_u32;
    for (relative, byte) in source.as_bytes()[open..].iter().copied().enumerate() {
        if byte == open_byte {
            depth += 1;
        } else if byte == close_byte {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + relative);
            }
        }
    }
    None
}

fn load_retail_sound_assignment_bytes(base_root: &Path) -> Result<Vec<u8>, String> {
    let candidates = [
        base_root.join("files/AudioRes/mSound.asn"),
        base_root.join("AudioRes/mSound.asn"),
        base_root.join("mSound.asn"),
    ];
    let path = candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "could not locate the retail AudioRes/mSound.asn".to_string())?;
    fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn extract_retail_sound_entries(bytes: &[u8]) -> Result<Vec<RetailSoundEntry>, String> {
    const RECORD_SIZE: usize = 0x20;
    const NAME_SIZE: usize = 0x1e;
    if bytes.len() < 0x10 + RECORD_SIZE {
        return Err("retail mSound.asn is too short for its assignment records".to_string());
    }
    let mut entries = BTreeMap::new();
    for record in bytes[0x10..].chunks_exact(RECORD_SIZE) {
        let name_end = record[..NAME_SIZE]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(NAME_SIZE);
        let Ok(symbol) = std::str::from_utf8(&record[..name_end]) else {
            continue;
        };
        if !(symbol.starts_with("MSD_SE_") || symbol.starts_with("MSD_XX_")) {
            continue;
        }
        let sound_id = u32::from(u16::from_be_bytes([
            record[NAME_SIZE],
            record[NAME_SIZE + 1],
        ]));
        let entry = RetailSoundEntry {
            sound_id,
            symbol: symbol.to_string(),
            label: friendly_sound_name(symbol),
        };
        if let Some(previous) = entries.insert(sound_id, entry) {
            return Err(format!(
                "retail mSound.asn assigns both {} and {} to SE 0x{sound_id:04X}",
                previous.symbol, symbol
            ));
        }
    }
    if entries.is_empty() {
        return Err("retail mSound.asn contains no named sound-effect assignments".to_string());
    }
    Ok(entries.into_values().collect())
}

fn friendly_sound_name(symbol: &str) -> String {
    let suffix = symbol
        .strip_prefix("MSD_SE_")
        .or_else(|| symbol.strip_prefix("MSD_XX_"))
        .unwrap_or(symbol);
    title_case_identifier(suffix)
}

fn extract_retail_bgm_names(source: &str) -> Result<BTreeMap<u32, String>, String> {
    let name = Regex::new(r"MSD_BGM_[A-Z0-9_]+").expect("static retail BGM name regex is valid");
    let mut result = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for matched in name.find_iter(source) {
        let symbol = matched.as_str();
        if !seen.insert(symbol.to_string()) {
            continue;
        }
        let ordinal = u32::try_from(result.len() + 1)
            .map_err(|_| "retail BGM name count does not fit u32".to_string())?;
        result.insert(BGM_BASE + ordinal, symbol.to_string());
    }
    if result.is_empty() {
        return Err("retail mSound.asn contains no MSD_BGM names".to_string());
    }
    Ok(result)
}

fn friendly_bgm_name(symbol: &str) -> String {
    let suffix = symbol.strip_prefix("MSD_BGM_").unwrap_or(symbol);
    let known = match suffix {
        "DOLPIC" => "Delfino Plaza",
        "BIANCO" => "Bianco Hills",
        "MAMMA" => "Gelato Beach",
        "PINNAPACO_SEA" => "Pinna Park Beach",
        "PINNAPACO" => "Pinna Park",
        "MARE_SEA" => "Noki Bay",
        "MONTEVILLAGE" => "Pianta Village",
        "SHILENA" => "Sirena Beach",
        "RICCO" => "Ricco Harbor",
        "GET_SHINE" => "Shine Get",
        "CHUBOSS" => "Mini-Boss",
        "BOSSPAKU_DEMO" => "Petey Piranha Demo",
        "CHUBOSS2" => "Mini-Boss 2",
        "DELFINO" => "Delfino Airstrip",
        "MAREVILLAGE" => "Noki Village",
        "KAGEMARIO" => "Shadow Mario",
        "MONTE_ONSEN" => "Pianta Hot Spring",
        "MECHAKUPPA" => "Mecha-Bowser",
        "TITLEBACK" => "Title Background",
        "MONTE_NIGHT" => "Pianta Village Night",
        "TIME_IVENT" => "Timed Event",
        "MONTE_RESCUE" => "Pianta Rescue",
        "MERRY_GO_ROUND" => "Merry-Go-Round",
        "CAMERA_KAGE" => "Shadow Mario Camera",
        "GAMEOVER" => "Game Over",
        "BOSSHANA_2ND3RD" => "Polluted Piranha (2nd/3rd)",
        "BOSSGESO_2DN3RD" => "Gooper Blooper (2nd/3rd)",
        "CHUBOSS_MANTA" => "Phantamanta",
        "MONTE_LAST" => "Pianta Village Finale",
        "KUPPA" => "Bowser",
        "MONTEMAN_RACE" => "Il Piantissimo Race",
        _ => return title_case_identifier(suffix),
    };
    known.to_string()
}

fn title_case_identifier(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            let mut word = first.to_uppercase().collect::<String>();
            word.push_str(&characters.as_str().to_ascii_lowercase());
            word
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_hex(value: &str) -> Result<u32, String> {
    u32::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|error| format!("parse hexadecimal value {value}: {error}"))
}

impl SmsEditorApp {
    pub(super) fn current_stage_music(&self) -> Option<ProjectStageMusic> {
        let stage_id = self.document.as_ref()?.stage_id.as_str();
        self.current_project
            .as_ref()?
            .descriptor
            .stage_music
            .iter()
            .find(|(stage, _)| stage.eq_ignore_ascii_case(stage_id))
            .map(|(_, music)| *music)
    }

    pub(super) fn set_current_stage_music(&mut self, music: Option<ProjectStageMusic>) {
        let Some(stage_id) = self
            .document
            .as_ref()
            .map(|document| document.stage_id.clone())
        else {
            return;
        };
        let Some(project) = &mut self.current_project else {
            self.log
                .push("Stage music requires a saved .sms project.".to_string());
            return;
        };
        let previous = project.descriptor.stage_music.clone();
        project
            .descriptor
            .stage_music
            .retain(|stage, _| !stage.eq_ignore_ascii_case(&stage_id));
        if let Some(music) = music {
            project
                .descriptor
                .stage_music
                .insert(stage_id.clone(), music);
        }
        if let Err(error) = project.save() {
            project.descriptor.stage_music = previous;
            self.log
                .push(format!("Could not save stage music selection: {error}"));
            return;
        }
        self.rebuild_audio_cube_helpers_cache();
        self.log.push(match music {
            Some(music) => format!(
                "Set stage '{stage_id}' music to BGM 0x{:08X} (wave scene 0x{:X}).",
                music.bgm_id, music.wave_scene_id
            ),
            None => format!("Restored stage '{stage_id}' to the game's default music."),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_decomp_bgm_wave_scene_pairs() {
        let source = "case 0x80010001:\n return 0x201;\ncase 0x80010002: return 0x202;";
        let result = extract_bgm_wave_scenes(source).unwrap();
        assert_eq!(result[&0x8001_0001], 0x201);
        assert_eq!(result[&0x8001_0002], 0x202);
    }

    #[test]
    fn extracts_only_primary_music_from_nested_area_cases() {
        let source = r#"
            switch (map) {
            case 1:
                MSStageInfo::stageBgm = base + 0x01;
                switch (area) {
                case 3:
                    MSStageInfo::stageBgm = base + 0x16;
                    break;
                }
                break;
            case 2:
            case 3:
                MSStageInfo::stageBgm = base + 0x02;
                break;
            }
        "#;
        let result = extract_primary_stage_music(source).unwrap();
        assert_eq!(result[&0x8001_0001], BTreeSet::from([1]));
        assert_eq!(result[&0x8001_0002], BTreeSet::from([2, 3]));
        assert!(!result.contains_key(&0x8001_0016));
    }

    #[test]
    fn extracts_ordered_retail_names_and_presents_readable_labels() {
        let names =
            extract_retail_bgm_names("\0MSD_BGM_DOLPIC\0\0MSD_BGM_BIANCO\0MSD_BGM_TIME_IVENT\0")
                .unwrap();
        assert_eq!(names[&0x8001_0001], "MSD_BGM_DOLPIC");
        assert_eq!(names[&0x8001_0002], "MSD_BGM_BIANCO");
        assert_eq!(names[&0x8001_0003], "MSD_BGM_TIME_IVENT");
        assert_eq!(friendly_bgm_name(&names[&0x8001_0001]), "Delfino Plaza");
        assert_eq!(friendly_bgm_name(&names[&0x8001_0003]), "Timed Event");
        assert_eq!(friendly_bgm_name("MSD_BGM_MAIN_TITLE"), "Main Title");
    }

    #[test]
    fn extracts_exact_sound_ids_from_fixed_asn_records() {
        let mut bytes = vec![0_u8; 0x10];
        for (symbol, sound_id) in [
            ("MSD_CAT_ENVIRONMENT", 0x0000_u16),
            ("MSD_SE_EV_GLOBAL_SEA_L", 0x5000),
            ("MSD_SE_EV_GLOBAL_SEA_R", 0x5001),
        ] {
            let mut record = [0_u8; 0x20];
            record[..symbol.len()].copy_from_slice(symbol.as_bytes());
            record[0x1e..].copy_from_slice(&sound_id.to_be_bytes());
            bytes.extend_from_slice(&record);
        }
        let entries = extract_retail_sound_entries(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sound_id, 0x5000);
        assert_eq!(entries[0].symbol, "MSD_SE_EV_GLOBAL_SEA_L");
        assert_eq!(entries[0].label, "Ev Global Sea L");
    }

    #[test]
    #[ignore = "requires SMS_DECOMP_ROOT and SMS_BASE_ROOT"]
    fn indexes_real_decomp_music_catalog() {
        let repo_root = std::env::var_os("SMS_DECOMP_ROOT").expect("SMS_DECOMP_ROOT");
        let base_root = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let entries = index_retail_music(
            Path::new(&repo_root),
            Path::new(&base_root),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(entries.len() >= 40, "found only {} tracks", entries.len());
        assert!(entries.iter().any(|entry| entry.bgm_id == 0x8001_0001));
        assert!(entries.iter().all(|entry| entry.wave_scene_id != u32::MAX));
        assert!(entries
            .iter()
            .all(|entry| !entry.label.starts_with("BGM 0x")));
        let names = load_retail_bgm_names(Path::new(&base_root)).unwrap();
        assert_eq!(names[&0x8001_0001], "MSD_BGM_DOLPIC");
        assert_eq!(names[&0x8001_002F], "MSD_BGM_MONTEMAN_RACE");
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT"]
    fn indexes_real_retail_sound_catalog() {
        let base_root = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let entries = index_retail_sounds(Path::new(&base_root)).unwrap();
        assert!(
            entries.len() >= 1_500,
            "found only {} sounds",
            entries.len()
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.sound_id == 0x5000)
                .map(|entry| entry.symbol.as_str()),
            Some("MSD_SE_EV_GLOBAL_SEA_L")
        );
    }

    #[test]
    #[ignore = "requires SMS_DECOMP_ROOT and SMS_BASE_ROOT"]
    fn evaluates_real_pinna_crossfade_assignments() {
        let repo_root = std::env::var_os("SMS_DECOMP_ROOT").expect("SMS_DECOMP_ROOT");
        let base_root = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let profiles =
            index_retail_stage_audio_profiles(Path::new(&repo_root), Path::new(&base_root))
                .unwrap();
        let pinna = profiles
            .iter()
            .find(|profile| {
                profile.primary_bgm_id == Some(0x8001_0005)
                    && profile.secondary_bgm_id == Some(0x8001_0023)
            })
            .unwrap_or_else(|| panic!("no Pinna crossfade profile in {profiles:#?}"));
        assert_eq!(pinna.wave_scene_id, Some(0x204));
        assert_eq!(pinna.fade_event, 2);
    }
}
