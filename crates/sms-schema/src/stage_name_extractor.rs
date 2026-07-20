use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use regex::Regex;

use crate::{Result, SchemaError, SchemaExtractor};

/// Decomp-derived indexes used by Sunshine to select localized stage and scenario messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageNameTables {
    area_stage_name_indices: Vec<usize>,
    normal_scenario_name_indices: Vec<Option<Vec<usize>>>,
    ex_area_start: usize,
    ex_scenario_name_indices: Vec<Option<usize>>,
}

impl StageNameTables {
    pub fn stage_name_index(&self, area_index: u32) -> Option<usize> {
        self.area_stage_name_indices
            .get(usize::try_from(area_index).ok()?)
            .copied()
    }

    pub fn normal_scenario_name_index(
        &self,
        area_index: u32,
        scenario_index: u32,
    ) -> Option<usize> {
        let stage = self.stage_name_index(area_index)?;
        self.normal_scenario_name_indices
            .get(stage)?
            .as_ref()?
            .get(usize::try_from(scenario_index).ok()?)
            .copied()
    }

    pub fn normal_scenario_count(&self, area_index: u32) -> Option<usize> {
        let stage = self.stage_name_index(area_index)?;
        self.normal_scenario_name_indices
            .get(stage)?
            .as_ref()
            .map(Vec::len)
    }

    pub fn ex_scenario_name_index(&self, area_index: u32) -> Option<usize> {
        let area = usize::try_from(area_index).ok()?;
        let offset = area.checked_sub(self.ex_area_start)?;
        self.ex_scenario_name_indices.get(offset).copied().flatten()
    }
}

pub fn extract_stage_name_tables(repo_root: impl AsRef<Path>) -> Result<StageNameTables> {
    let repo_root = repo_root.as_ref();
    let source_path = repo_root.join("src/System/StageUtil.cpp");
    let header_path = repo_root.join("include/System/StageUtil.hpp");
    let source = read_source(&source_path)?;
    let header = read_source(&header_path)?;

    let area_stage_name_indices = required_numeric_array(&source, "shineStageTable", &source_path)?;
    let ex_shine_ids = required_numeric_array(&source, "exShineTable", &source_path)?;
    let scenario_message_indices =
        required_numeric_array(&header, "scScenarioNameTable", &header_path)?;
    let normal_table_names = required_identifier_array(&header, "scShineConvTable", &header_path)?;
    let ex_area_start = extract_ex_area_start(&source).ok_or_else(|| {
        extraction_drift(
            &source_path,
            "the ex-stage array base used by SMS_getShineIDofExStage",
        )
    })?;

    let numeric_arrays = extract_all_numeric_arrays(&header);
    let mut normal_scenario_name_indices = Vec::with_capacity(normal_table_names.len());
    for table_name in normal_table_names {
        let Some(table_name) = table_name else {
            normal_scenario_name_indices.push(None);
            continue;
        };
        let shine_ids = numeric_arrays
            .get(&table_name)
            .ok_or_else(|| extraction_drift(&header_path, "every scShineConvTable target array"))?;
        let message_indices = shine_ids
            .iter()
            .map(|shine_id| scenario_message_indices.get(*shine_id).copied())
            .collect::<Option<Vec<_>>>();
        // Airport has a Shine ID but no entry in scenarioname.bmg. Sunshine
        // therefore has a valid conversion array without a displayable episode
        // title; retain the stage mapping and omit only that scenario table.
        normal_scenario_name_indices.push(message_indices);
    }

    let ex_scenario_name_indices = ex_shine_ids
        .into_iter()
        .map(|shine_id| {
            (shine_id != 0xff)
                .then(|| scenario_message_indices.get(shine_id).copied())
                .flatten()
        })
        .collect::<Vec<_>>();

    if area_stage_name_indices.is_empty()
        || normal_scenario_name_indices.is_empty()
        || ex_scenario_name_indices.is_empty()
    {
        return Err(extraction_drift(
            &source_path,
            "non-empty stage and scenario lookup tables",
        ));
    }

    Ok(StageNameTables {
        area_stage_name_indices,
        normal_scenario_name_indices,
        ex_area_start,
        ex_scenario_name_indices,
    })
}

fn read_source(path: &Path) -> Result<String> {
    if !path.is_file() {
        return Err(SchemaError::MissingSource(path.to_path_buf()));
    }
    fs::read_to_string(path).map_err(|source| SchemaError::SourceRead {
        path: path.to_path_buf(),
        source,
    })
}

fn required_numeric_array(text: &str, name: &str, path: &Path) -> Result<Vec<usize>> {
    extract_numeric_array(text, name)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| extraction_drift(path, "the required numeric stage lookup array"))
}

