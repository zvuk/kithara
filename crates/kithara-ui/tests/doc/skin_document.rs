use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    error::UiDocError,
    ids::{DocId, SourceUri},
    skin::{ColorRole, PopSkin, SkinDoc, load_skin, parse_skin_over},
    source::{Limits, MemResolver},
};

fn gold() -> SkinDoc {
    const GOLD: &str = r##"(
        schema: "kithara.skin",
        version: 1,
        id: "kithara-gold",
        palette: (accent: "#ff0000"),
        button: (icon_size: 42.0),
        )"##;

    parse_skin_over(builtin::skin_doc(), GOLD, &origin())
        .expect("a patch over the builtin skin parses")
}

fn origin() -> SourceUri {
    SourceUri("kithara-gold.kskin.ron".to_owned())
}

#[kithara::test]
fn a_patch_takes_the_color_it_names() {
    assert_eq!(gold().palette.accent, "#ff0000");
}

#[kithara::test]
fn a_patch_keeps_the_colors_it_does_not_name() {
    assert_eq!(gold().palette.bg, builtin::skin_doc().palette.bg);
}

#[kithara::test]
fn a_patch_takes_the_section_field_it_names() {
    assert_eq!(gold().button.icon_size, 42.0);
}

#[kithara::test]
fn a_patch_keeps_the_section_fields_it_does_not_name() {
    assert_eq!(
        gold().button.padding_x,
        builtin::skin_doc().button.padding_x
    );
}

#[kithara::test]
fn a_patch_keeps_the_sections_it_does_not_name() {
    assert_eq!(gold().window, builtin::skin_doc().window);
}

#[kithara::test]
fn a_patch_carries_its_own_identity() {
    assert_eq!(gold().id, DocId("kithara-gold".to_owned()));
}

fn dressing(id: &str, body: &str) -> String {
    format!(
        r##"(
                schema: "kithara.skin",
                version: 1,
                id: "kithara-{id}",
                overrides: {body},
            )"##
    )
}

#[kithara::test]
fn a_skin_carries_the_override_it_names() {
    let text = dressing(
        "dressed",
        r#"{"deck.gain": (fader: (rail_filled: Danger))}"#,
    );

    let document =
        parse_skin_over(builtin::skin_doc(), &text, &origin()).expect("the patch parses");

    assert_eq!(
        document.overrides["deck.gain"]
            .fader
            .expect("the override names the fader section")
            .rail_filled,
        Some(ColorRole::Danger)
    );
}

#[kithara::test]
fn a_skin_declares_no_overrides_by_default() {
    assert!(builtin::skin_doc().overrides.is_empty());
}

