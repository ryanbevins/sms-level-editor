use std::collections::BTreeSet;

use regex::Regex;

use super::{
    braced_body, parse_cpp_u32, MapObjStringTevProgramDefinition, MapObjStringTevVariantDefinition,
};

const COLOR_MEMBERS: [&str; 4] = ["unk15E", "unk160", "unk162", "unk164"];

pub(super) fn extract_nozzle_box_tev_program(
    text: &str,
    header: &str,
    source_file: &str,
) -> Result<MapObjStringTevProgramDefinition, String> {
    require_contiguous_nozzle_color_members(header)?;
    let default_color = extract_constructor_color(text)?;
    let load_re = Regex::new(r"void\s+TNozzleBox::load\s*\([^)]*\)\s*\{")
        .expect("valid TNozzleBox::load regex");
    let load = load_re
        .find(text)
        .ok_or_else(|| "missing TNozzleBox::load".to_string())?;
    let body = braced_body(text, load.end() - 1)
        .ok_or_else(|| "unterminated TNozzleBox::load".to_string())?;
    if !Regex::new(r"TMapObjBase::load\s*\(\s*stream\s*\)")
        .expect("valid base load regex")
        .is_match(body)
    {
        return Err("TNozzleBox::load no longer calls TMapObjBase::load(stream)".to_string());
    }
    if !Regex::new(r"unk158\s*=\s*stream\.readString\s*\(\s*\)\s*;")
        .expect("valid selector read regex")
        .is_match(body)
    {
        return Err("TNozzleBox::load no longer reads its string selector into unk158".to_string());
    }

    let tev_re = Regex::new(
        r"initPacketMatColor\s*\(\s*getModel\s*\(\s*\)\s*,\s*GX_TEVREG([0-3])\s*,\s*(?:\([^()]*\)\s*)?&\s*unk15E\s*\)",
    )
    .expect("valid NozzleBox TEV binding regex");
    let registers = tev_re
        .captures_iter(body)
        .map(|captures| captures[1].parse::<u8>().expect("single TEV digit"))
        .collect::<BTreeSet<_>>();
    if registers.len() != 1 {
        return Err(format!(
            "TNozzleBox::load has {} distinct recognizable TEV color registers; expected one",
            registers.len()
        ));
    }
    let tev_register = *registers.first().expect("one TEV register");

    let branch_re = Regex::new(
        r#"(?:if|else\s+if)\s*\(\s*strcmp\s*\(\s*unk158\s*,\s*"([^"]+)"\s*\)\s*==\s*0\s*\)\s*\{"#,
    )
    .expect("valid NozzleBox selector branch regex");
    let assignment_re = Regex::new(r"\b(unk15E|unk160|unk162|unk164)\s*=\s*([^;]+);")
        .expect("valid NozzleBox color assignment regex");
    let mut variants = Vec::new();
    let mut selectors = BTreeSet::new();
    for branch in branch_re.captures_iter(body) {
        let whole = branch
            .get(0)
            .ok_or_else(|| "missing NozzleBox selector branch match".to_string())?;
        let branch_body = braced_body(body, whole.end() - 1)
            .ok_or_else(|| format!("unterminated NozzleBox selector branch {}", &branch[1]))?;
        let mut color = default_color;
        let mut assigned = BTreeSet::new();
        for assignment in assignment_re.captures_iter(branch_body) {
            let member = assignment[1].to_string();
            let channel = COLOR_MEMBERS
                .iter()
                .position(|candidate| *candidate == member)
                .expect("assignment regex restricts members");
            let value = parse_cpp_u32(&assignment[2])
                .and_then(|value| i16::try_from(value).ok())
                .ok_or_else(|| {
                    format!(
                        "NozzleBox selector {} has non-i16 assignment {}",
                        &branch[1], &assignment[2]
                    )
                })?;
            color[channel] = value;
            assigned.insert(member);
        }
        if !COLOR_MEMBERS[..3]
            .iter()
            .all(|member| assigned.contains(*member))
        {
            return Err(format!(
                "NozzleBox selector {} does not assign every RGB channel",
                &branch[1]
            ));
        }
        let selector_value = branch[1].to_string();
        if !selectors.insert(selector_value.clone()) {
            return Err(format!("duplicate NozzleBox selector {selector_value}"));
        }
        variants.push(MapObjStringTevVariantDefinition {
            selector_value,
            color,
        });
    }
    if variants.is_empty() {
        return Err("TNozzleBox::load has no recognizable selector color branches".to_string());
    }
    Ok(MapObjStringTevProgramDefinition {
        resource_name: "NozzleBox".to_string(),
        class_name: "TNozzleBox".to_string(),
        tev_register,
        default_color,
        variants,
        source_file: source_file.to_string(),
    })
}

