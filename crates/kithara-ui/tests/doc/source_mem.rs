use kithara_test_utils::kithara;
use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    source::{MemResolver, SourceResolver},
};

#[kithara::test]
fn relative_include_resolves_against_base_dir() {
    let mut resolver = MemResolver::default();
    resolver.insert("modules/deck/transport.kmodule.ron", "x");
    let base = SourceUri("modules/deck.kmodule.ron".into());
    let loaded = resolver
        .load(Some(&base), "deck/transport.kmodule.ron")
        .unwrap();
    assert_eq!(loaded.uri.0, "modules/deck/transport.kmodule.ron");
}

#[kithara::test]
fn parent_escape_is_rejected() {
    let resolver = MemResolver::default();
    let base = SourceUri("modules/deck.kmodule.ron".into());
    let error = resolver.load(Some(&base), "../../etc/passwd").unwrap_err();
    assert!(matches!(error, UiDocError::RootEscape { .. }));
}

#[kithara::test]
fn absolute_path_is_rejected() {
    let resolver = MemResolver::default();
    let error = resolver.load(None, "/etc/passwd").unwrap_err();
    assert!(matches!(error, UiDocError::RootEscape { .. }));
}

#[kithara::test]
fn bytes_resolve_against_the_base_dir_like_text_does() {
    let mut resolver = MemResolver::default();
    resolver.insert_bytes("skins/sprites/spinner.png", &[1, 2, 3]);
    let base = SourceUri("skins/kithara-dark.kskin.ron".into());
    let loaded = resolver.bytes(Some(&base), "sprites/spinner.png").unwrap();

    assert_eq!(loaded.uri.0, "skins/sprites/spinner.png");
}

/// The two doors hold two sets: a name answered as text is not answered as
/// bytes, so a picture cannot be read as a document or the other way round.
#[kithara::test]
fn a_text_source_is_not_answered_as_bytes() {
    let mut resolver = MemResolver::default();
    resolver.insert("kithara-dark.kskin.ron", "(id: \"dark\")");
    let error = resolver.bytes(None, "kithara-dark.kskin.ron").unwrap_err();

    assert!(matches!(error, UiDocError::NotFound { .. }));
}

#[kithara::test]
fn missing_source_is_not_found() {
    let resolver = MemResolver::default();
    let error = resolver.load(None, "nope.ron").unwrap_err();
    assert!(matches!(error, UiDocError::NotFound { .. }));
}
