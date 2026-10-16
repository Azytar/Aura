use std::fs;
use std::path::{Path, PathBuf};

use swash::FontRef;

use super::{FaceData, Font, normalize_name, search_font_dir};

pub(super) fn load(path: &Path) -> Result<FaceData, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    FontRef::from_index(&bytes, 0).ok_or_else(|| format!("invalid font: {}", path.display()))?;
    Ok(FaceData { bytes, index: 0 })
}

pub(super) fn style_path(family: &str, style: &str) -> Option<PathBuf> {
    if Path::new(family).is_file() {
        return Some(PathBuf::from(family));
    }
    if let Some(path) = Font::resolve_family_path(family)
        && style_attributes_match(&path, style)
    {
        return Some(path);
    }
    for style in [
        style.to_owned(),
        style
            .replace("Italic", "Oblique")
            .replace("italic", "oblique"),
    ] {
        if let Some(path) = Font::resolve_family_path(&format!("{family} {style}"))
            && named_style_matches(&path, family, &style)
        {
            return Some(path);
        }
    }
    None
}

pub(super) fn sibling_style_path(path: &Path, style: &str) -> Option<PathBuf> {
    let family = family_stem(path);
    let parent = path.parent().unwrap_or(Path::new("."));
    for style in [
        style.to_owned(),
        style
            .replace("Italic", "Oblique")
            .replace("italic", "oblique"),
    ] {
        if let Some(path) = search_font_dir(parent, &format!("{family}{style}"))
            && named_style_matches(&path, &family, &style)
        {
            return Some(path);
        }
    }
    None
}

fn named_style_matches(path: &Path, family: &str, style: &str) -> bool {
    let stem = normalize_name(
        &path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase(),
    );
    let query = normalize_name(&format!("{family}{style}").to_lowercase());
    // General family search permits a shorter family prefix. A style query
    // must not accept Regular before trying the Italic/Oblique alias.
    if !stem.contains(&query) {
        return false;
    }
    if matches!(
        normalize_name(&style.to_lowercase()).as_str(),
        "regular" | "bold" | "italic" | "oblique" | "bolditalic" | "boldoblique"
    ) {
        style_attributes_match(path, style)
    } else {
        true
    }
}

fn style_attributes_match(path: &Path, style: &str) -> bool {
    let expected = match normalize_name(&style.to_lowercase()).as_str() {
        "regular" => (false, false),
        "bold" => (true, false),
        "italic" | "oblique" => (false, true),
        "bolditalic" | "boldoblique" => (true, true),
        _ => return false,
    };
    let Ok(face) = load(path) else {
        return false;
    };
    let attributes = FontRef::from_index(&face.bytes, face.index)
        .unwrap()
        .attributes();
    (
        attributes.weight().0 >= 600,
        attributes.style() != swash::Style::Normal,
    ) == expected
}

pub(super) fn load_styles(path: &Path) -> [Option<FaceData>; 3] {
    let family = family_stem(path);
    let parent = path.parent().unwrap_or(Path::new("."));
    std::array::from_fn(|slot| {
        let names: &[&str] = match slot {
            0 => &["Bold"],
            1 => &["Italic", "Oblique"],
            _ => &["BoldItalic", "BoldOblique"],
        };
        names.iter().find_map(|style| {
            let path = search_font_dir(parent, &format!("{family}{style}"))?;
            let face = load(&path).ok()?;
            let attributes = FontRef::from_index(&face.bytes, face.index)?.attributes();
            let bold = attributes.weight().0 >= 600;
            let italic = attributes.style() != swash::Style::Normal;
            (bold == (slot != 1) && italic == (slot != 0)).then_some(face)
        })
    })
}

pub(super) fn family_stem(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    for suffix in [
        "BoldItalic",
        "BoldOblique",
        "Regular",
        "Bold",
        "Italic",
        "Oblique",
    ] {
        if stem.to_lowercase().ends_with(&suffix.to_lowercase()) {
            return stem[..stem.len() - suffix.len()]
                .trim_end_matches(['-', '_'])
                .to_owned();
        }
    }
    stem.into_owned()
}

pub(super) fn covers(face: FontRef<'_>, text: &str) -> bool {
    text.chars().all(|ch| {
        // These controls affect shaping but deliberately have no glyph of
        // their own. Requiring them in cmap rejects valid emoji ligatures.
        matches!(ch, '\u{200c}' | '\u{200d}' | '\u{fe00}'..='\u{fe0f}'
            | '\u{e0020}'..='\u{e007f}' | '\u{e0100}'..='\u{e01ef}')
            || face.charmap().map(ch) != 0
    })
}

pub(super) fn has_color(face: FontRef<'_>) -> bool {
    face.color_strikes().next().is_some() || face.color_palettes().next().is_some()
}
