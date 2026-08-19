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
    ("strip.eq.title", "EQUALIZER"),
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
    ("table.footer_rows", "ROWS"),
    (
        "tree.search_placeholder",
        "Search Zvuk: track, artist, release...",
    ),
    ("crossfader.left_label", "A"),
    ("crossfader.center_label", "XFADE"),
    ("crossfader.right_label", "B"),
    ("clock.tempo_label", "MASTER BPM"),
    ("clock.title", "CLOCK"),
    ("clock.column.tempo", "TEMPO"),
    ("clock.column.mode", "MODE"),
    ("clock.column.pulse", "PULSE"),
    ("clock.column.stretch", "STRETCH"),
    ("clock.portals_label", "PORTALS"),
    ("clock.family.step_label", "STEP"),
    ("clock.family.leap_label", "LEAP"),
    ("clock.limit_label", "TO"),
    ("clock.tolerance_label", "TOL"),
    ("clock.grid.quantize_label", "GRID START"),
    ("clock.grid.snap_label", "GRID MARKS"),
    ("clock.link.status_label", "LINK"),
    ("clock.link.toggle_label", "SYNC"),
    ("clock.midi.status_label", "MIDI"),
    ("clock.midi.send_label", "SEND"),
    ("clock.tap_label", "TAP"),
    ("clock.reset_label", "RESET"),
    ("pivot.title", "PIVOT PORTAL"),
    ("pivot.master_label", "MASTER"),
    ("pivot.family.ratio_label", "RATIO"),
    ("pivot.family.step_label", "STEP"),
    ("pivot.family.leap_label", "LEAP"),
    ("pivot.range_label", "RANGE"),
    ("pivot.column.ratio", "RATIO"),
    ("pivot.column.bpm", "TEMPO"),
    ("pivot.column.pulse", "PIVOT"),
    ("pivot.column.loop", "LOOP"),
    ("pivot.column.stretch", "INSTEAD OF STRETCH"),
    ("pivot.loops_label", "DECK LOOP"),
    ("pivot.tracks_label", "TRACKS IN COLLECTION AT THIS TEMPO"),
    ("menu.new_window", "New window"),
    ("menu.save_layout", "Save window layout…"),
    ("menu.full_screen_window", "Full screen · active window"),
    ("menu.section.mixing", "MIXING"),
    ("menu.add_folder", "Add folder…"),
    ("menu.settings", "Settings…"),
    ("deck.key_lock.label", "KEY LOCK"),
    ("menu.module.overview", "OVERVIEW"),
    ("menu.module.mixer", "MIXER"),
    ("menu.module.fx1", "FX 1"),
    ("menu.module.fx2", "FX 2"),
    ("menu.module.vcf", "VCF"),
    ("menu.module.rec", "REC"),
    ("menu.module.visual", "VISUAL"),
    ("menu.module.timeline", "TIMELINE"),
    ("menu.module.cpu", "CPU"),
    ("menu.module.net", "NET"),
    ("menu.module.buf", "BUF"),
    ("menu.layout.club", "CLUB · 2 DECKS"),
    ("menu.layout.studio", "STUDIO · 4 DECKS + VST"),
    ("menu.layout.visuals", "VISUALS + TIMELINE"),
    ("menu.layout.narrow", "NARROW WINDOW · TABS"),
    ("menu.toggle.wave_follow", "Waveform follows playhead"),
    ("menu.toggle.autogain", "Per-track autogain"),
    ("menu.toggle.mono", "Mono output"),
    ("menu.toggle.record_set", "Record set"),
    ("menu.broadcast", "Broadcast"),
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
