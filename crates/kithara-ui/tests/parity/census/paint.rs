use kithara_test_utils::kithara;
use kithara_ui::{
    app::Frame, builtin, ids::SourceUri, render::Skin, shaping::FontPolicy, skin::parse_skin_over,
};

use super::{
    fixture::{Fixture, skin},
    table::{CENSUS_KIND, CONTROL_CENSUS, Paints},
};

/// What the retained host draws for one document, under one skin.
fn drawn(control: &str, skin: &Skin) -> Frame {
    let fixture = Fixture::new(control);
    fixture
        .mount(skin)
        .render()
        .unwrap_or_else(|error| panic!("`{control}` must reach a Masonry paint pass: {error}"))
}

/// Mounts one control on its own and asks Masonry to draw it. A document that
/// holds nothing else has nothing else to contribute, so an empty scene means
/// that control drew nothing.
#[kithara::test]
fn masonry_draws_every_control_the_census_claims_it_draws() {
    let skin = skin();
    let observed = CONTROL_CENSUS
        .iter()
        .map(|(name, _, control)| {
            let frame = drawn(control, &skin);
            let encoding = frame.scene().encoding();
            let paints = if !(encoding.is_empty() && encoding.resources.glyphs.is_empty()) {
                Paints::Yes
            } else if !frame.vis().is_empty() {
                Paints::Native
            } else {
                Paints::Nothing
            };
            (*name, paints)
        })
        .collect::<Vec<_>>();
    let expected = CONTROL_CENSUS
        .iter()
        .map(|(name, paints, _)| (*name, *paints))
        .collect::<Vec<_>>();

    assert_eq!(
        observed, expected,
        "the census is stale — move a row when its painter lands, and never leave the census \
         describing a host it no longer matches"
    );
}

/// A skin dressing the census extension in one named colour, so what an
/// extension is drawn in can be changed without changing the extension.
fn dressed(ink: &str) -> Skin {
    let origin = SourceUri("fixture:masonry-dressed-extension".to_owned());
    let text = format!(
        r#"(schema: "kithara.skin", version: 1, id: "dressed",
            custom: {{ "{CENSUS_KIND}": {{ "ink": Color("{ink}") }} }})"#
    );
    let document = parse_skin_over(builtin::skin_doc(), &text, &origin)
        .unwrap_or_else(|error| panic!("the dressing patch must parse: {error}"));
    Skin::resolve_with_font_policy(
        document,
        builtin::text_doc(),
        &origin,
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("the dressed skin must resolve: {error}"))
}

/// What the retained host draws for a mounted extension under one skin.
fn extension_paint(skin: &Skin) -> Vec<u32> {
    let frame = drawn(
        &format!(r#"Custom(id: "drawn", kind: "{CENSUS_KIND}")"#),
        skin,
    );
    frame.scene().encoding().draw_data.clone()
}

/// The dressing is taken from the skin the leaf was mounted under, so two
/// skins draw one extension two ways without the extension knowing either.
#[kithara::test]
fn a_mounted_extension_is_drawn_in_what_the_skin_dresses_its_kind_in() {
    assert_ne!(
        extension_paint(&dressed("#ff0000")),
        extension_paint(&dressed("#0000ff")),
        "the two skins dress this kind in two colours, so an extension painting the same under \
         both is reading neither"
    );
}