/// An override is restated whole. A skin written over another that already
/// dresses a control says everything it means about that control, rather
/// than half of it and half of what it inherited.
#[kithara::test]
fn a_patch_replaces_the_override_it_restates() {
    let base = parse_skin_over(
        builtin::skin_doc(),
        &dressing("base", r#"{"deck.gain": (fader: (rail_filled: Danger))}"#),
        &origin(),
    )
    .expect("the base parses");

    let document = parse_skin_over(
        &base,
        &dressing("over", r#"{"deck.gain": (frames: (radius: 0.0))}"#),
        &origin(),
    )
    .expect("the patch over it parses");

    assert_eq!(document.overrides["deck.gain"].fader, None);
}

#[kithara::test]
fn a_patch_keeps_the_overrides_it_does_not_name() {
    let base = parse_skin_over(
        builtin::skin_doc(),
        &dressing("base", r#"{"deck.gain": (fader: (rail_filled: Danger))}"#),
        &origin(),
    )
    .expect("the base parses");

    let document = parse_skin_over(
        &base,
        &dressing("over", r#"{"deck.pitch": (frames: (radius: 0.0))}"#),
        &origin(),
    )
    .expect("the patch over it parses");

    assert_eq!(
        document.overrides["deck.gain"]
            .fader
            .expect("the inherited override is still there")
            .rail_filled,
        Some(ColorRole::Danger)
    );
}

fn chain(links: &[(&str, &str)]) -> MemResolver {
    let mut resolver = builtin::resolver();
    for (path, text) in links {
        resolver.insert(path, text);
    }
    resolver
}

fn over(base: &str, accent: &str) -> String {
    format!(
        r##"(
                schema: "kithara.skin",
                version: 1,
                id: "kithara-{accent}",
                base: "{base}",
                palette: (accent: "{accent}"),
            )"##
    )
}

#[kithara::test]
fn a_skin_takes_the_color_it_writes_over_its_base() {
    let gold = over(builtin::DARK_SKIN_PATH, "#ff0000");
    let resolver = chain(&[("gold.kskin.ron", &gold)]);

    let document = load_skin(&resolver, "gold.kskin.ron", &Limits::default())
        .expect("a skin over the builtin skin loads");

    assert_eq!(document.palette.accent, "#ff0000");
}

#[kithara::test]
fn a_skin_keeps_what_its_base_declared() {
    let gold = over(builtin::DARK_SKIN_PATH, "#ff0000");
    let resolver = chain(&[("gold.kskin.ron", &gold)]);

    let document = load_skin(&resolver, "gold.kskin.ron", &Limits::default())
        .expect("a skin over the builtin skin loads");

    assert_eq!(document.palette.bg, builtin::skin_doc().palette.bg);
}

#[kithara::test]
fn the_last_skin_in_a_chain_wins_the_color_they_both_name() {
    let gold = over(builtin::DARK_SKIN_PATH, "#ff0000");
    let rose = over("gold.kskin.ron", "#00ff00");
    let resolver = chain(&[("gold.kskin.ron", &gold), ("rose.kskin.ron", &rose)]);

    let document =
        load_skin(&resolver, "rose.kskin.ron", &Limits::default()).expect("a two-link chain loads");

    assert_eq!(document.palette.accent, "#00ff00");
}

#[kithara::test]
fn a_chain_longer_than_the_limit_is_refused() {
    let loop_skin = over("loop.kskin.ron", "#ff0000");
    let resolver = chain(&[("loop.kskin.ron", &loop_skin)]);
    let limits = Limits::builder().max_depth(4).build();

    let error = load_skin(&resolver, "loop.kskin.ron", &limits)
        .expect_err("a skin written over itself cannot resolve");

    assert!(matches!(error, UiDocError::DepthExceeded { max: 4, .. }));
}

#[kithara::test]
fn a_patch_refuses_a_field_no_section_declares() {
    let text = r##"(
            schema: "kithara.skin",
            version: 1,
            id: "kithara-typo",
            button: (icon_sze: 42.0),
        )"##;

    let error = parse_skin_over(builtin::skin_doc(), text, &origin())
        .expect_err("a misspelled field is an error, not a silent default");

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn a_patch_refuses_a_color_it_cannot_read() {
    let text = r##"(
            schema: "kithara.skin",
            version: 1,
            id: "kithara-broken",
            palette: (accent: "not a color"),
        )"##;

    let error = parse_skin_over(builtin::skin_doc(), text, &origin())
        .expect_err("a broken color is an error");

    assert!(matches!(error, UiDocError::BadColor { .. }));
}

#[kithara::test]
fn pop_holds_exactly_the_declared_chrome() {
    let declared: PopSkin = ron::from_str(
        "(background: BgFooter, frame: (radius: 0.0, border_width: 1.0, border: LineHi), \
         cap_height: 2.0, cap_color: Accent, \
         shadow: (color: Shadow, alpha: 0.6, offset_x: 0.0, offset_y: 16.0, blur: 40.0))",
    )
    .unwrap_or_else(|error| panic!("the declared pop chrome must be a pop section: {error}"));

    assert_eq!(builtin::skin_doc().pop, declared);
}
