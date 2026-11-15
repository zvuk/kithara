use std::collections::BTreeMap;

use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;

use crate::{
    FontFamily, FontId, FontPolicy, FontWeight, GlyphFace, TextContext, TextError, TextResources,
    TextStyle,
};

#[kithara::test]
fn context_registers_only_embedded_families() {
    let resources = TextResources::new(FontPolicy::Embedded).unwrap();
    let mut collection = resources.collection();
    let mut families: Vec<String> = collection.family_names().map(ToOwned::to_owned).collect();
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

fn sources(family: FontFamily, faces: &[FontId]) -> BTreeMap<FontFamily, Vec<Arc<[u8]>>> {
    BTreeMap::from([(
        family,
        faces.iter().map(|face| Arc::from(face.bytes())).collect(),
    )])
}

fn style(family: FontFamily, weight: FontWeight) -> TextStyle {
    TextStyle {
        font: family,
        weight,
        size: 24.0,
        spacing: 0.0,
    }
}

#[kithara::test]
fn a_skin_family_shapes_cyrillic_from_its_exact_registered_bytes() {
    let resources = TextResources::with_fonts(
        FontPolicy::Embedded,
        sources(FontFamily::Sans, &[FontId::JetBrainsMonoRegular]),
    )
    .unwrap();
    let mut custom = TextContext::from(&resources);
    let words = "\u{0426}\u{0430}\u{0440}\u{044c} \u{0412}\u{043e}\u{043b}\u{043d}\u{044b}";
    let run = custom.shape(words, style(FontFamily::Sans, FontWeight::Normal), None);
    assert!(!run.segments().is_empty());
    for segment in run.segments() {
        let GlyphFace::System(data) = segment.face() else {
            panic!("the declared source must answer every Cyrillic glyph");
        };
        assert_eq!(data.data.as_ref(), FontId::JetBrainsMonoRegular.bytes());
        assert!(segment.glyphs().iter().all(|glyph| glyph.id != 0));
    }
    let default =
        TextContext::new()
            .unwrap()
            .shape(words, style(FontFamily::Sans, FontWeight::Normal), None);
    assert!(
        default
            .segments()
            .iter()
            .all(|segment| { segment.face() == &GlyphFace::Embedded(FontId::InterRegular) })
    );
    let display = style(FontFamily::Display, FontWeight::Normal);
    assert_eq!(
        custom.shape("Night Loop", display, None),
        TextContext::new()
            .unwrap()
            .shape("Night Loop", display, None)
    );
    let play = char::from(lucide_icons::Icon::Play).to_string();
    assert_eq!(
        custom.shape_lucide(&play, 24.0),
        TextContext::new().unwrap().shape_lucide(&play, 24.0)
    );
}

#[kithara::test]
fn a_skin_family_selects_real_regular_and_bold_sources() {
    let resources = TextResources::with_fonts(
        FontPolicy::Embedded,
        sources(
            FontFamily::Sans,
            &[FontId::SpaceGroteskRegular, FontId::SpaceGroteskBold],
        ),
    )
    .unwrap();
    let mut text = TextContext::from(&resources);
    let mut runs = Vec::new();
    for (weight, font) in [
        (FontWeight::Normal, FontId::SpaceGroteskRegular),
        (FontWeight::Bold, FontId::SpaceGroteskBold),
    ] {
        let run = text.shape("Night Loop", style(FontFamily::Sans, weight), None);
        for segment in run.segments() {
            let GlyphFace::System(data) = segment.face() else {
                panic!("the requested weight must use its declared face");
            };
            assert_eq!(data.data.as_ref(), font.bytes());
        }
        runs.push(run);
    }
    assert_ne!(runs[0].width(), runs[1].width());
}

#[kithara::test]
fn font_bytes_and_role_are_part_of_resource_identity() {
    let make = |family, face| {
        TextResources::with_fonts(FontPolicy::Embedded, sources(family, &[face])).unwrap()
    };
    let first = make(FontFamily::Sans, FontId::InterRegular);
    assert_eq!(first, make(FontFamily::Sans, FontId::InterRegular));
    assert_ne!(first, make(FontFamily::Sans, FontId::InterSemibold));
    assert_ne!(first, make(FontFamily::Mono, FontId::InterRegular));
    assert_eq!(
        TextResources::new(FontPolicy::Embedded).unwrap(),
        TextResources::with_fonts(FontPolicy::Embedded, BTreeMap::new()).unwrap()
    );
}

#[kithara::test]
fn an_empty_family_or_malformed_font_is_an_explicit_error() {
    assert!(matches!(
        TextResources::with_fonts(
            FontPolicy::Embedded,
            BTreeMap::from([(FontFamily::Sans, Vec::new())]),
        ),
        Err(TextError::EmptyFamily {
            family: FontFamily::Sans
        })
    ));
    assert!(matches!(
        TextResources::with_fonts(
            FontPolicy::Embedded,
            BTreeMap::from([(FontFamily::Sans, vec![Arc::from(b"not a font".as_slice())])]),
        ),
        Err(TextError::InvalidSource {
            family: FontFamily::Sans,
            ..
        })
    ));
}
