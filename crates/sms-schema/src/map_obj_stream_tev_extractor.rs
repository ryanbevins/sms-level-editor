use regex::Regex;

use super::{braced_body, parse_cpp_u32, MapObjStreamTevColorDefinition};

pub(super) fn extract_map_obj_stream_tev_colors(
    text: &str,
    source_file: &str,
) -> Vec<MapObjStreamTevColorDefinition> {
    let load_re = Regex::new(
        r"void\s+(T[A-Za-z_][A-Za-z0-9_]*)::load\s*\(\s*JSUMemoryInputStream\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\{",
    )
    .expect("valid map-object load regex");
    let mut definitions = Vec::new();
    for load in load_re.captures_iter(text) {
        let Some(whole) = load.get(0) else {
            continue;
        };
        let Some(body) = braced_body(text, whole.end() - 1) else {
            continue;
        };
        if let Some(definition) =
            extract_load_stream_tev_color(body, &load[1], &load[2], source_file)
        {
            definitions.push(definition);
        }
    }
    definitions
}

fn extract_load_stream_tev_color(
    body: &str,
    class_name: &str,
    stream_name: &str,
    source_file: &str,
) -> Option<MapObjStreamTevColorDefinition> {
    let packet_re = Regex::new(
        r"initPacketMatColor\s*\(\s*getModel\s*\(\s*\)\s*,\s*(?:\(\s*GXTevRegID\s*\)\s*([1-3])|GX_TEVREG([0-2]))\s*,\s*(?:\([^)]*\)\s*)?&\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .expect("valid map-object packet color regex");
    let packet = packet_re.captures(body)?;
    // GX_TEVPREV occupies enum value 0, so numeric GXTevRegID casts are one
    // greater than the zero-based REG0..REG2 material-array index. Symbolic
    // GX_TEVREGn spellings already expose that zero-based suffix directly.
    let tev_register = if let Some(numeric) = packet.get(1) {
        numeric.as_str().parse::<u8>().ok()?.checked_sub(1)?
    } else {
        packet.get(2)?.as_str().parse::<u8>().ok()?
    };
    let first_color_member = packet.get(3)?.as_str();

    let read_re = Regex::new(&format!(
        r"{}\.read\s*\(\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*4\s*\)\s*;",
        regex::escape(stream_name)
    ))
    .expect("valid fixed-width stream read regex");
    let reads = read_re
        .captures_iter(body)
        .filter_map(|capture| {
            Some((
                capture.get(0)?.start(),
                capture.get(1)?.as_str().to_string(),
            ))
        })
        .collect::<Vec<_>>();
    let trailing_reads = reads.get(reads.len().checked_sub(3)?..)?;
    let first_trailing_read = trailing_reads.first()?.0;

    let assignment_re = Regex::new(
        r"\b([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:\(\s*u16\s*\)\s*)?(?:\(\s*u8\s*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*;",
    )
    .expect("valid stream color assignment regex");
    let assignments = assignment_re
        .captures_iter(&body[first_trailing_read..])
        .filter_map(|capture| {
            Some((
                capture.get(1)?.as_str().to_string(),
                capture.get(2)?.as_str().to_string(),
            ))
        })
        .collect::<Vec<_>>();
    let mut color_members = Vec::with_capacity(3);
    for (_, read_variable) in trailing_reads {
        let member = assignments
            .iter()
            .find_map(|(member, value)| (value == read_variable).then(|| member.clone()))?;
        color_members.push(member);
    }
    if color_members.first().map(String::as_str) != Some(first_color_member) {
        return None;
    }

    let alpha_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;]+)\s*;")
        .expect("valid stream color alpha regex");
    let alpha = alpha_re
        .captures_iter(&body[first_trailing_read..])
        .filter(|capture| {
            let member = &capture[1];
            !color_members.iter().any(|candidate| candidate == member)
        })
        .find_map(|capture| {
            parse_cpp_u32(&capture[2]).and_then(|value| i16::try_from(value).ok())
        })?;

    Some(MapObjStreamTevColorDefinition {
        class_name: class_name.to_string(),
        tev_register,
        trailing_rgb_u32_count: 3,
        alpha,
        source_file: source_file.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_class_agnostic_trailing_rgb_tev_program() {
        let source = r#"
            void TFixturePaint::load(JSUMemoryInputStream& input) {
                TMapObjBase::load(input);
                u32 red, green, blue;
                input.read(&red, 4);
                input.read(&green, 4);
                input.read(&blue, 4);
                tint0 = (u16)(u8)red;
                tint1 = (u16)(u8)green;
                tint2 = (u16)(u8)blue;
                tint3 = 0xff;
                TMapObjBase::initPacketMatColor(
                    getModel(), (GXTevRegID)2, (GXColorS10*)&tint0);
            }
        "#;
        assert_eq!(
            extract_map_obj_stream_tev_colors(source, "src/MoveBG/Fixture.cpp"),
            [MapObjStreamTevColorDefinition {
                class_name: "TFixturePaint".to_string(),
                tev_register: 1,
                trailing_rgb_u32_count: 3,
                alpha: 255,
                source_file: "src/MoveBG/Fixture.cpp".to_string(),
            }]
        );
    }

    #[test]
    fn rejects_non_trailing_or_unbound_rgb_reads() {
        let source = r#"
            void TFixturePaint::load(JSUMemoryInputStream& stream) {
                u32 r, g, b, later;
                stream.read(&r, 4); stream.read(&g, 4); stream.read(&b, 4);
                tint0 = (u16)(u8)r; tint1 = (u16)(u8)g; tint2 = (u16)(u8)b;
                tint3 = 255;
                stream.read(&later, 4);
                initPacketMatColor(getModel(), GX_TEVREG1, (GXColorS10*)&tint0);
            }
        "#;
        assert!(extract_map_obj_stream_tev_colors(source, "fixture.cpp").is_empty());
    }
}
