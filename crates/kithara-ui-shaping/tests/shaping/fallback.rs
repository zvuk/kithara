use kithara_test_utils::kithara;
use kithara_ui_shaping::{FontId, FontPolicy, GlyphFace, TextContext, TextResources};

use crate::support::{DISPLAY, glyph_ids};

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
fn display_cyrillic_uses_embedded_fallback_under_both_policies() {
    for policy in [FontPolicy::Embedded, FontPolicy::System] {
        let run = TextContext::from(&TextResources::new(policy).unwrap()).shape(
            WORDS.cyrillic,
            DISPLAY,
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

#[kithara::test]
fn the_harness_policy_reaches_no_face_outside_the_catalog() {
    let mut harness = TextContext::from(&TextResources::new(FontPolicy::Embedded).unwrap());

    for content in WORDS.outside_the_catalog {
        let run = harness.shape(content, DISPLAY, None);

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
    let mut production = TextContext::from(&TextResources::new(FontPolicy::System).unwrap());
    let mut resolved = Vec::new();

    for content in WORDS.outside_the_catalog {
        let run = production.shape(content, DISPLAY, None);
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
    let run = TextContext::new()
        .unwrap()
        .shape(&format!("{} Mix", WORDS.cyrillic), DISPLAY, None);
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
fn display_greek_fallback_has_no_notdef_glyphs() {
    let run = TextContext::new().unwrap().shape("Ελληνικά", DISPLAY, None);
    let [segment] = run.segments() else {
        panic!("Display Greek must shape as one fallback segment");
    };

    assert_eq!(segment.face(), &GlyphFace::Embedded(FontId::InterRegular));
    assert!(!segment.glyphs().is_empty());
    assert!(segment.glyphs().iter().all(|glyph| glyph.id != 0));
}
