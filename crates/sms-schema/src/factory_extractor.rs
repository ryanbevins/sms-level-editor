use std::collections::BTreeSet;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactoryEvidence {
    /// The name guard directly returns a newly constructed class.
    ConstructedReturn,
    /// The name is handled by the factory but returns an existing object.
    NameComparison,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactoryCandidate {
    pub(crate) factory_name: String,
    pub(crate) class_name: Option<String>,
    pub(crate) evidence: FactoryEvidence,
}

/// Extract only names demonstrated by factory control flow.
///
/// Deliberately unrelated string literals are not candidates. In particular,
/// resource names and texture names declared near a factory must never become
/// editable scene object types.
pub(crate) fn extract_factory_candidates(text: &str) -> Vec<FactoryCandidate> {
    let factory_return_re = Regex::new(
        r#"strcmp\s*\(\s*name\s*,\s*"([^"]+)"\s*\)\s*==\s*0\s*\)\s*(?:\{[^}]*?)?return\s+(?:[A-Za-z0-9_:]+\s*=\s*)?new\s+([A-Za-z_:][A-Za-z0-9_:]*)"#,
    )
    .expect("valid factory regex");
    let compare_re = Regex::new(r#"strcmp\s*\(\s*name\s*,\s*"([^"]+)"\s*\)\s*==\s*0"#)
        .expect("valid factory-name comparison regex");

    let mut candidates = Vec::new();
    let mut handled_names = BTreeSet::new();
    for captures in factory_return_re.captures_iter(text) {
        let factory_name = captures[1].to_string();
        handled_names.insert(factory_name.clone());
        candidates.push(FactoryCandidate {
            factory_name,
            class_name: Some(captures[2].to_string()),
            evidence: FactoryEvidence::ConstructedReturn,
        });
    }

    for captures in compare_re.captures_iter(text) {
        let factory_name = captures[1].to_string();
        if handled_names.insert(factory_name.clone()) {
            candidates.push(FactoryCandidate {
                factory_name,
                class_name: None,
                evidence: FactoryEvidence::NameComparison,
            });
        }
    }

    // Some retail factories compare `name` against a bounded static table
    // instead of spelling each string literal at the call site. Only accept a
    // table when that exact identifier participates in the factory comparison;
    // this keeps unrelated resource-name arrays out of the object registry.
    let string_table_re = Regex::new(
        r#"(?s)(?:static\s+)?(?:const\s+)?char\s*\*\s*(?:const\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\[\s*\]\s*=\s*\{(.*?)\}\s*;"#,
    )
    .expect("valid factory string-table regex");
    let string_literal_re =
        Regex::new(r#""([^"\\]*(?:\\.[^"\\]*)*)""#).expect("valid C++ string-literal regex");

    for table in string_table_re.captures_iter(text) {
        let table_name = &table[1];
        let comparison = format!(
            r"strcmp\s*\(\s*name\s*,\s*{}\s*\[[^\]]+\]\s*\)\s*==\s*0",
            regex::escape(table_name)
        );
        if !Regex::new(&comparison)
            .expect("escaped identifier produces a valid regex")
            .is_match(text)
        {
            continue;
        }

        let constructed_return = Regex::new(&format!(
            r"{}\s*\)\s*(?:\{{[^}}]*?)?return\s+(?:[A-Za-z0-9_:]+\s*=\s*)?new\s+([A-Za-z_:][A-Za-z0-9_:]*)",
            comparison
        ))
        .expect("escaped identifier produces a valid return regex")
        .captures(text)
        .map(|captures| captures[1].to_string());
        let evidence = if constructed_return.is_some() {
            FactoryEvidence::ConstructedReturn
        } else {
            FactoryEvidence::NameComparison
        };

        for literal in string_literal_re.captures_iter(&table[2]) {
            let factory_name = literal[1].to_string();
            if handled_names.insert(factory_name.clone()) {
                candidates.push(FactoryCandidate {
                    factory_name,
                    class_name: constructed_return.clone(),
                    evidence,
                });
            }
        }
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_unrelated_resource_strings() {
        let text = r#"
            static const char* pollutionTexture = "H_ma_rak_dummy";
            if (strcmp(name, "BossEel") == 0)
                return new TBossEel;
        "#;

        let candidates = extract_factory_candidates(text);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].factory_name, "BossEel");
        assert_eq!(candidates[0].class_name.as_deref(), Some("TBossEel"));
        assert_eq!(candidates[0].evidence, FactoryEvidence::ConstructedReturn);
    }

    #[test]
    fn retains_compare_only_factory_branches_with_provenance() {
        let text = r#"
            if (strcmp(name, "coin") == 0)
                return gpItemManager->coin;
        "#;

        let candidates = extract_factory_candidates(text);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].factory_name, "coin");
        assert_eq!(candidates[0].class_name, None);
        assert_eq!(candidates[0].evidence, FactoryEvidence::NameComparison);
    }

    #[test]
    fn extracts_only_factory_controlled_string_tables() {
        let text = r#"
            static const char* resource_names[] = { "H_ma_rak_dummy", nullptr };
            static const char* item_names[] = {
                "mario_cap", "bottle_large", "bottle_short",
                "GesoSurfBoardStatic", "GesoSurfBoard", nullptr
            };

            for (int i = 0; item_names[i]; ++i)
                if (strcmp(name, item_names[i]) == 0)
                    return new TItem(name);
        "#;

        let candidates = extract_factory_candidates(text);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.factory_name.as_str())
                .collect::<Vec<_>>(),
            [
                "mario_cap",
                "bottle_large",
                "bottle_short",
                "GesoSurfBoardStatic",
                "GesoSurfBoard"
            ]
        );
        assert!(candidates.iter().all(|candidate| {
            candidate.class_name.as_deref() == Some("TItem")
                && candidate.evidence == FactoryEvidence::ConstructedReturn
        }));
    }
}
