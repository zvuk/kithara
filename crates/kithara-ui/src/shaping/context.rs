use std::borrow::Cow;

#[cfg(feature = "render")]
use num_traits::cast::AsPrimitive;
#[cfg(feature = "render")]
use parley::layout::{Affinity, Cursor};
use parley::{
    FontContext, LayoutContext, PositionedLayoutItem, StyleProperty,
    fontique::SourceCache,
    layout::Layout,
    style::{FontFamily as ParleyFamily, FontWeight as ParleyWeight},
};
#[cfg(feature = "render")]
use unicode_segmentation::UnicodeSegmentation;

use super::{
    FontId, FontPolicy, Glyph, GlyphRun, GlyphSegment, TextError, TextResources,
    resources::FaceBlobs, select,
};
use crate::skin::{FontWeight, TextRoleSkin};

/// Owns a policy-selected font collection and Parley shaping scratch space.
pub struct TextContext {
    faces: FaceBlobs,
    fonts: FontContext,
    layout: LayoutContext<()>,
}

#[derive(Clone, Copy)]
struct FaceStyle {
    font: FontId,
    size: f32,
    spacing: f32,
    weight: FontWeight,
}

impl TextContext {
    /// Creates a text context containing only `kithara-ui`'s embedded faces.
    ///
    /// # Errors
    ///
    /// Returns [`TextError`] when a compile-time embedded face is invalid.
    pub fn new() -> Result<Self, TextError> {
        Ok(Self::from(&TextResources::new(FontPolicy::Embedded)?))
    }

