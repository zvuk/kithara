use std::collections::BTreeMap;

use kithara_test_utils::kithara;

use super::binding::*;
use crate::{
    error::UiDocError,
    ids::{DocId, SourceUri},
    text::TextDoc,
};

fn with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn catalog(pairs: &[(&str, &str)]) -> TextDoc {
    TextDoc {
        id: DocId("test".to_owned()),
        schema: "kithara.text".to_owned(),
        version: 1,
        entries: pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn origin() -> SourceUri {
    SourceUri("test.ron".into())
}

#[kithara::test]
fn a_leading_at_resolves_a_catalog_key() {
    let text = catalog(&[("menu.modules", "Modules")]);
    assert_eq!(
        resolve_text_key(&text, "@menu.modules", &origin(), "node").unwrap(),
        "Modules"
    );
}

#[kithara::test]
fn doubled_at_escapes_to_a_literal_at() {
    let text = catalog(&[]);
    assert_eq!(
        resolve_text_key(&text, "@@handle", &origin(), "node").unwrap(),
        "@handle"
    );
}

#[kithara::test]
fn a_mid_string_at_is_not_a_marker() {
    let text = catalog(&[]);
    assert_eq!(
        resolve_text_key(&text, "user@example.com", &origin(), "node").unwrap(),
        "user@example.com"
    );
}

#[kithara::test]
fn a_value_without_at_passes_through_unchanged() {
    let text = catalog(&[]);
    assert_eq!(
        resolve_text_key(&text, "PLAY", &origin(), "node").unwrap(),
        "PLAY"
    );
}

#[kithara::test]
fn an_unknown_key_is_an_error_carrying_origin_and_path() {
    let text = catalog(&[]);
    let error = resolve_text_key(&text, "@missing", &origin(), "node/path").unwrap_err();
    assert!(matches!(
        error,
        UiDocError::UnknownTextKey { key, path, .. }
            if key == "missing" && path == "node/path"
    ));
}

#[kithara::test]
fn scoped_key_is_the_bare_id_without_scopes() {
    assert_eq!(
        scoped_key("player.output.volume", &BTreeMap::new()),
        "player.output.volume"
    );
}

#[kithara::test]
fn scoped_key_appends_sorted_scope_pairs() {
    assert_eq!(
        scoped_key("deck.playback.playing", &with(&[("deck", "b")])),
        "deck.playback.playing@deck=b"
    );
    assert_eq!(
        scoped_key(
            "deck.playback.playing",
            &with(&[("layer", "2"), ("deck", "a")])
        ),
        "deck.playback.playing@deck=a,layer=2"
    );
}
