#![cfg(feature = "render")]

mod common;

#[path = "common/app_menu_spec.rs"]
mod app_menu_spec;

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

/// Records every endpoint the renderer asks for, and answers `true` for the
/// keys the case drives. A case needs several axes at once, so the true set is
/// a set and not one key.
struct FlagReads {
    seen: RefCell<BTreeSet<String>>,
    truthy: BTreeSet<String>,
}

impl FlagReads {
    fn new(truthy: BTreeSet<String>) -> Self {
        Self {
            seen: RefCell::default(),
            truthy,
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
        &UiConfig::default(),
    )
}

fn rendered_endpoints(ui: &CompiledUi, truthy: BTreeSet<String>) -> BTreeSet<String> {
    let reads = FlagReads::new(truthy);
    drop(tree::render(&ui.root, ui, &reads, builtin::skin()));
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

/// §2.7's axes. Everything else in that table is a tone, which the recorder
/// cannot see and which `RUNS`, `MARKS` and the join tests carry instead.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MenuState {
    open: bool,
    group: Group,
    windows: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Group {
    Win,
    Mod,
    Lay,
    Closed,
}

/// §2.7's default snapshot: the menu open, the window group selected, two
/// windows.
const DEFAULT: MenuState = MenuState {
    open: true,
    group: Group::Win,
    windows: 2,
};

impl MenuState {
    /// §2.7's host predicates, which S8's gallery mock implements too.
    fn truthy(self) -> BTreeSet<String> {
        let mut truthy = BTreeSet::new();
        if self.open {
            truthy.insert("ui.menu.open".to_owned());
        }
        if self.windows < 3 {
            truthy.insert("ui.window.can_open".to_owned());
        }
        for window in 1..=3 {
            if window > self.windows {
                truthy.insert(format!("ui.window.hidden@window={window}"));
            }
            if self.windows == 1 || window == 1 {
                truthy.insert(format!("ui.window.close_hidden@window={window}"));
            }
        }
        if self.group != Group::Mod {
            truthy.insert("ui.menu.group_hidden@group=mod".to_owned());
        }
        if self.group != Group::Lay {
            truthy.insert("ui.menu.group_hidden@group=lay".to_owned());
        }
        truthy
    }
}

/// The keys that live strictly inside one `Optional`. The gate itself sits
/// outside it and is read in every state.
const WINDOW_2: [&str; 4] = [
    "ui.window.active@window=2",
    "ui.window.caption@window=2",
    "ui.window.close_hidden@window=2",
    "ui.window.title@window=2",
];

const WINDOW_3: [&str; 4] = [
    "ui.window.active@window=3",
    "ui.window.caption@window=3",
    "ui.window.close_hidden@window=3",
    "ui.window.title@window=3",
];

const MODULES: [&str; 11] = [
    "ui.module.on@module=ov",
    "ui.module.on@module=mix",
    "ui.module.on@module=fx1",
    "ui.module.on@module=fx2",
    "ui.module.on@module=vcf",
    "ui.module.on@module=rec",
    "ui.module.on@module=vis",
    "ui.module.on@module=tl",
    "ui.module.on@module=cpu",
    "ui.module.on@module=net",
    "ui.module.on@module=buf",
];

const LAYOUTS: [&str; 4] = [
    "ui.layout.selected@layout=1",
    "ui.layout.selected@layout=2",
    "ui.layout.selected@layout=3",
    "ui.layout.selected@layout=4",
];

/// Every endpoint the four tables name on a read side: container, run and
/// glyph tones, block gates, and run contents.
fn declared_reads() -> BTreeSet<&'static str> {
    app_menu_spec::BOXES
        .iter()
        .filter_map(|entry| entry.active)
        .chain(app_menu_spec::RUNS.iter().filter_map(|entry| entry.read))
        .chain(app_menu_spec::RUNS.iter().filter_map(|entry| entry.active))
        .chain(app_menu_spec::MARKS.iter().filter_map(|entry| entry.active))
        .chain(app_menu_spec::BLOCKS.iter().map(|entry| entry.hidden))
        .collect()
}

/// Every endpoint the design tables name, minus the subtrees this state hides.
fn expected(hidden: &[&[&str]]) -> BTreeSet<String> {
    let hidden: BTreeSet<&str> = hidden
        .iter()
        .flat_map(|group| group.iter().copied())
        .collect();
    declared_reads()
        .into_iter()
        .filter(|key| !hidden.contains(key))
        .map(str::to_owned)
        .collect()
}

fn menu_reads(state: MenuState) -> BTreeSet<String> {
    rendered_endpoints(&app_menu_spec::menu(), state.truthy())
}

#[kithara::test]
fn the_default_snapshot_reads_every_endpoint_outside_a_collapsed_group() {
    assert_eq!(
        menu_reads(DEFAULT),
        expected(&[&WINDOW_3, &MODULES, &LAYOUTS])
    );
}

#[kithara::test]
fn a_closed_menu_reads_nothing_below_the_pop_and_still_reads_its_anchor() {
    let closed = MenuState {
        open: false,
        ..DEFAULT
    };

    assert_eq!(menu_reads(closed), keys(["ui.menu.open"]));
}

#[kithara::test]
fn the_module_group_adds_exactly_the_eleven_module_flags() {
    let state = MenuState {
        group: Group::Mod,
        ..DEFAULT
    };

    let open = menu_reads(state);

    assert_eq!(open, expected(&[&WINDOW_3, &LAYOUTS]));
    assert_eq!(
        open.difference(&menu_reads(DEFAULT))
            .cloned()
            .collect::<BTreeSet<_>>(),
        keys(MODULES),
    );
}

#[kithara::test]
fn the_layout_group_adds_exactly_the_four_layout_flags() {
    let state = MenuState {
        group: Group::Lay,
        ..DEFAULT
    };

    let open = menu_reads(state);

    assert_eq!(open, expected(&[&WINDOW_3, &MODULES]));
    assert_eq!(
        open.difference(&menu_reads(DEFAULT))
            .cloned()
            .collect::<BTreeSet<_>>(),
        keys(LAYOUTS),
        "the inert save row reads no endpoint and so contributes none",
    );
}

#[kithara::test]
fn no_open_group_reads_neither_the_grid_nor_the_list_and_still_reads_the_windows() {
    let state = MenuState {
        group: Group::Closed,
        ..DEFAULT
    };

    let closed = menu_reads(state);

    assert_eq!(closed, expected(&[&WINDOW_3, &MODULES, &LAYOUTS]));
    assert_eq!(
        closed,
        menu_reads(DEFAULT),
        "no group and the window group are the same surface",
    );
}

#[kithara::test]
fn one_window_reads_only_the_first_window_row() {
    let state = MenuState {
        windows: 1,
        ..DEFAULT
    };

    assert_eq!(
        menu_reads(state),
        expected(&[&WINDOW_2, &WINDOW_3, &MODULES, &LAYOUTS])
    );
}

#[kithara::test]
fn three_windows_read_every_window_row() {
    let state = MenuState {
        windows: 3,
        ..DEFAULT
    };

    assert_eq!(menu_reads(state), expected(&[&MODULES, &LAYOUTS]));
}

#[kithara::test]
fn every_endpoint_the_tables_name_is_reachable_in_some_state() {
    let cases = [
        DEFAULT,
        MenuState {
            open: false,
            ..DEFAULT
        },
        MenuState {
            group: Group::Mod,
            ..DEFAULT
        },
        MenuState {
            group: Group::Lay,
            ..DEFAULT
        },
        MenuState {
            group: Group::Closed,
            ..DEFAULT
        },
        MenuState {
            windows: 1,
            ..DEFAULT
        },
        MenuState {
            windows: 3,
            ..DEFAULT
        },
    ];

    let live: BTreeSet<String> = cases.into_iter().flat_map(menu_reads).collect();

    assert_eq!(live, expected(&[]));
}
