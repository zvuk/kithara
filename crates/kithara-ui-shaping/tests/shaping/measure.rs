use std::collections::BTreeMap;

use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use kithara_ui_shaping::{
    Elision, FontFamily, FontId, FontPolicy, FontWeight, GlyphFace, TextContext, TextResources,
};

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
fn registered_font_bytes_outlive_resources_and_context() {
    let mut bytes = FontId::JetBrainsMonoRegular.bytes().to_vec();
    bytes.extend_from_slice(b"skin-owned");
    let run = {
        let mut context = {
            let resources = TextResources::with_fonts(
                FontPolicy::Embedded,
                BTreeMap::from([(FontFamily::Sans, vec![Arc::from(bytes.clone())])]),
            )
            .unwrap();
            TextContext::from(&resources)
        };
        context.shape(
            "GAIN",
            style(FontFamily::Sans, FontWeight::Normal, 12.0, 0.0),
            None,
        )
    };

    let [segment] = run.segments() else {
        panic!("the registered Sans source must shape as one segment");
    };
    let GlyphFace::System(font) = segment.face() else {
        panic!("the shaped segment must retain the registered font data");
    };
    assert_eq!(font.data.as_ref(), bytes);
    assert!(segment.glyphs().iter().all(|glyph| glyph.id != 0));
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
fn elided_text_stays_on_one_line_and_fits_measured_width() {
    let mut context = TextContext::new().unwrap();
    let title = "Big Man, Little Dignity (Re: DOM & JD BECK)";
    let full = context.shape(title, DISPLAY, None);
    let width = full.width() / 2.0;
    let (content, run) = context.shape_elided(title, DISPLAY, width, Elision::End);

    assert!(content.ends_with('\u{2026}'));
    assert!(title.starts_with(content.trim_end_matches('\u{2026}')));
    assert!(run.width() <= width);
    assert_eq!(run.height(), full.height());
    let (unchanged, exact) = context.shape_elided(title, DISPLAY, full.width(), Elision::End);
    assert_eq!(unchanged, title);
    assert_eq!(exact, full);
    let (joined, run) =
        context.shape_elided("Title\nArtist\u{2028}Album", DISPLAY, 500.0, Elision::End);
    assert_eq!(joined, "Title Artist Album");
    assert_eq!(run, context.shape("Title Artist Album", DISPLAY, None));
}

#[kithara::test]
fn elision_keeps_combining_graphemes_and_handles_a_tiny_box() {
    let mut context = TextContext::new().unwrap();
    let title = "e\u{301}e\u{301}e\u{301}e\u{301}";
    let width = context.shape("e\u{301}\u{2026}", DISPLAY, None).width();
    let (content, run) = context.shape_elided(title, DISPLAY, width, Elision::End);

    assert_eq!(content, "e\u{301}\u{2026}");
    assert!(run.width() <= width);
    for width in [0.0, 1.0] {
        let (content, run) = context.shape_elided(title, DISPLAY, width, Elision::End);
        assert!(content.is_empty());
        assert_eq!(run.width(), 0.0);
    }
}

#[kithara::test]
fn middle_elision_keeps_both_ends_of_the_line() {
    let mut context = TextContext::new().unwrap();
    let path = "/Users/someone/Library/Application Support/kithara/config.toml";
    let width = context.shape(path, DISPLAY, None).width() / 2.0;
    let (content, run) = context.shape_elided(path, DISPLAY, width, Elision::Middle);

    let (head, tail) = content
        .split_once('\u{2026}')
        .unwrap_or_else(|| panic!("the line carries an ellipsis: {content}"));
    assert!(path.starts_with(head) && path.ends_with(tail));
    assert!(head.len().abs_diff(tail.len()) <= 1, "{content}");
    assert!(run.width() <= width);
    let wider = context.shape(&format!("{head}x\u{2026}x{tail}"), DISPLAY, None);
    assert!(
        wider.width() > width,
        "the line keeps as much as fits: {content}"
    );
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
