use kithara_test_utils::kithara;
use kithara_ui::{builtin, render::Skin, skin::ColorRole};

/// The kind the shipped skins dress, which is the gallery's own extension.
const LADDER: &str = "level-ladder";

fn skin(id: &str) -> &'static Skin {
    builtin::skins()
        .iter()
        .find(|skin| skin.id() == id)
        .unwrap_or_else(|| panic!("the toolkit must ship a skin called {id}"))
}

#[kithara::test]
fn the_skin_answers_for_the_kind_it_dresses() {
    assert_eq!(
        skin("kithara-dark").custom(LADDER).number("bars"),
        Some(12.0)
    );
}

#[kithara::test]
fn a_kind_no_skin_names_is_dressed_in_nothing() {
    assert_eq!(
        skin("kithara-dark").custom("nobody.at.all").number("bars"),
        None
    );
}

#[kithara::test]
fn a_number_is_not_answered_as_a_colour() {
    assert_eq!(skin("kithara-dark").custom(LADDER).color("bars"), None);
}

#[kithara::test]
fn a_setting_written_as_a_role_reads_the_palette_of_the_skin_that_answers() {
    let dark = skin("kithara-dark");

    assert_eq!(
        dark.custom(LADDER).color("bar_high"),
        Some(dark.palette[ColorRole::WaveHigh])
    );
}

#[kithara::test]
fn two_skins_dress_one_kind_two_ways() {
    let neon = skin("kithara-neon");

    assert_eq!(
        neon.custom(LADDER).color("bar_high"),
        Some(neon.palette[ColorRole::Accent])
    );
    assert_ne!(
        neon.custom(LADDER).color("bar_high"),
        skin("kithara-dark").custom(LADDER).color("bar_high")
    );
}

#[kithara::test]
fn a_skin_restating_one_setting_keeps_the_ones_beside_it() {
    let neon = skin("kithara-neon");

    assert_eq!(neon.custom(LADDER).number("bars"), Some(8.0));
    assert_eq!(
        neon.custom(LADDER).color("ground"),
        Some(neon.palette[ColorRole::BgInset]),
        "the ground neon never restates is the one it inherits, read through its own palette"
    );
}
