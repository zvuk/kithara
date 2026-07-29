#![cfg(feature = "render")]

mod common;

use std::{cell::RefCell, collections::BTreeSet};

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
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

/// Records every endpoint the renderer asks for, and hides the one block the
/// fixture declares when asked to.
struct BlockReads {
    seen: RefCell<BTreeSet<String>>,
    hidden: bool,
}

impl BlockReads {
    fn new(hidden: bool) -> Self {
        Self {
            seen: RefCell::default(),
            hidden,
        }
    }
}

impl Reads for BlockReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        self.seen.borrow_mut().insert(endpoint.to_owned());
        (self.hidden && endpoint == "ui.block.hidden").then_some(ReadValue::Bool(true))
    }
}

fn block_ui() -> Result<CompiledUi, UiDocError> {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "blocks.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "blocks",
            root: Module(instance: "mixer", source: "blocks.kmodule.ron"))"#,
    );
    resolver.insert(
        "blocks.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Knob(id: "volume", read: Parameter(id: "player.output.volume")),
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Knob(id: "low", read: Model(id: "deck.view.zoom"))),
            ]))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.block.hidden",
        EndpointDesc::new(ValueKind::Bool),
    );
    compile(
        "blocks.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        &UiConfig::default(),
    )
}

fn rendered_endpoints(ui: &CompiledUi, hidden: bool) -> BTreeSet<String> {
    let reads = BlockReads::new(hidden);
    drop(tree::render(&ui.root, ui, &reads, builtin::skin()));
    reads.seen.into_inner()
}

#[kithara::test]
fn a_hidden_block_renders_none_of_the_endpoints_below_it() {
    let ui = block_ui().unwrap();

    let visible = rendered_endpoints(&ui, false);
    let hidden = rendered_endpoints(&ui, true);

    assert!(
        visible.contains("deck.view.zoom"),
        "the block renders while it is visible: {visible:?}"
    );
    assert!(
        !hidden.contains("deck.view.zoom"),
        "a hidden block renders nothing below it: {hidden:?}"
    );
    assert!(
        hidden.contains("player.output.volume"),
        "its siblings keep rendering: {hidden:?}"
    );
    assert_eq!(
        visible.difference(&hidden).collect::<Vec<_>>(),
        ["deck.view.zoom"].iter().collect::<Vec<_>>(),
        "hiding a block changes nothing else",
    );
}
