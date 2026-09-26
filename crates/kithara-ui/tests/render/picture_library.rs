//! The pictures a skin carries, asked of the skin that carries them.
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    draw::Image,
    error::UiDocError,
    ids::SourceUri,
    render::Skin,
    shaping::FontPolicy,
    skin::{SkinDoc, parse_skin_over},
};

fn origin() -> SourceUri {
    SourceUri("pictured.kskin.ron".to_owned())
}

/// A skin over the built-in one naming one spinner, cut into `columns`.
fn pictured_over(origin: &SourceUri, source: &str, columns: u32) -> SkinDoc {
    let text = format!(
        r#"(schema: "kithara.skin", version: 1, id: "pictured",
            pictures: {{ "spinner": (source: "{source}", columns: {columns}, rows: 1) }})"#
    );
    parse_skin_over(builtin::skin_doc(), &text, origin)
        .unwrap_or_else(|error| panic!("the picture patch must parse: {error}"))
}

fn resolved(source: &str, columns: u32) -> Result<Skin, UiDocError> {
    Skin::resolve_with_font_policy(
        pictured_over(&origin(), source, columns),
        builtin::text_doc(),
        &origin(),
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
}

fn spinner_skin() -> Skin {
    resolved("sprites/spinner.png", 8)
        .unwrap_or_else(|error| panic!("a declared picture must load: {error}"))
}

#[kithara::test]
fn the_skin_answers_the_name_it_declared() {
    assert_eq!(
        spinner_skin().sheet("spinner").map(|sheet| sheet.len()),
        Some(8)
    );
}

/// A document naming a picture the skin does not carry draws nothing,
/// rather than standing in for it with one it did not ask for.
#[kithara::test]
fn a_name_the_skin_carries_nothing_for_answers_nothing() {
    assert!(spinner_skin().sheet("no-such-picture").is_none());
}

/// The picture is cut once and shared: a frame drawn on one screen and the
/// same frame drawn on the next are one picture to whatever uploads it.
#[kithara::test]
fn asking_twice_gives_back_the_same_cut() {
    let skin = spinner_skin();
    let (first, again) = (skin.sheet("spinner"), skin.sheet("spinner"));

    assert_eq!(
        first.map(|sheet| std::ptr::from_ref(sheet.as_ref())),
        again.map(|sheet| std::ptr::from_ref(sheet.as_ref()))
    );
}

/// A skin that names a picture nothing answers is a broken skin, not a
/// skin with one drawing fewer.
#[kithara::test]
fn a_picture_the_resolver_cannot_answer_is_an_error() {
    let error = resolved("sprites/missing.png", 8)
        .err()
        .unwrap_or_else(|| panic!("a missing picture must be refused"));

    assert!(matches!(error, UiDocError::NotFound { .. }), "{error:?}");
}

/// A grid the file does not divide is caught while the skin resolves,
/// rather than showing torn frames at every draw.
#[kithara::test]
fn a_grid_the_picture_does_not_divide_is_an_error() {
    let error = resolved("sprites/spinner.png", 7)
        .err()
        .unwrap_or_else(|| panic!("a grid the picture does not divide must be refused"));

    assert!(matches!(error, UiDocError::Picture { .. }), "{error:?}");
}

/// The picture is named beside the skin that declares it, so a skin in a
/// directory of its own reaches the picture beside it and nothing else.
#[kithara::test]
fn a_picture_is_named_beside_the_skin_that_declares_it() {
    let document = pictured_over(
        &SourceUri("skins/dark.kskin.ron".to_owned()),
        "sprites/spinner.png",
        8,
    );

    assert_eq!(
        document.pictures.sheets["spinner"].source,
        "skins/sprites/spinner.png"
    );
}

fn worn(id: &str) -> &'static Skin {
    builtin::skins()
        .iter()
        .find(|skin| skin.id() == id)
        .unwrap_or_else(|| panic!("the toolkit ships a skin called {id:?}"))
}

fn first_frame(id: &str) -> Option<Vec<u8>> {
    worn(id)
        .sheet("spinner")?
        .frame(0)
        .and_then(Image::rgba)
        .map(|pixels| pixels.to_vec())
}

/// The whole point of a skin carrying pictures: one document naming one
/// picture draws two different ones under two skins.
#[kithara::test]
fn two_skins_answer_one_name_with_two_pictures() {
    assert_ne!(first_frame("kithara-dark"), first_frame("kithara-neon"));
}

/// A skin that restates no picture keeps the ones it is written over,
/// on the same terms as every colour it leaves alone.
#[kithara::test]
fn a_skin_restating_no_picture_keeps_the_ones_it_inherits() {
    assert_eq!(first_frame("kithara-dark"), first_frame("kithara-light"));
}
