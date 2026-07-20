use sms_scene::SceneObject;

const EXACT_TRANSLATIONS: &[(&str, &str)] = &[
    ("鏡カメラ", "Mirror Camera"),
    ("空", "Sky"),
    ("落書き管理", "Goop Manager"),
    ("コイン", "Coin"),
    ("アイテムグループ", "Item Group"),
    ("インダイレクトシーン", "Indirect Scene"),
    ("オブジェクトグループ", "Object Group"),
    ("コンダクター初期化用", "Conductor Initialization"),
    ("ストラテジ", "Strategy"),
    ("スペキュラシーン", "Specular Scene"),
    ("データテーブル群", "Data Tables"),
    ("プレーヤーグループ", "Player Group"),
    ("ボスグループ", "Boss Group"),
    ("マップグループ", "Map Group"),
    ("マネージャーグループ", "Manager Group"),
    ("全体シーン", "Main Scene"),
    ("初期化用グループ", "Initialization Group"),
    ("敵グループ", "Enemy Group"),
    ("水パーティクルグループ", "Water Particle Group"),
    ("空グループ", "Sky Group"),
    ("落書きグループ", "Goop Group"),
    ("通常シーン", "Normal Scene"),
    ("鏡シーン", "Mirror Scene"),
    ("ＮＰＣグループ", "NPC Group"),
    ("太陽サブ（オブジェクト）", "Sun Sub-Light (Objects)"),
    ("太陽サブ（プレイヤー）", "Sun Sub-Light (Player)"),
    ("太陽サブ（敵）", "Sun Sub-Light (Enemies)"),
    ("太陽スペキュラ（オブジェクト）", "Sun Specular (Objects)"),
    ("太陽スペキュラ（プレイヤー）", "Sun Specular (Player)"),
    ("太陽スペキュラ（敵）", "Sun Specular (Enemies)"),
    ("太陽（オブジェクト）", "Sun (Objects)"),
    ("太陽（プレイヤー）", "Sun (Player)"),
    ("太陽（敵）", "Sun (Enemies)"),
    ("影サブ（オブジェクト）", "Shadow Sub-Light (Objects)"),
    ("影サブ（プレイヤー）", "Shadow Sub-Light (Player)"),
    ("影サブ（敵）", "Shadow Sub-Light (Enemies)"),
    ("影（オブジェクト）", "Shadow (Objects)"),
    ("影（プレイヤー）", "Shadow (Player)"),
    ("影（敵）", "Shadow (Enemies)"),
];

const GENERIC_FACTORY_NAMES: &[&str] = &[
    "CameraMapInfo",
    "CubeGeneralInfo",
    "GroupObj",
    "HideObj",
    "HideObjInfo",
    "IdxGroup",
    "MapObjBase",
    "MapStaticObj",
    "MarScene",
    "NameRefGrp",
    "Strategy",
    "WaterHitHideObj",
];

pub(super) fn bilingual_object_name(object: &SceneObject) -> String {
    let original = object
        .raw_param("name")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(object.factory_name.as_str());
    if !contains_japanese(original) {
        return original.to_string();
    }

    let factory_is_generic = GENERIC_FACTORY_NAMES.contains(&object.factory_name.as_str());
    let selector = ["actor_tail_string", "resource_name", "stream_string_0"]
        .into_iter()
        .filter_map(|key| object.raw_param(key))
        .map(str::trim)
        .find(|value| !value.is_empty() && value.is_ascii() && !value.eq_ignore_ascii_case("null"));
    let hint = translate_game_text(original).unwrap_or_else(|| {
        if factory_is_generic {
            selector
                .map(humanize_identifier)
                .unwrap_or_else(|| humanize_identifier(&object.factory_name))
        } else {
            humanize_identifier(&object.factory_name)
        }
    });

    bilingual_with_english(original, &with_instance_suffix(original, hint))
}

pub(super) fn bilingual_record_name(name: &str, type_name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return humanize_identifier(type_name);
    }
    if !contains_japanese(name) {
        return name.to_string();
    }
    let english = translate_game_text(name).unwrap_or_else(|| humanize_identifier(type_name));
    bilingual_with_english(name, &with_instance_suffix(name, english))
}

pub(super) fn bilingual_game_text(text: &str) -> String {
    if !contains_japanese(text) {
        return text.to_string();
    }
    translate_game_text(text)
        .map(|english| bilingual_with_english(text, &english))
        .unwrap_or_else(|| text.to_string())
}

pub(super) fn bilingual_object_parameter_value(
    object: &SceneObject,
    key: &str,
    value: &str,
) -> String {
    if !contains_japanese(value) {
        return value.to_string();
    }
    if key == "name" {
        return bilingual_object_name(object);
    }
    if value.trim_end().ends_with("キャラ") {
        let english = english_half(&bilingual_object_name(object))
            .map(|name| format!("{name} Character"))
            .unwrap_or_else(|| format!("{} Character", humanize_identifier(&object.factory_name)));
        return bilingual_with_english(value, &english);
    }
    bilingual_game_text(value)
}

pub(super) fn object_parameter_english_translation(
    object: &SceneObject,
    key: &str,
    value: &str,
) -> Option<String> {
    let bilingual = bilingual_object_parameter_value(object, key, value);
    (bilingual != value).then(|| {
        english_half(&bilingual)
            .map(str::to_string)
            .unwrap_or(bilingual)
    })
}

fn translate_game_text(text: &str) -> Option<String> {
    let (base, suffix) = split_instance_suffix(text.trim());
    let translation = EXACT_TRANSLATIONS
        .iter()
        .find_map(|(japanese, english)| (*japanese == base).then_some(*english))?;
    Some(if suffix.is_empty() {
        translation.to_string()
    } else {
        format!("{translation}{suffix}")
    })
}

