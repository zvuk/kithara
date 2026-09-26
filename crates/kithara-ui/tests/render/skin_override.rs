//! What a skin dresses one control in, asked of the skin that dresses it.
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    ids::SourceUri,
    render::Skin,
    skin::{ColorRole, parse_skin_over},
};

fn origin() -> SourceUri {
    SourceUri("kithara-dressed.kskin.ron".to_owned())
}

/// A skin that dresses one fader differently from every other.
fn dressed() -> Skin {
    let text = r##"(
        schema: "kithara.skin",
        version: 1,
        id: "kithara-dressed",
        overrides: {
            "deck.gain": (
                frames: (radius: 0.0),
                fader: (rail_filled: Danger),
            ),
        },
    )"##;
    let document = parse_skin_over(builtin::skin_doc(), text, &origin())
        .unwrap_or_else(|error| panic!("the patch parses: {error}"));
    Skin::resolve(
        document,
        builtin::text_doc(),
        &origin(),
        &builtin::resolver(),
    )
    .unwrap_or_else(|error| panic!("the dressed document resolves: {error}"))
}

#[kithara::test]
fn a_control_the_skin_names_wears_what_the_override_restates() {
    let skin = dressed();

    assert_eq!(skin.at("deck.gain").fader.rail_filled, ColorRole::Danger);
}

#[kithara::test]
fn a_control_the_override_does_not_name_wears_the_skin_itself() {
    let skin = dressed();

    assert!(std::ptr::eq(skin.at("deck.pitch"), &skin));
}

#[kithara::test]
fn the_skin_itself_keeps_what_one_control_restated() {
    let skin = dressed();

    assert_eq!(skin.fader.rail_filled, builtin::skin().fader.rail_filled);
}

#[kithara::test]
fn an_override_keeps_the_sections_it_does_not_name() {
    let skin = dressed();

    assert_eq!(skin.at("deck.gain").knob, skin.knob);
}

#[kithara::test]
fn an_override_shares_the_palette_it_was_dressed_from() {
    let skin = dressed();

    assert_eq!(skin.at("deck.gain").palette, skin.palette);
}

/// A blanket inside an override is a blanket over that control alone: it
/// reaches every frame the control's own sections declare, and no other
/// control's.
#[kithara::test]
fn a_blanket_inside_an_override_reaches_that_controls_frames() {
    let skin = dressed();

    assert_eq!(skin.at("deck.gain").fader.handle_frame.radius, 0.0);
}

#[kithara::test]
fn a_blanket_inside_an_override_leaves_every_other_control_alone() {
    let skin = dressed();

    assert_eq!(
        skin.button.frame.radius,
        builtin::skin().button.frame.radius
    );
}
