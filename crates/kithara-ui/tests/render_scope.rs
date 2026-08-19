#![cfg(feature = "render")]

mod common;

use std::{cell::RefCell, collections::BTreeSet};

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{Clock, ReadValue, Reads, tree},
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
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let reads = RecordingReads::default();

    drop(tree::render(
        &ui.root,
        &ui,
        &reads,
        builtin::skin(),
        Clock::default(),
    ));

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

struct FlagReads {
    truthy: BTreeSet<String>,
    seen: RefCell<BTreeSet<String>>,
}

impl FlagReads {
    fn new(truthy: BTreeSet<String>) -> Self {
        Self {
            truthy,
            seen: RefCell::default(),
        }
    }
}

impl Reads for FlagReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        self.seen.borrow_mut().insert(endpoint.to_owned());
        self.truthy
            .contains(endpoint)
            .then_some(ReadValue::Bool(true))
    }
}

fn keys<const N: usize>(endpoints: [&str; N]) -> BTreeSet<String> {
    endpoints.into_iter().map(str::to_owned).collect()
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
        builtin::text_doc(),
        &UiConfig::default(),
    )
}

fn rendered_endpoints(ui: &CompiledUi, truthy: BTreeSet<String>) -> BTreeSet<String> {
    let reads = FlagReads::new(truthy);
    drop(tree::render(
        &ui.root,
        ui,
        &reads,
        builtin::skin(),
        Clock::default(),
    ));
    reads.seen.into_inner()
}

#[kithara::test]
fn a_hidden_block_renders_none_of_the_endpoints_below_it() {
    let ui = block_ui().unwrap();

    let visible = rendered_endpoints(&ui, keys([]));
    let hidden = rendered_endpoints(&ui, keys(["ui.block.hidden"]));

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

fn menu_registry() -> common::TestRegistry {
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.group",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for id in ["ui.menu.toggle", "ui.menu.pick"] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
        );
    }
    registry
}

fn menu_ui(module: &'static str) -> Result<CompiledUi, UiDocError> {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "menu.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "menu",
            root: Module(instance: "shell", source: "menu.kmodule.ron"))"#,
    );
    resolver.insert("menu.kmodule.ron", module);
    compile(
        "menu.klayout.ron",
        &resolver,
        &menu_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
}

const POPOVER_MODULE: &str = r#"(schema: "kithara.module", version: 1, id: "shell",
    root: Popover(
        id: "menu",
        open: Model(id: "ui.menu.open"),
        anchor: Pressable(id: "burger", press: Command(id: "ui.menu.toggle"),
            child: Row(id: "burger-cell", children: [
                Knob(id: "burger-knob", read: Parameter(id: "player.output.volume")),
            ])),
        content: Column(id: "pop", children: [
            Knob(id: "zoom", read: Model(id: "deck.view.zoom")),
            Knob(id: "group", read: Model(id: "ui.menu.group")),
        ]),
    ))"#;

#[kithara::test]
fn a_closed_popover_renders_none_of_its_content_and_an_open_one_renders_all_of_it() {
    let ui = menu_ui(POPOVER_MODULE).unwrap();

    let closed = rendered_endpoints(&ui, keys([]));
    let open = rendered_endpoints(&ui, keys(["ui.menu.open"]));

    for endpoint in ["deck.view.zoom", "ui.menu.group"] {
        assert!(
            !closed.contains(endpoint),
            "a closed popover renders nothing below its content: {closed:?}"
        );
        assert!(
            open.contains(endpoint),
            "an open popover renders every endpoint below its content: {open:?}"
        );
    }
    for seen in [&closed, &open] {
        assert!(
            seen.contains("player.output.volume"),
            "the anchor renders in both states: {seen:?}"
        );
        assert!(
            seen.contains("ui.menu.open"),
            "the open flag is read in both states: {seen:?}"
        );
    }
}

const PRESSABLE_MODULE: &str = r#"(schema: "kithara.module", version: 1, id: "shell",
    root: Pressable(id: "row", press: Command(id: "ui.menu.toggle"),
        child: Row(children: [
            Button(id: "play", label: "PLAY",
                read: Telemetry(id: "deck.playback.playing", with: {"deck": "a"}),
                write: Command(id: "deck.transport.toggle_play", with: {"deck": "a"})),
            Pressable(id: "cell", press: Command(id: "ui.menu.pick"),
                child: Row(children: [
                    Knob(id: "zoom", read: Model(id: "deck.view.zoom")),
                ])),
        ])))"#;

#[kithara::test]
fn a_pressable_keeps_every_control_below_it_live() {
    let ui = menu_ui(PRESSABLE_MODULE).unwrap();

    let seen = rendered_endpoints(&ui, keys([]));

    assert!(
        seen.contains("deck.playback.playing@deck=a"),
        "a button inside a pressed row keeps its own read: {seen:?}"
    );
    assert!(
        seen.contains("deck.view.zoom"),
        "a nested pressable keeps the subtree below it: {seen:?}"
    );
}
