use std::collections::BTreeMap;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    envelope::{DocKind, probe},
    error::UiDocError,
    ids::{DocId, SourceUri},
    text::parse_text,
};

fn origin() -> SourceUri {
    SourceUri("test.ktext.ron".into())
}

const BUILTIN_ENTRIES: &[(&str, &str)] = &[
    ("deck.transport.play", "PLAY"),
    ("deck.transport.pause", "PAUSE"),
    ("deck.transport.cue", "CUE"),
    ("deck.transport.sync", "SYNC"),
    ("deck.transport.loop", "LOOP 4"),
    ("deck.tempo.caption", "TEMPO"),
    ("deck.stream.hls_label", "HLS"),
    ("deck.stream.quality_title", "HLS QUALITY"),
    ("deck.stream.auto_label", "AUTO"),
    ("deck.stream.auto_sub", "BY NETWORK"),
    ("bar.cpu", "CPU"),
    ("bar.rec", "REC"),
    ("menu.brand", "KITHARA"),
    ("menu.section.windows", "WINDOWS & MODULES"),
    ("menu.layouts", "Saved layouts"),
    ("menu.full_screen", "Full screen"),
    ("menu.section.set", "SET"),
    ("strip.eq.title", "ЭКВАЛАЙЗЕР"),
    ("strip.eq.high", "HIGH"),
    ("strip.eq.hi_mid", "HI-MID"),
    ("strip.eq.lo_mid", "LO-MID"),
    ("strip.eq.mid", "MID"),
    ("strip.eq.low", "LOW"),
    ("track_list.column.index", "#"),
    ("track_list.column.deck", "DECK"),
    ("track_list.column.title", "TITLE"),
    ("track_list.column.artist", "ARTIST"),
    ("track_list.column.bpm", "BPM"),
    ("track_list.column.key", "KEY"),
    ("track_list.column.time", "TIME"),
    ("track_list.column.energy", "ENERGY"),
    ("track_list.column.transition", "TRANSITION"),
    ("track_list.footer_tracks", "TRACKS"),
    (
        "tree.search_placeholder",
        "Search Zvuk: track, artist, release...",
    ),
    ("crossfader.left_label", "A"),
    ("crossfader.center_label", "XFADE"),
    ("crossfader.right_label", "B"),
];

#[kithara::test]
fn parses_a_text_catalog_document() {
    let text = r#"(
        id: "sample",
        schema: "kithara.text",
        version: 1,
        entries: { "deck.transport.play": "PLAY" },
    )"#;
    let doc = parse_text(text, &origin()).unwrap();

    assert_eq!(doc.id, DocId("sample".to_owned()));
    assert_eq!(doc.get("deck.transport.play"), Some("PLAY"));
    assert_eq!(doc.get("nowhere"), None);
}

#[kithara::test]
fn text_envelope_is_probed_as_text() {
    let text = r#"(id: "sample", schema: "kithara.text", version: 1, entries: {})"#;
    let envelope = probe(text, &origin()).unwrap();

    assert_eq!(envelope.kind, DocKind::Text);
}

#[kithara::test]
fn rejects_a_document_of_a_different_kind() {
    let error = parse_text(builtin::DARK_SKIN, &origin()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::WrongDocKind {
            expected: "text",
            found: "skin",
            ..
        }
    ));
}

#[kithara::test]
fn rejects_an_unsupported_version() {
    let text = r#"(id: "sample", schema: "kithara.text", version: 99, entries: {})"#;
    let error = parse_text(text, &origin()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::UnsupportedVersion {
            version: 99,
            max: 1,
            ..
        }
    ));
}

#[kithara::test]
fn every_shipped_catalog_carries_the_same_key_set() {
    let catalogs = [builtin::text_doc()];
    let keys: Vec<_> = catalogs[0].keys().collect();

    for catalog in &catalogs {
        assert_eq!(catalog.keys().collect::<Vec<_>>(), keys);
    }
}

#[kithara::test]
fn builtin_catalog_holds_exactly_the_declared_entries() {
    let expected: BTreeMap<&str, &str> = BUILTIN_ENTRIES.iter().copied().collect();
    let actual: BTreeMap<&str, &str> = builtin::text_doc()
        .keys()
        .map(|key| (key, builtin::text_doc().get(key).unwrap()))
        .collect();

    assert_eq!(actual.len(), expected.len(), "entry count drifted");
    assert_eq!(actual, expected);
}

#[kithara::test]
fn merge_combines_disjoint_catalogs() {
    let base = parse_text(
        r#"(id: "a", schema: "kithara.text", version: 1, entries: { "a.one": "One" })"#,
        &origin(),
    )
    .unwrap();
    let extra = parse_text(
        r#"(id: "b", schema: "kithara.text", version: 1, entries: { "b.one": "Uno" })"#,
        &origin(),
    )
    .unwrap();

    let merged = base.merge(&extra, &origin()).unwrap();

    assert_eq!(merged.get("a.one"), Some("One"));
    assert_eq!(merged.get("b.one"), Some("Uno"));
}

#[kithara::test]
fn merge_rejects_a_key_defined_in_both_catalogs() {
    let base = parse_text(
        r#"(id: "a", schema: "kithara.text", version: 1, entries: { "shared": "One" })"#,
        &origin(),
    )
    .unwrap();
    let extra = parse_text(
        r#"(id: "b", schema: "kithara.text", version: 1, entries: { "shared": "Two" })"#,
        &origin(),
    )
    .unwrap();

    let error = base.merge(&extra, &origin()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::DuplicateTextKey { key, .. } if key == "shared"
    ));
}
