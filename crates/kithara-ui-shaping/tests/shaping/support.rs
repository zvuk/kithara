use kithara_ui_shaping::{FontFamily, FontWeight, GlyphRun, TextStyle};

pub(crate) fn glyph_ids(run: &GlyphRun) -> Vec<u32> {
    run.segments()
        .iter()
        .flat_map(|segment| segment.glyphs().iter().map(|glyph| glyph.id))
        .collect()
}

pub(crate) const fn style(
    font: FontFamily,
    weight: FontWeight,
    size: f32,
    spacing: f32,
) -> TextStyle {
    TextStyle {
        font,
        weight,
        size,
        spacing,
    }
}

pub(crate) const DISPLAY: TextStyle = style(FontFamily::Display, FontWeight::Normal, 12.0, 0.0);