    #[cfg(test)]
    fn family_names(&mut self) -> Vec<String> {
        self.fonts
            .collection
            .family_names()
            .map(ToOwned::to_owned)
            .collect()
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
    /// Shapes and measures text with embedded primary faces and configured fallbacks.
    ///
    /// `max_width` is `None` for an unbounded line or `Some(width)` for line
    /// breaking. The role travels whole rather than as a face and a size,
    /// because `spacing` is letter tracking: a signature that took those loose
    /// let a caller shape text and drop the tracking the skin declared, which
    /// is what every string rendered through iced did.
    #[must_use]
    pub fn shape(&mut self, content: &str, role: TextRoleSkin, max_width: Option<f32>) -> GlyphRun {
        self.shape_run(
            content,
            FaceStyle {
                font: select(role.font, role.weight),
                size: role.size,
                spacing: role.spacing,
                weight: role.weight,
            },
            max_width,
        )
    }

    #[cfg(feature = "render")]
    pub(crate) fn shape_lucide(&mut self, content: &str, size: f32) -> GlyphRun {
        self.shape_run(
            content,
            FaceStyle {
                font: FontId::Lucide,
                size,
                spacing: 0.0,
                weight: FontWeight::Normal,
            },
            None,
        )
    }

    #[cfg(feature = "render")]
    pub(crate) fn shape_input(
        &mut self,
        content: &str,
        role: TextRoleSkin,
    ) -> (GlyphRun, Vec<(usize, f32)>) {
        let style = FaceStyle {
            font: select(role.font, role.weight),
            size: role.size,
            spacing: role.spacing,
            weight: role.weight,
        };
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

    fn shape_run(&mut self, content: &str, style: FaceStyle, max_width: Option<f32>) -> GlyphRun {
        let layout = self.build_layout(content, style, max_width);
        self.glyph_run(&layout, style)
    }

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
        let mut segments = Vec::new();
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
}

const fn parley_weight(weight: FontWeight) -> ParleyWeight {
    match weight {
        FontWeight::Normal => ParleyWeight::NORMAL,
        FontWeight::Medium => ParleyWeight::MEDIUM,
        FontWeight::Semibold => ParleyWeight::SEMI_BOLD,
        FontWeight::Bold => ParleyWeight::BOLD,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        shaping::GlyphFace,
        skin::{ColorRole, FontFamily},
    };

    fn context(policy: FontPolicy) -> TextContext {
        TextContext::from(&TextResources::new(policy).unwrap())
    }

    fn glyph_ids(run: &GlyphRun) -> Vec<u32> {
        run.segments()
            .iter()
            .flat_map(|segment| segment.glyphs().iter().map(|glyph| glyph.id))
            .collect()
    }

    fn role(family: FontFamily, weight: FontWeight, size: f32, spacing: f32) -> TextRoleSkin {
        TextRoleSkin {
            color: ColorRole::Text,
            font: family,
            size,
            spacing,
            weight,
        }
    }

    #[kithara::test]
    fn context_registers_only_embedded_families() {
        let mut context = context(FontPolicy::Embedded);
        let mut families = context.family_names();
        families.sort();

        assert_eq!(
            families,
            ["Inter", "JetBrains Mono", "Space Grotesk", "lucide"],
            "the harness policy must register the ten embedded faces and exclude machine-owned families"
        );
        assert_eq!(
            FontId::ALL,
            [
                FontId::InterRegular,
                FontId::InterSemibold,
                FontId::JetBrainsMonoRegular,
                FontId::JetBrainsMonoMedium,
                FontId::JetBrainsMonoSemibold,
                FontId::SpaceGroteskRegular,
                FontId::SpaceGroteskMedium,
                FontId::SpaceGroteskSemibold,
                FontId::SpaceGroteskBold,
                FontId::Lucide,
            ],
            "all ten registered embedded faces are named by the catalog contract"
        );
    }

    /// The words are written as escapes rather than as letters: this file is
    /// checked for non-English text, and what the test needs is the codepoints,
    /// not the glyphs in the source.
    #[kithara::test]
    fn display_cyrillic_uses_embedded_fallback_under_both_policies() {
        for policy in [FontPolicy::Embedded, FontPolicy::System] {
            let run = context(policy).shape(
                WORDS.cyrillic,
                role(FontFamily::Display, FontWeight::Normal, 12.0, 0.0),
                None,
            );
            let [segment] = run.segments() else {
                panic!("Display Cyrillic must shape as one fallback segment under {policy:?}");
            };

            assert_eq!(
                segment.face(),
                &GlyphFace::Embedded(FontId::InterRegular),
                "the registered embedded fallback must win under {policy:?}"
            );
            assert_eq!(glyph_ids(&run), [2437, 848, 641, 1264]);
        }
    }

    /// The words these tests shape, spelled in escapes: this file is checked
    /// for non-English text, and what a fallback test needs is the codepoints
    /// rather than the letters.
    struct Words {
        /// A script the display face does not carry, so shaping falls back to
        /// an embedded face that does.
        cyrillic: &'static str,
        /// Scripts no embedded face covers. Which of them a given machine can
        /// answer is the machine's business; that the production policy reaches
        /// *some* of them, and the harness policy reaches none, is the
        /// contract.
        outside_the_catalog: [&'static str; 5],
    }

    const WORDS: Words = Words {
        cyrillic: "\u{0422}\u{0440}\u{0435}\u{043a}",
        outside_the_catalog: [
            "\u{66f2}\u{540d}",
            "\u{5e0}\u{5dc}\u{5d5}\u{5dd}",
            "\u{645}\u{631}\u{62d}\u{628}\u{627}",
            "\u{c81c}\u{baa9}",
            "\u{e0a}\u{e37}\u{e48}\u{e2d}",
        ],
    };

    #[kithara::test]
    fn the_harness_policy_reaches_no_face_outside_the_catalog() {
        let mut harness = context(FontPolicy::Embedded);

        for content in WORDS.outside_the_catalog {
            let run = harness.shape(
                content,
                role(FontFamily::Display, FontWeight::Normal, 12.0, 0.0),
                None,
            );

            assert!(
                glyph_ids(&run).iter().all(|glyph| *glyph == 0),
                "{content:?} must stay .notdef under the harness policy"
            );
            assert!(
                run.segments()
                    .iter()
                    .all(|segment| matches!(segment.face(), GlyphFace::Embedded(_))),
                "the harness policy must never name a machine-owned face"
            );
        }
    }

    #[kithara::test]
    fn the_production_policy_reaches_faces_the_catalog_does_not_own() {
        let mut production = context(FontPolicy::System);
        let mut resolved = Vec::new();

        for content in WORDS.outside_the_catalog {
            let run = production.shape(
                content,
                role(FontFamily::Display, FontWeight::Normal, 12.0, 0.0),
                None,
            );
            let system = run
                .segments()
                .iter()
                .any(|segment| matches!(segment.face(), GlyphFace::System(_)));

            if system {
                assert!(
                    glyph_ids(&run).iter().all(|glyph| *glyph != 0),
                    "a resolved system face must paint real glyphs, not .notdef: {run:?}"
                );
                resolved.push(content);
            } else {
                assert!(
                    glyph_ids(&run).iter().all(|glyph| *glyph == 0),
                    "a script with no machine candidate must stay .notdef: {run:?}"
                );
            }
        }

        // Without this the test would pass vacuously on a host that resolves
        // nothing, and the system arm would be dead code no one noticed.
        assert!(
            !resolved.is_empty(),
            "a system-backed target must answer at least one script the catalog cannot: \
             none of {:?} resolved",
            WORDS.outside_the_catalog
        );
    }

    #[kithara::test]
    fn display_mixed_script_preserves_visual_segment_order() {
        let run = TextContext::new().unwrap().shape(
            &format!("{} Mix", WORDS.cyrillic),
            role(FontFamily::Display, FontWeight::Normal, 12.0, 0.0),
            None,
        );
        let segments = run.segments();

        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments[0].face(),
            &GlyphFace::Embedded(FontId::InterRegular)
        );
        assert_eq!(
            segments[1].face(),
            &GlyphFace::Embedded(FontId::SpaceGroteskRegular)
        );
        assert_eq!(
            segments[2].face(),
            &GlyphFace::Embedded(FontId::SpaceGroteskRegular)
        );
        assert_eq!(
            segments[0]
                .glyphs()
                .iter()
                .map(|glyph| glyph.id)
                .collect::<Vec<_>>(),
            [2437, 848, 641, 1264]
        );
    }

    #[kithara::test]
    fn display_latin_uses_one_space_grotesk_segment() {
        let run = TextContext::new().unwrap().shape(
            "Track",
            role(FontFamily::Display, FontWeight::Normal, 12.0, 0.0),
            None,
        );
        let [segment] = run.segments() else {
            panic!("Display Latin must stay in one primary-face segment");
        };

        assert_eq!(
            segment.face(),
            &GlyphFace::Embedded(FontId::SpaceGroteskRegular)
        );
        assert!(segment.glyphs().iter().all(|glyph| glyph.id != 0));
    }

    #[kithara::test]
    fn display_greek_fallback_has_no_notdef_glyphs() {
        let run = TextContext::new().unwrap().shape(
            "Ελληνικά",
            role(FontFamily::Display, FontWeight::Normal, 12.0, 0.0),
            None,
        );
        let [segment] = run.segments() else {
            panic!("Display Greek must shape as one fallback segment");
        };

        assert_eq!(segment.face(), &GlyphFace::Embedded(FontId::InterRegular));
        assert!(!segment.glyphs().is_empty());
        assert!(segment.glyphs().iter().all(|glyph| glyph.id != 0));
    }

    #[kithara::test]
    fn shape_returns_positioned_glyphs_and_measurement() {
        let run = TextContext::new().unwrap().shape(
            "GAIN",
            role(FontFamily::Sans, FontWeight::Semibold, 12.0, 0.0),
            None,
        );

        let [segment] = run.segments() else {
            panic!("Latin in the Sans family must stay in one primary-face segment");
        };

        assert_eq!(segment.face(), &GlyphFace::Embedded(FontId::InterSemibold));
        assert!(!segment.glyphs().is_empty());
        assert!(
            segment
                .glyphs()
                .iter()
                .all(|glyph| glyph.x.is_finite() && glyph.y.is_finite())
        );
        assert!(run.width() > 0.0);
        assert!(run.height() > 0.0);
    }

    #[cfg(feature = "render")]
    #[kithara::test]
    fn explicit_lucide_face_shapes_an_icon_glyph() {
        let content = char::from(lucide_icons::Icon::Play).to_string();
        let run = TextContext::new().unwrap().shape_lucide(&content, 14.0);

        let [segment] = run.segments() else {
            panic!("an icon glyph must shape as one Lucide segment");
        };

        assert_eq!(segment.face(), &GlyphFace::Embedded(FontId::Lucide));
        assert_eq!(segment.glyphs().len(), 1);
        assert!(run.width() > 0.0);
        assert!(run.height() > 0.0);
    }

    #[kithara::test]
    fn tracking_increases_measured_width() {
        let mut context = TextContext::new().unwrap();
        let plain = context.shape(
            "GAIN",
            role(FontFamily::Sans, FontWeight::Normal, 12.0, 0.0),
            None,
        );
        let tracked = context.shape(
            "GAIN",
            role(FontFamily::Sans, FontWeight::Normal, 12.0, 0.1),
            None,
        );

        assert!(tracked.width() > plain.width());
    }

    #[kithara::test]
    fn max_width_breaks_lines_and_changes_measurement() {
        let mut context = TextContext::new().unwrap();
        let unbounded = context.shape(
            "GAIN GAIN GAIN",
            role(FontFamily::Sans, FontWeight::Normal, 12.0, 0.0),
            None,
        );
        let wrapped = context.shape(
            "GAIN GAIN GAIN",
            role(FontFamily::Sans, FontWeight::Normal, 12.0, 0.0),
            Some(35.0),
        );

        assert!(wrapped.width() <= 35.0);
        assert!(wrapped.height() > unbounded.height());
    }
}
