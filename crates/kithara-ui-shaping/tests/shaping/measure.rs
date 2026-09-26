use kithara_test_utils::kithara;
use kithara_ui_shaping::{FontFamily, FontId, FontWeight, GlyphFace, TextContext};

use crate::support::{DISPLAY, style};

#[kithara::test]
fn display_latin_uses_one_space_grotesk_segment() {
    let run = TextContext::new().unwrap().shape("Track", DISPLAY, None);
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
fn shape_returns_positioned_glyphs_and_measurement() {
    let run = TextContext::new().unwrap().shape(
        "GAIN",
        style(FontFamily::Sans, FontWeight::Semibold, 12.0, 0.0),
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
        style(FontFamily::Sans, FontWeight::Normal, 12.0, 0.0),
        None,
    );
    let tracked = context.shape(
        "GAIN",
        style(FontFamily::Sans, FontWeight::Normal, 12.0, 0.1),
        None,
    );

    assert!(tracked.width() > plain.width());
}

#[kithara::test]
fn max_width_breaks_lines_and_changes_measurement() {
    let mut context = TextContext::new().unwrap();
    let sans = style(FontFamily::Sans, FontWeight::Normal, 12.0, 0.0);
    let unbounded = context.shape("GAIN GAIN GAIN", sans, None);
    let wrapped = context.shape("GAIN GAIN GAIN", sans, Some(35.0));

    assert!(wrapped.width() <= 35.0);
    assert!(wrapped.height() > unbounded.height());
}

#[kithara::test]
fn caret_offsets_cover_every_grapheme_boundary() {
    let (run, carets) = TextContext::new().unwrap().shape_input("GAIN", DISPLAY);

    assert_eq!(
        carets.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    assert!(carets.windows(2).all(|pair| pair[0].1 < pair[1].1));
    assert!((carets[4].1 - run.width()).abs() < 0.5);
}
