use kithara_test_utils::kithara;

use super::Skin;
use crate::{
    builtin,
    error::UiDocError,
    ids::SourceUri,
    shaping::{FontId, FontPolicy, GlyphFace, GlyphSegment, TextContext, TextError},
    skin::{FontFamily, parse_skin_over},
};

fn resolve(files: &[(&str, &[u8])]) -> Result<Skin, UiDocError> {
    let origin = SourceUri("theme/skin.ron".into());
    let document = parse_skin_over(
        builtin::skin_doc(),
        r#"(schema: "kithara.skin", version: 1, id: "font-source",
            fonts: {Sans: ["face.ttf"]},
            overrides: {"title": (text_roles: (weight: Bold))})"#,
        &origin,
    )?;
    let mut resolver = builtin::resolver();
    for (path, bytes) in files {
        resolver.insert_bytes(path, bytes);
    }
    Skin::resolve_with_font_policy(
        document,
        builtin::text_doc(),
        &origin,
        &resolver,
        FontPolicy::Embedded,
    )
}

#[kithara::test]
fn a_skin_and_its_control_overrides_share_the_declared_font_bytes() {
    let font = FontId::JetBrainsMonoRegular.bytes();
    let skin = resolve(&[("theme/face.ttf", font)]).unwrap();
    let mut text = TextContext::from(skin.text_resources.as_ref());
    let run = text.shape("\u{0426}\u{0430}\u{0440}\u{044c}", skin.text.body, None);
    for segment in run.segments() {
        let GlyphFace::System(data) = segment.face() else {
            panic!("the resolved skin must shape with its own font");
        };
        assert_eq!(data.data.as_ref(), font);
        assert!(segment.glyphs().iter().all(|glyph| glyph.id != 0));
    }
    assert!(std::ptr::eq(
        skin.text_resources.as_ref(),
        skin.at("title").text_resources.as_ref()
    ));
    let origin = SourceUri("default.ron".into());
    let default = Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        builtin::text_doc(),
        &origin,
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .unwrap();
    assert_eq!(
        TextContext::from(default.text_resources.as_ref()).shape(
            "Night Loop",
            default.text.body,
            None
        ),
        TextContext::new()
            .unwrap()
            .shape("Night Loop", default.text.body, None)
    );
    assert_ne!(skin.text_resources, default.text_resources);
}

#[kithara::test]
fn declared_fonts_fail_on_missing_or_invalid_bytes() {
    assert!(matches!(resolve(&[]), Err(UiDocError::NotFound { .. })));
    assert!(matches!(
        resolve(&[("theme/face.ttf", b"invalid")]),
        Err(UiDocError::Font {
            source: TextError::InvalidSource {
                family: FontFamily::Sans,
                ..
            },
            ..
        })
    ));
}

#[cfg(feature = "vello")]
#[kithara::test]
fn the_painted_glyphs_keep_the_same_font_and_positions_the_skin_measured() {
    use crate::{
        backends::VelloBackend,
        draw::{DrawListBuilder, Rgba, Transform, replay},
        skin::FontWeight,
    };

    let font = FontId::SpaceGroteskBold.bytes();
    let skin = resolve(&[("theme/face.ttf", font)]).unwrap();
    let mut role = skin.text.body;
    role.weight = FontWeight::Bold;
    let run = TextContext::from(skin.text_resources.as_ref()).shape("Night Loop", role, None);
    let mut list = DrawListBuilder::default();
    list.text(
        &run,
        "Night Loop",
        Transform::IDENTITY,
        Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        },
    );
    let mut scene = vello::Scene::new();
    replay(&list.finish(), &mut VelloBackend::new(&mut scene));
    let encoded = &scene.encoding().resources;
    assert!(!encoded.glyph_runs.is_empty());
    assert!(
        encoded
            .glyph_runs
            .iter()
            .all(|painted| painted.font.data.as_ref() == font)
    );
    let positioned: Vec<_> = run
        .segments()
        .iter()
        .flat_map(GlyphSegment::glyphs)
        .collect();
    assert_eq!(encoded.glyphs.len(), positioned.len());
    for (painted, measured) in encoded.glyphs.iter().zip(positioned) {
        assert_eq!(painted.id, measured.id);
        assert_eq!(painted.x, measured.x);
        assert_eq!(painted.y, measured.y);
    }
}
