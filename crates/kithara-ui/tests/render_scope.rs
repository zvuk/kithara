#![cfg(feature = "render")]

mod common;

use std::{cell::RefCell, collections::BTreeSet};

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::compile,
    render::{ReadValue, Reads, tree},
    source::UiConfig,
};

/// Records every endpoint the renderer asks for and answers nothing.
#[derive(Default)]
struct RecordingReads(RefCell<BTreeSet<String>>);

impl Reads for RecordingReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        self.0.borrow_mut().insert(endpoint.to_owned());
        None
    }
}

#[kithara::test]
fn rendering_two_decks_reads_scoped_endpoints_for_both() {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "two_deck.klayout.ron",
        include_str!("fixtures/two_deck.klayout.ron"),
    );
    let ui = compile(
        "two_deck.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let reads = RecordingReads::default();

    drop(tree::render(&ui.root, &ui, &reads, builtin::skin()));

    let seen = reads.0.borrow();
    for deck in ["a", "b"] {
        for endpoint in [
            // Read bindings from the builtin deck module.
            "deck.playback.waveform",
            "deck.playback.playing",
            "deck.playback.tempo",
            // Endpoints the wave widget derives from its binding scope.
            "deck.playback.position_normalized",
            "deck.track.title",
        ] {
            let key = format!("{endpoint}@deck={deck}");
            assert!(seen.contains(&key), "missing {key}; saw: {seen:?}");
        }
    }
}