fn require_contiguous_nozzle_color_members(header: &str) -> Result<(), String> {
    for (member, offset) in [
        ("unk15E", "15E"),
        ("unk160", "160"),
        ("unk162", "162"),
        ("unk164", "164"),
    ] {
        let pattern = format!(r"/\*\s*0x{offset}\s*\*/\s*u16\s+{member}\s*;");
        if !Regex::new(&pattern)
            .expect("valid generated color-member regex")
            .is_match(header)
        {
            return Err(format!(
                "TNozzleBox color member {member} is not a u16 at 0x{offset}"
            ));
        }
    }
    Ok(())
}

fn extract_constructor_color(text: &str) -> Result<[i16; 4], String> {
    let constructor_re = Regex::new(r"(?s)TNozzleBox::TNozzleBox\s*\([^)]*\)\s*:(.*?)\{")
        .expect("valid TNozzleBox constructor regex");
    let initializer = constructor_re
        .captures(text)
        .ok_or_else(|| "missing TNozzleBox constructor initializer".to_string())?;
    let mut color = [None; 4];
    for (channel, member) in COLOR_MEMBERS.iter().enumerate() {
        let member_re = Regex::new(&format!(r"\b{}\s*\(\s*([^)]+)\s*\)", regex::escape(member)))
            .expect("valid generated constructor member regex");
        let value = member_re
            .captures(&initializer[1])
            .and_then(|captures| parse_cpp_u32(&captures[1]))
            .and_then(|value| i16::try_from(value).ok())
            .ok_or_else(|| format!("missing numeric TNozzleBox constructor color {member}"))?;
        color[channel] = Some(value);
    }
    let [Some(r), Some(g), Some(b), Some(a)] = color else {
        return Err("incomplete TNozzleBox constructor color".to_string());
    };
    Ok([r, g, b, a])
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"
        /* 0x15E */ u16 unk15E;
        /* 0x160 */ u16 unk160;
        /* 0x162 */ u16 unk162;
        /* 0x164 */ u16 unk164;
    "#;
    const SOURCE: &str = r#"
        TNozzleBox::TNozzleBox(const char* name)
            : TMapObjGeneral(name), unk15E(0xFF), unk160(0xFF),
              unk162(0xFF), unk164(100) {}
        void TNozzleBox::load(JSUMemoryInputStream& stream) {
            TMapObjBase::load(stream);
            unk158 = stream.readString();
            if (strcmp(unk158, "normal_nozzle_item") == 0) {
                unk15E = 0; unk160 = 0; unk162 = 0xFF;
            } else if (strcmp(unk158, "rocket_nozzle_item") == 0) {
                unk15E = 0xFF; unk160 = 0; unk162 = 0;
            } else if (strcmp(unk158, "back_nozzle_item") == 0) {
                unk15E = 0x5A; unk160 = 0x5A; unk162 = 0x78;
            }
            initPacketMatColor(getModel(), GX_TEVREG1,
                (const GXColorS10*)&unk15E);
        }
    "#;

    #[test]
    fn extracts_exact_selector_colors_and_constructor_default() {
        let program = extract_nozzle_box_tev_program(SOURCE, HEADER, "src/MoveBG/Item.cpp")
            .expect("extract NozzleBox colors");
        assert_eq!(program.tev_register, 1);
        assert_eq!(program.default_color, [255, 255, 255, 100]);
        assert_eq!(
            program
                .variants
                .iter()
                .map(|variant| (variant.selector_value.as_str(), variant.color))
                .collect::<Vec<_>>(),
            [
                ("normal_nozzle_item", [0, 0, 255, 100]),
                ("rocket_nozzle_item", [255, 0, 0, 100]),
                ("back_nozzle_item", [90, 90, 120, 100]),
            ]
        );
    }

    #[test]
    fn rejects_unrelated_selector_or_noncontiguous_color_layout() {
        let unrelated = SOURCE.replace("strcmp(unk158", "strcmp(otherSelector");
        let error = extract_nozzle_box_tev_program(&unrelated, HEADER, "fixture.cpp").unwrap_err();
        assert!(error.contains("no recognizable selector"), "{error}");

        let moved = HEADER.replace("0x160", "0x168");
        let error = extract_nozzle_box_tev_program(SOURCE, &moved, "fixture.cpp").unwrap_err();
        assert!(error.contains("unk160"), "{error}");
    }

    #[test]
    fn does_not_bind_a_register_across_an_unrelated_color_call() {
        let source = SOURCE.replace(
            "initPacketMatColor(getModel(), GX_TEVREG1,",
            "initPacketMatColor(getModel(), GX_TEVREG2, &otherColor);\n            initPacketMatColor(getModel(), GX_TEVREG1,",
        );
        let program = extract_nozzle_box_tev_program(&source, HEADER, "fixture.cpp")
            .expect("bind only the call that passes the contiguous NozzleBox color");
        assert_eq!(program.tev_register, 1);
    }
}
