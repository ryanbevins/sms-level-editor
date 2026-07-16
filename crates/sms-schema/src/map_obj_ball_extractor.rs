use std::collections::BTreeMap;

use regex::Regex;

use super::{braced_body, parse_cpp_u32, MapObjBallTransformDefinition};

pub(super) fn extract_map_obj_ball_transforms(
    text: &str,
    source_file: &str,
) -> Result<Vec<MapObjBallTransformDefinition>, String> {
    let init_body = method_body(text, "TMapObjBall", "initMapObj")?;
    let switch_re =
        Regex::new(r"switch\s*\(\s*mActorType\s*\)\s*\{").expect("valid TMapObjBall switch regex");
    let switch_match = switch_re
        .find(init_body)
        .ok_or_else(|| "TMapObjBall::initMapObj has no mActorType switch".to_string())?;
    let switch_body = braced_body(init_body, switch_match.end() - 1)
        .ok_or_else(|| "unterminated TMapObjBall actor-type switch".to_string())?;
    let case_group_re = Regex::new(r"(?s)((?:case\s+[^:]+:\s*)+)(.*?\bbreak\s*;)")
        .expect("valid actor-type case group regex");
    let case_re = Regex::new(r"case\s+([^:]+)\s*:").expect("valid actor-type case regex");
    let radius_re = Regex::new(r"mBodyRadius\s*=\s*([0-9]+(?:\.[0-9]+)?)f?\s*\*\s*mScaling\.y\s*;")
        .expect("valid body-radius regex");
    let mut definitions = BTreeMap::<u32, MapObjBallTransformDefinition>::new();
    for group in case_group_re.captures_iter(switch_body) {
        let Some(radius) = radius_re
            .captures(&group[2])
            .and_then(|captures| parse_whole_f32_as_u16(&captures[1]))
        else {
            continue;
        };
        for actor_type in case_re.captures_iter(&group[1]) {
            let actor_type = parse_cpp_u32(&actor_type[1])
                .ok_or_else(|| format!("non-numeric TMapObjBall actor type {}", &actor_type[1]))?;
            definitions.insert(
                actor_type,
                MapObjBallTransformDefinition {
                    actor_type,
                    body_radius: radius,
                    positive_y_axis_subtract: None,
                    one_minus_y_axis_subtract: None,
                    source_file: source_file.to_string(),
                },
            );
        }
    }
    if definitions.is_empty() {
        return Err("TMapObjBall::initMapObj has no recognizable body-radius cases".to_string());
    }

    let override_re = Regex::new(r"if\s*\(\s*isActorType\s*\(\s*([^)]+)\s*\)\s*\)\s*\{")
        .expect("valid actor-type override regex");
    for actor_match in override_re.captures_iter(init_body) {
        let whole = actor_match
            .get(0)
            .ok_or_else(|| "missing actor-type override match".to_string())?;
        let body = braced_body(init_body, whole.end() - 1)
            .ok_or_else(|| format!("unterminated actor-type override {}", &actor_match[1]))?;
        let Some(radius) = radius_re
            .captures(body)
            .and_then(|captures| parse_whole_f32_as_u16(&captures[1]))
        else {
            continue;
        };
        let actor_type = parse_cpp_u32(&actor_match[1])
            .ok_or_else(|| format!("non-numeric actor-type override {}", &actor_match[1]))?;
        definitions
            .entry(actor_type)
            .and_modify(|definition| definition.body_radius = radius)
            .or_insert(MapObjBallTransformDefinition {
                actor_type,
                body_radius: radius,
                positive_y_axis_subtract: None,
                one_minus_y_axis_subtract: None,
                source_file: source_file.to_string(),
            });
    }

    let appeared_body = method_body(text, "TResetFruit", "makeObjAppeared")?;
    if !Regex::new(r"\(\*m\)\[1\]\[3\]\s*=\s*mPosition\.y\s*\+\s*mBodyRadius\s*;")
        .expect("valid reset-fruit base Y regex")
        .is_match(appeared_body)
    {
        return Err(
            "TResetFruit::makeObjAppeared no longer adds mBodyRadius to model-matrix Y".to_string(),
        );
    }
    let positive_re = Regex::new(
        r"(?s)\(\*m\)\[1\]\[1\]\s*>\s*0(?:\.0)?f?.*?-\s*([0-9]+(?:\.[0-9]+)?)f?\s*\*\s*\(\*m\)\[1\]\[1\]",
    )
    .expect("valid positive-axis correction regex");
    let one_minus_re = Regex::new(
        r"(?s)-\s*([0-9]+(?:\.[0-9]+)?)f?\s*\*\s*\(\s*1(?:\.0)?f?\s*-\s*\(\*m\)\[1\]\[1\]\s*\)",
    )
    .expect("valid one-minus-axis correction regex");
    for actor_match in override_re.captures_iter(appeared_body) {
        let whole = actor_match
            .get(0)
            .ok_or_else(|| "missing reset-fruit actor-type match".to_string())?;
        let body = braced_body(appeared_body, whole.end() - 1).ok_or_else(|| {
            format!(
                "unterminated TResetFruit actor-type correction {}",
                &actor_match[1]
            )
        })?;
        let actor_type = parse_cpp_u32(&actor_match[1])
            .ok_or_else(|| format!("non-numeric reset-fruit actor type {}", &actor_match[1]))?;
        let Some(definition) = definitions.get_mut(&actor_type) else {
            return Err(format!(
                "TResetFruit correction references actor type {actor_type:#010x} without a body radius"
            ));
        };
        if let Some(value) = positive_re
            .captures(body)
            .and_then(|captures| parse_whole_f32_as_u16(&captures[1]))
        {
            definition.positive_y_axis_subtract = Some(value);
        }
        if let Some(value) = one_minus_re
            .captures(body)
            .and_then(|captures| parse_whole_f32_as_u16(&captures[1]))
        {
            definition.one_minus_y_axis_subtract = Some(value);
        }
    }
    Ok(definitions.into_values().collect())
}

