use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    envelope::{DocKind, probe},
    error::UiDocError,
    ids::{DocId, SourceUri},
    render::Skin,
    skin::{FontFamily, parse_skin},
    source::SourceResolver,
};

fn origin() -> SourceUri {
    SourceUri("kithara-dark.kskin.ron".to_owned())
}

/// The dark skin, read through the resolver the crate ships it in.
fn dark_skin() -> String {
    builtin::resolver()
        .load(None, builtin::DARK_SKIN_PATH)
        .expect("the shipped folder holds the dark skin")
        .text
}

/// Every section and every field a skin document declares is required, and an
/// unknown one is refused, so a document that parses at all is a complete one.
/// A role added to the palette with no value in the asset fails here rather
/// than reaching a renderer.
#[kithara::test]
fn builtin_skin_parses_every_required_section() {
    let document = parse_skin(&dark_skin(), &origin()).unwrap();

    assert_eq!(document.id, DocId("kithara-dark".to_owned()));
}

/// Every skin the crate ships is loaded through whatever base chain it
/// declares and resolved. A patch naming a field its base does not have, or a
/// colour that does not parse, fails here rather than at the first frame worn
/// in it.
#[kithara::test]
fn every_shipped_skin_resolves_under_its_own_name() {
    let names: Vec<&str> = builtin::skins().iter().map(Skin::id).collect();

    assert_eq!(
        names,
        [
            "kithara-dark",
            "kithara-light",
            "kithara-neon",
            "kithara-soft"
        ]
    );
}

/// A skin written over another restates only what it changes, so the page
/// colour moves and the room every control asks for does not.
#[kithara::test]
fn a_skin_written_over_the_dark_one_keeps_its_measurements() {
    let [dark, light, ..] = builtin::skins() else {
        panic!("the crate must ship a dark skin and a skin written over it")
    };

    assert_ne!(light.palette.bg, dark.palette.bg);
    assert_eq!(light.chrome.header_height, dark.chrome.header_height);
}

/// A skin carries measurements as well as colour, and a patch may restate one
/// field of a section without restating the section.
#[kithara::test]
fn a_skin_may_restate_one_measurement_of_a_section() {
    let [dark, _, neon, ..] = builtin::skins() else {
        panic!("the crate must ship a dark skin and a neon one written over it")
    };

    assert_ne!(neon.nav.item_height, dark.nav.item_height);
    assert_eq!(neon.nav.icon_size, dark.nav.icon_size);
}

/// The soft skin, which is the one written on blanket terms.
fn soft() -> &'static Skin {
    builtin::skins()
        .iter()
        .find(|skin| skin.id() == "kithara-soft")
        .unwrap_or_else(|| panic!("the crate must ship the soft skin"))
}

/// A blanket reaches a frame the skin never names, which is the whole reason
/// to write one: rounding the design must not mean restating every control.
#[kithara::test]
fn a_frame_blanket_reaches_a_frame_the_skin_never_names() {
    assert_eq!(soft().table.badge_frame.radius, 12.0);
}

/// A frame nested inside another value is reached on the same terms, so a
/// blanket cannot leave a hole where a section holds its own structure.
#[kithara::test]
fn a_frame_blanket_reaches_a_frame_nested_in_another_value() {
    assert_eq!(soft().wave.overlay.art_frame.radius, 12.0);
}

/// The blanket is the ground, not the last word: a control the skin states
/// keeps what it states.
#[kithara::test]
fn a_stated_frame_wins_over_the_blanket_around_it() {
    assert_eq!(soft().fader.rail_frame.radius, 3.0);
}

/// One face named once reaches every typographic role, including the ones a
/// section holds rather than the text block.
#[kithara::test]
fn a_role_blanket_reaches_a_role_a_section_holds() {
    assert_eq!(soft().window.titlebar_text.font, FontFamily::Sans);
}

/// A blanket states one part of a role and leaves the rest of it where the
/// base put it, so naming a face does not silently reset every size.
#[kithara::test]
fn a_role_blanket_leaves_the_rest_of_the_role_alone() {
    let [dark, ..] = builtin::skins() else {
        panic!("the crate must ship a dark skin")
    };

    assert_eq!(
        soft().window.titlebar_text.size,
        dark.window.titlebar_text.size
    );
}

/// A role the skin states keeps what it states, on the same terms as a stated
/// frame: the blanket is the ground the document is written on, not its last
/// word.
#[kithara::test]
fn a_stated_role_wins_over_the_blanket_around_it() {
    assert_eq!(soft().text.section.font, FontFamily::Display);
}

#[kithara::test]
fn skin_envelope_is_probed_as_skin() {
    let envelope = probe(&dark_skin(), &origin()).unwrap();

    assert_eq!(envelope.kind, DocKind::Skin);
}

#[kithara::test]
fn unknown_skin_field_is_rejected() {
    let text = dark_skin().replacen(
        "id: \"kithara-dark\",",
        "id: \"kithara-dark\", unknown: 1,",
        1,
    );
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

/// A field left out of any section is refused rather than defaulted, sampled
/// once per kind of section the document is made of.
#[kithara::test]
fn a_required_skin_field_is_rejected_when_missing() {
    for line in [
        "        body_fill: BgSelect,\n",
        "        rail_height: 6.0,\n",
        "        header_height: 26.0,\n",
    ] {
        let text = dark_skin().replacen(line, "", 1);
        let error = parse_skin(&text, &origin()).unwrap_err();

        assert!(
            matches!(error, UiDocError::Syntax { .. }),
            "a skin without `{}` must be refused",
            line.trim()
        );
    }
}

#[kithara::test]
fn malformed_skin_hex_has_typed_error() {
    let text = dark_skin().replacen("#12121f", "#12xx1f", 1);
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::BadColor { origin, value }
            if origin == self::origin() && value == "#12xx1f"
    ));
}