fn contains_japanese(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xff66..=0xff9f
        )
    })
}

fn with_instance_suffix(original: &str, mut english: String) -> String {
    let (_, suffix) = split_instance_suffix(original.trim());
    if !suffix.is_empty() && !english.ends_with(suffix) {
        english.push_str(suffix);
    }
    english
}

fn split_instance_suffix(text: &str) -> (&str, &str) {
    let split = text
        .rfind(char::is_whitespace)
        .filter(|index| text[*index..].trim().chars().all(|ch| ch.is_ascii_digit()));
    split.map_or((text, ""), |index| (&text[..index], &text[index..]))
}

fn bilingual_with_english(original: &str, english: &str) -> String {
    format!("{original} ({english})")
}

fn english_half(bilingual: &str) -> Option<&str> {
    bilingual
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once(" (").map(|(_, english)| english))
}

fn humanize_identifier(identifier: &str) -> String {
    let chars = identifier.chars().collect::<Vec<_>>();
    let mut output = String::new();
    for (index, ch) in chars.iter().copied().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|index| chars.get(index))
            .copied();
        let next = chars.get(index + 1).copied();
        let boundary = !output.is_empty()
            && !output.ends_with(' ')
            && ((ch.is_ascii_uppercase()
                && previous.is_some_and(|previous| previous.is_ascii_lowercase())
                || ch.is_ascii_uppercase()
                    && previous.is_some_and(|previous| previous.is_ascii_uppercase())
                    && next.is_some_and(|next| next.is_ascii_lowercase()))
                || (ch.is_ascii_digit()
                    && previous.is_some_and(|previous| !previous.is_ascii_digit())));
        if boundary {
            output.push(' ');
        }
        if matches!(ch, '_' | '-' | '/') {
            if !output.ends_with(' ') {
                output.push(' ');
            }
        } else {
            output.push(ch);
        }
    }
    let mut output = output.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(first) = output.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_hierarchy_names_are_bilingual() {
        for (japanese, english) in [
            ("通常シーン", "Normal Scene"),
            ("ストラテジ", "Strategy"),
            ("空グループ", "Sky Group"),
            ("落書きグループ", "Goop Group"),
            ("プレーヤーグループ", "Player Group"),
        ] {
            assert_eq!(
                bilingual_record_name(japanese, "GroupObj"),
                format!("{japanese} ({english})")
            );
        }
    }

    #[test]
    fn every_retail_japanese_hierarchy_group_has_an_explicit_translation() {
        for (japanese, english) in [
            ("アイテムグループ", "Item Group"),
            ("インダイレクトシーン", "Indirect Scene"),
            ("オブジェクトグループ", "Object Group"),
            ("コンダクター初期化用", "Conductor Initialization"),
            ("ストラテジ", "Strategy"),
            ("スペキュラシーン", "Specular Scene"),
            ("データテーブル群", "Data Tables"),
            ("プレーヤーグループ", "Player Group"),
            ("ボスグループ", "Boss Group"),
            ("マップグループ", "Map Group"),
            ("マネージャーグループ", "Manager Group"),
            ("全体シーン", "Main Scene"),
            ("初期化用グループ", "Initialization Group"),
            ("敵グループ", "Enemy Group"),
            ("水パーティクルグループ", "Water Particle Group"),
            ("空グループ", "Sky Group"),
            ("落書きグループ", "Goop Group"),
            ("通常シーン", "Normal Scene"),
            ("鏡シーン", "Mirror Scene"),
            ("ＮＰＣグループ", "NPC Group"),
        ] {
            assert_eq!(translate_game_text(japanese).as_deref(), Some(english));
        }
    }

    #[test]
    fn object_names_use_runtime_types_and_keep_instance_numbers() {
        let mut mirror = SceneObject::new("mirror", "MirrorCamera");
        mirror.set_raw_param("name", "鏡カメラ");
        assert_eq!(bilingual_object_name(&mirror), "鏡カメラ (Mirror Camera)");

        let mut pollution = SceneObject::new("pollution", "Pollution");
        pollution.set_raw_param("name", "落書き管理");
        assert_eq!(
            bilingual_object_name(&pollution),
            "落書き管理 (Goop Manager)"
        );

        let mut coin = SceneObject::new("coin", "Coin");
        coin.set_raw_param("name", "コイン 12");
        assert_eq!(bilingual_object_name(&coin), "コイン 12 (Coin 12)");
    }

    #[test]
    fn generic_object_types_prefer_the_asset_selector() {
        let mut waterfall = SceneObject::new("waterfall", "MapStaticObj");
        waterfall.set_raw_param("name", "滝つぼ");
        waterfall.set_raw_param("actor_tail_string", "falls");
        assert_eq!(bilingual_object_name(&waterfall), "滝つぼ (Falls)");
    }

    #[test]
    fn raw_character_names_receive_the_same_english_object_label() {
        let mut sky = SceneObject::new("sky", "Sky");
        sky.set_raw_param("name", "空");
        assert_eq!(
            bilingual_object_parameter_value(&sky, "stream_string_0", "空 キャラ"),
            "空 キャラ (Sky Character)"
        );
    }

    #[test]
    fn ascii_editor_and_game_names_are_unchanged() {
        let object = SceneObject::new("mario", "Mario");
        assert_eq!(bilingual_object_name(&object), "Mario");
        assert_eq!(bilingual_game_text("Mario"), "Mario");
    }
}