fn method_body<'a>(text: &'a str, class_name: &str, method_name: &str) -> Result<&'a str, String> {
    let method_re = Regex::new(&format!(
        r"void\s+{}::{}\s*\([^)]*\)\s*\{{",
        regex::escape(class_name),
        regex::escape(method_name)
    ))
    .expect("valid generated method regex");
    let method = method_re
        .find(text)
        .ok_or_else(|| format!("missing {class_name}::{method_name}"))?;
    braced_body(text, method.end() - 1)
        .ok_or_else(|| format!("unterminated {class_name}::{method_name}"))
}

fn parse_whole_f32_as_u16(value: &str) -> Option<u16> {
    let value = value.parse::<f32>().ok()?;
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= f32::from(u16::MAX))
        .then_some(value as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
        void TMapObjBall::initMapObj() {
            switch (mActorType) {
            case 0x40000390:
            case 0x40000391:
            case 0x40000392:
            case 0x40000394:
            case 0x40000395:
                mBodyRadius = 50.0f * mScaling.y;
                break;
            case 0x40000393:
                mBodyRadius = 50.0f * mScaling.y;
                break;
            }
            if (isActorType(0x40000393)) {
                mBodyRadius = 45.0f * mScaling.y;
            }
            if (isActorType(0x40000390)) {
                mBodyRadius = 40.0f * mScaling.y;
            }
            if (isActorType(0x40000391)) {
                mBodyRadius = 40.0f * mScaling.y;
            }
        }
        void TResetFruit::makeObjAppeared() {
            (*m)[1][3] = mPosition.y + mBodyRadius;
            if (isActorType(0x40000394)) {
                if ((*m)[1][1] > 0.0f) {
                    (*m)[1][3] = (*m)[1][3] - 50.0f * (*m)[1][1];
                }
            }
            if (isActorType(0x40000392)) {
                (*m)[1][3] = (*m)[1][3] - 10.0f * (1.0f - (*m)[1][1]);
            }
        }
    "#;

    #[test]
    fn extracts_actor_type_radius_and_matrix_corrections() {
        let definitions = extract_map_obj_ball_transforms(FIXTURE, "src/MoveBG/MapObjBall.cpp")
            .expect("extract ball transforms");
        let by_type = definitions
            .iter()
            .map(|definition| (definition.actor_type, definition))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_type[&0x4000_0395].body_radius, 50);
        assert_eq!(by_type[&0x4000_0393].body_radius, 45);
        assert_eq!(by_type[&0x4000_0390].body_radius, 40);
        assert_eq!(by_type[&0x4000_0394].positive_y_axis_subtract, Some(50));
        assert_eq!(by_type[&0x4000_0392].one_minus_y_axis_subtract, Some(10));
    }
}