fn required_identifier_array(text: &str, name: &str, path: &Path) -> Result<Vec<Option<String>>> {
    extract_identifier_array(text, name)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| extraction_drift(path, "the normal Shine conversion table"))
}

fn extraction_drift(path: &Path, expected: &'static str) -> SchemaError {
    SchemaError::ExtractionDrift {
        extractor: SchemaExtractor::StageNames,
        source_path: path.to_path_buf(),
        expected,
    }
}

fn extract_all_numeric_arrays(text: &str) -> BTreeMap<String, Vec<usize>> {
    let declaration = Regex::new(
        r"(?m)\b(?:static\s+)?(?:const\s+)?u(?:8|16|32)\s+([A-Za-z_]\w*)\s*\[\s*\]\s*=\s*\{",
    )
    .expect("numeric array regex");
    declaration
        .captures_iter(text)
        .filter_map(|capture| {
            let name = capture.get(1)?.as_str().to_string();
            extract_numeric_array(text, &name).map(|values| (name, values))
        })
        .collect()
}

fn extract_numeric_array(text: &str, name: &str) -> Option<Vec<usize>> {
    let body = array_initializer(text, name)?;
    split_initializer(&body)
        .map(|token| parse_cpp_usize(&token))
        .collect()
}

fn extract_identifier_array(text: &str, name: &str) -> Option<Vec<Option<String>>> {
    let body = array_initializer(text, name)?;
    split_initializer(&body)
        .map(|token| {
            let token = token.trim();
            if token == "nullptr" || token == "NULL" || token == "0" {
                Some(None)
            } else if token
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
            {
                Some(Some(token.to_string()))
            } else {
                None
            }
        })
        .collect()
}

fn array_initializer(text: &str, name: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r"\b{}\s*\[\s*\]\s*=\s*\{{", regex::escape(name))).ok()?;
    let start = pattern.find(text)?.end();
    let bytes = text.as_bytes();
    let mut depth = 1usize;
    let mut cursor = start;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..cursor].to_string());
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn split_initializer(body: &str) -> impl Iterator<Item = String> + '_ {
    let line_comment = Regex::new(r"//[^\r\n]*").expect("line comment regex");
    let block_comment = Regex::new(r"(?s)/\*.*?\*/").expect("block comment regex");
    let without_comments = line_comment.replace_all(body, "");
    let without_comments = block_comment.replace_all(&without_comments, "");
    without_comments
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
}

fn parse_cpp_usize(token: &str) -> Option<usize> {
    let token = token.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        usize::from_str_radix(hex, 16).ok()
    } else {
        token.parse().ok()
    }
}

fn extract_ex_area_start(text: &str) -> Option<usize> {
    let pattern = Regex::new(r"exShineTable\s*\[\s*\w+\s*-\s*(0[xX][0-9A-Fa-f]+|\d+)\s*\]").ok()?;
    parse_cpp_usize(pattern.captures(text)?.get(1)?.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_message_indexes_from_decomp_tables() {
        let source = r#"
            static u8 shineStageTable[] = { 0, 2, 3, 4 };
            static u8 exShineTable[] = { 0xFF, 2, 1 };
            return exShineTable[r3 - 0x15];
        "#;
        let header = r#"
            static const u8 scShineTableAirport[] = { 2 };
            static const u8 scShineTableBianco[] = { 1, 0 };
            static const u8* scShineConvTable[] = {
                scShineTableAirport, nullptr, scShineTableBianco, nullptr,
            };
            static u32 scScenarioNameTable[] = { 9, 8, 7 };
        "#;

        let area = extract_numeric_array(source, "shineStageTable").unwrap();
        let ex = extract_numeric_array(source, "exShineTable").unwrap();
        let messages = extract_numeric_array(header, "scScenarioNameTable").unwrap();
        let names = extract_identifier_array(header, "scShineConvTable").unwrap();
        let arrays = extract_all_numeric_arrays(header);
        let normal = names
            .into_iter()
            .map(|name| {
                name.map(|name| {
                    arrays[&name]
                        .iter()
                        .map(|shine| messages[*shine])
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let tables = StageNameTables {
            area_stage_name_indices: area,
            normal_scenario_name_indices: normal,
            ex_area_start: extract_ex_area_start(source).unwrap(),
            ex_scenario_name_indices: ex
                .into_iter()
                .map(|shine| {
                    (shine != 0xff)
                        .then(|| messages.get(shine).copied())
                        .flatten()
                })
                .collect(),
        };

        assert_eq!(tables.stage_name_index(1), Some(2));
        assert_eq!(tables.normal_scenario_name_index(1, 0), Some(8));
        assert_eq!(tables.normal_scenario_name_index(1, 1), Some(9));
        assert_eq!(tables.ex_scenario_name_index(0x15), None);
        assert_eq!(tables.ex_scenario_name_index(0x16), Some(7));
    }
}
