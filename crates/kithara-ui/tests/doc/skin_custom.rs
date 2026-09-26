use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    ids::SourceUri,
    skin::{ColorRole, SettingDoc, SkinDoc, parse_skin_over},
};

fn origin() -> SourceUri {
    SourceUri("skins/test.kskin.ron".to_owned())
}

/// A skin over the built-in one that dresses extensions in `custom`, written
/// the way a skin file writes its table.
fn dressed_over(base: &SkinDoc, custom: &str) -> SkinDoc {
    let text =
        format!(r#"(schema: "kithara.skin", version: 1, id: "dressed", custom: {{ {custom} }})"#);
    parse_skin_over(base, &text, &origin())
        .unwrap_or_else(|error| panic!("`{custom}` must dress a skin: {error}"))
}

fn setting<'a>(skin: &'a SkinDoc, kind: &str, name: &str) -> &'a SettingDoc {
    &skin.custom.kinds[kind].settings[name]
}

#[kithara::test]
fn a_restated_setting_replaces_the_one_it_names() {
    let first = dressed_over(
        builtin::skin_doc(),
        r#""lsq.wheel": { "ring": Role(Accent) }"#,
    );

    let restated = dressed_over(&first, r#""lsq.wheel": { "ring": Role(Danger) }"#);

    assert_eq!(
        setting(&restated, "lsq.wheel", "ring"),
        &SettingDoc::Role(ColorRole::Danger)
    );
}

#[kithara::test]
fn a_restated_setting_keeps_the_ones_beside_it() {
    let first = dressed_over(
        builtin::skin_doc(),
        r#""lsq.wheel": { "ring": Role(Accent), "spokes": Number(12.0) }"#,
    );

    let restated = dressed_over(&first, r#""lsq.wheel": { "ring": Role(Danger) }"#);

    assert_eq!(
        setting(&restated, "lsq.wheel", "spokes"),
        &SettingDoc::Number(12.0)
    );
}

#[kithara::test]
fn a_patch_dressing_another_kind_leaves_the_first_alone() {
    let first = dressed_over(
        builtin::skin_doc(),
        r#""lsq.wheel": { "ring": Number(3.0) }"#,
    );

    let restated = dressed_over(&first, r#""lsq.deck": { "ring": Number(9.0) }"#);

    assert_eq!(
        setting(&restated, "lsq.wheel", "ring"),
        &SettingDoc::Number(3.0)
    );
    assert_eq!(
        setting(&restated, "lsq.deck", "ring"),
        &SettingDoc::Number(9.0)
    );
}

#[kithara::test]
fn a_colour_that_is_not_one_is_refused_where_it_was_written() {
    let text = r#"(schema: "kithara.skin", version: 1, id: "dressed",
        custom: { "lsq.wheel": { "ring": Color("teal") } })"#;

    let error = parse_skin_over(builtin::skin_doc(), text, &origin())
        .expect_err("a colour that is not written as one must be refused");

    assert!(
        format!("{error}").contains("teal"),
        "the error must name the value that is not a colour: {error}"
    );
}
