use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui;
use egui::{FontData, FontDefinitions, FontFamily};

const PROPORTIONAL_CJK_FAMILIES: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans CJK TC",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
    "Source Han Sans SC",
    "Source Han Sans TC",
    "Source Han Sans JP",
    "Source Han Sans K",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];

const MONOSPACE_CJK_FAMILIES: &[&str] = &[
    "Noto Sans Mono CJK SC",
    "Noto Sans Mono CJK TC",
    "Noto Sans Mono CJK JP",
    "Noto Sans Mono CJK KR",
    "Source Han Mono SC",
    "Sarasa Mono SC",
    "WenQuanYi Zen Hei Mono",
];

#[derive(Clone, Debug, Default)]
pub struct CjkFontStatus {
    pub enabled: bool,
    pub loaded_fonts: Vec<String>,
}

impl CjkFontStatus {
    pub fn unavailable(&self) -> bool {
        self.enabled && self.loaded_fonts.is_empty()
    }
}

pub fn configure_fonts(
    ctx: &egui::Context,
    cjk_fallback_enabled: bool,
    preferred_font: &str,
) -> CjkFontStatus {
    let mut definitions = FontDefinitions::default();
    let mut status = CjkFontStatus {
        enabled: cjk_fallback_enabled,
        loaded_fonts: Vec::new(),
    };

    if cjk_fallback_enabled {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut inserted = HashMap::new();
        add_family_fallbacks(
            &db,
            &mut definitions,
            FontFamily::Proportional,
            "prop",
            &candidate_families(preferred_font, PROPORTIONAL_CJK_FAMILIES),
            &mut inserted,
            &mut status,
        );
        add_family_fallbacks(
            &db,
            &mut definitions,
            FontFamily::Monospace,
            "mono",
            &candidate_families(preferred_font, MONOSPACE_CJK_FAMILIES),
            &mut inserted,
            &mut status,
        );
        status.loaded_fonts.sort();
        status.loaded_fonts.dedup();
    }

    ctx.set_fonts(definitions);
    status
}

fn add_family_fallbacks(
    db: &fontdb::Database,
    definitions: &mut FontDefinitions,
    family: FontFamily,
    prefix: &str,
    candidates: &[String],
    inserted: &mut HashMap<FontKey, String>,
    status: &mut CjkFontStatus,
) {
    for candidate in candidates {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(candidate)],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        };
        let Some(id) = db.query(&query) else {
            continue;
        };
        let Some(face) = db.face(id) else {
            continue;
        };
        let key = FontKey::from_face(face);
        let font_name = if let Some(font_name) = inserted.get(&key) {
            font_name.clone()
        } else {
            let Some((bytes, face_index)) =
                db.with_face_data(id, |data, face_index| (data.to_vec(), face_index))
            else {
                continue;
            };
            let mut data = FontData::from_owned(bytes);
            data.index = face_index;
            let font_name = format!("oxidex-cjk-{prefix}-{}", inserted.len());
            definitions
                .font_data
                .insert(font_name.clone(), Arc::new(data));
            inserted.insert(key, font_name.clone());
            status.loaded_fonts.push(candidate.clone());
            font_name
        };

        if let Some(families) = definitions.families.get_mut(&family)
            && !families.iter().any(|existing| existing == &font_name)
        {
            families.push(font_name);
        }
    }
}

fn candidate_families(preferred: &str, defaults: &[&str]) -> Vec<String> {
    let preferred = preferred.trim();
    let mut out = Vec::new();
    if !preferred.is_empty() {
        out.push(preferred.to_owned());
    }
    for family in defaults {
        if !out.iter().any(|existing| existing == family) {
            out.push((*family).to_owned());
        }
    }
    out
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FontKey {
    source: String,
    index: u32,
}

impl FontKey {
    fn from_face(face: &fontdb::FaceInfo) -> Self {
        Self {
            source: source_key(&face.source),
            index: face.index,
        }
    }
}

fn source_key(source: &fontdb::Source) -> String {
    match source {
        fontdb::Source::Binary(_) => "binary".to_owned(),
        fontdb::Source::File(path) => path.display().to_string(),
        fontdb::Source::SharedFile(path, _) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_font_is_first_and_deduped() {
        let families = candidate_families("Noto Sans CJK SC", PROPORTIONAL_CJK_FAMILIES);
        assert_eq!(
            families.first().map(String::as_str),
            Some("Noto Sans CJK SC")
        );
        assert_eq!(
            families
                .iter()
                .filter(|family| family.as_str() == "Noto Sans CJK SC")
                .count(),
            1
        );
    }

    #[test]
    fn empty_preferred_font_uses_default_order() {
        let families = candidate_families("  ", PROPORTIONAL_CJK_FAMILIES);
        assert_eq!(
            families.first().map(String::as_str),
            Some("Noto Sans CJK SC")
        );
    }
}
