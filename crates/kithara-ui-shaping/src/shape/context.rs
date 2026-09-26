use std::borrow::Cow;

use num_traits::cast::AsPrimitive;
use parley::{
    FontContext, LayoutContext, PositionedLayoutItem, StyleProperty,
    fontique::SourceCache,
    layout::{Affinity, Cursor, Layout},
    style::{FontFamily as ParleyFamily, FontWeight as ParleyWeight},
};
use unicode_segmentation::UnicodeSegmentation;

use super::resources::FaceBlobs;
use crate::{
    FontId, FontPolicy, FontWeight, Glyph, GlyphRun, GlyphSegment, TextError, TextResources,
    TextStyle,
};

/// Owns a policy-selected font collection and Parley shaping scratch space.
pub struct TextContext {
    faces: FaceBlobs,
    fonts: FontContext,
    layout: LayoutContext<()>,
}

#[derive(Clone, Copy)]
struct FaceStyle {
    font: FontId,
    weight: FontWeight,
    size: f32,
    spacing: f32,
}

impl From<TextStyle> for FaceStyle {
    fn from(style: TextStyle) -> Self {
        Self {
            font: FontId::select(style.font, style.weight),
            size: style.size,
            spacing: style.spacing,
            weight: style.weight,
        }
    }
}

impl TextContext {
    /// Creates a text context containing only the embedded faces.
    ///
    /// # Errors
    ///
    /// Returns [`TextError`] when a compile-time embedded face is invalid.
    pub fn new() -> Result<Self, TextError> {
        Ok(Self::from(&TextResources::new(FontPolicy::Embedded)?))
    }
}

impl From<&TextResources> for TextContext {
    fn from(resources: &TextResources) -> Self {
        Self {
            faces: resources.faces(),
            fonts: FontContext {
                collection: resources.collection(),
                source_cache: SourceCache::default(),
            },
            layout: LayoutContext::new(),
        }
    }
}

impl TextContext {
    fn build_layout(
        &mut self,
        content: &str,
        style: FaceStyle,
        max_width: Option<f32>,
    ) -> Layout<()> {
        let mut builder = self
            .layout
            .ranged_builder(&mut self.fonts, content, 1.0, false);
        builder.push_default(ParleyFamily::Named(Cow::Borrowed(style.font.family_name())));
        builder.push_default(StyleProperty::FontWeight(parley_weight(style.weight)));
        builder.push_default(StyleProperty::FontSize(style.size));
        builder.push_default(StyleProperty::LetterSpacing(style.spacing * style.size));
        let mut layout = builder.build(content);
        layout.break_all_lines(max_width);
        layout
    }

    fn glyph_run(&self, layout: &Layout<()>, style: FaceStyle) -> GlyphRun {
        let mut segments: Vec<GlyphSegment> = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let face = self.faces.resolve(run.run().font());
                let normalized_coords = run.run().normalized_coords().to_vec();
                let glyphs = run
                    .positioned_glyphs()
                    .map(|glyph| Glyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    })
                    .collect();
                segments.push(GlyphSegment::new(face, normalized_coords, glyphs));
            }
        }
        GlyphRun::new(segments, layout.height(), style.size, layout.width())
    }

    /// Shapes and measures text with embedded primary faces and configured fallbacks.
    ///
    /// `max_width` is `None` for an unbounded line or `Some(width)` for line
    /// breaking.
    #[must_use]
    pub fn shape<S: Into<TextStyle>>(
        &mut self,
        content: &str,
        style: S,
        max_width: Option<f32>,
    ) -> GlyphRun {
        self.shape_run(content, FaceStyle::from(style.into()), max_width)
    }

    /// Shapes one editable line and returns the caret offset of every
    /// grapheme boundary, the end of the text included.
    #[must_use]
    pub fn shape_input<S: Into<TextStyle>>(
        &mut self,
        content: &str,
        style: S,
    ) -> (GlyphRun, Vec<(usize, f32)>) {
        let style = FaceStyle::from(style.into());
        let layout = self.build_layout(content, style, None);
        let carets = content
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(content.len()))
            .map(|index| {
                let cursor = Cursor::from_byte_index(&layout, index, Affinity::Downstream);
                (
                    index,
                    AsPrimitive::<f32>::as_(cursor.geometry(&layout, 1.0).x0),
                )
            })
            .collect();
        (self.glyph_run(&layout, style), carets)
    }

    /// Shapes Lucide icon codepoints at `size`.
    #[must_use]
    pub fn shape_lucide(&mut self, content: &str, size: f32) -> GlyphRun {
        self.shape_run(
            content,
            FaceStyle {
                size,
                font: FontId::Lucide,
                spacing: 0.0,
                weight: FontWeight::Normal,
            },
            None,
        )
    }

    fn shape_run(&mut self, content: &str, style: FaceStyle, max_width: Option<f32>) -> GlyphRun {
        let layout = self.build_layout(content, style, max_width);
        self.glyph_run(&layout, style)
    }
}

const fn parley_weight(weight: FontWeight) -> ParleyWeight {
    match weight {
        FontWeight::Normal => ParleyWeight::NORMAL,
        FontWeight::Medium => ParleyWeight::MEDIUM,
        FontWeight::Semibold => ParleyWeight::SEMI_BOLD,
        FontWeight::Bold => ParleyWeight::BOLD,
    }
}
