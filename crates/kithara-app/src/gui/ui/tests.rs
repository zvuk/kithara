use std::{cell::RefCell, collections::BTreeSet};

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi, compiled_min},
    expand::{ControlSpec, ExpandedNode},
    module::{MeasureAxis, TextAlign, TextStyle},
    render::{ReadValue, Reads, tree},
    size::{Dim, SizeSpec, control_size},
};

use super::{cache::DeckLayout, compile::compile_ui, events::route, scope::MICRO_DECK};

const LAYOUTS: [DeckLayout; 2] = [DeckLayout::Single, DeckLayout::Dual];

fn each_expanded(node: &ExpandedNode, visit: &mut impl FnMut(&ExpandedNode)) {
    visit(node);
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => {
            for child in children {
                each_expanded(child, visit);
            }
        }
        ExpandedNode::Optional { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. }
        | ExpandedNode::Scroll { child, .. } => {
            each_expanded(child, visit);
        }
        ExpandedNode::Adaptive { base, steps, .. } => {
            each_expanded(base, visit);
            for (_, branch) in steps {
                each_expanded(branch, visit);
            }
        }
        ExpandedNode::Popover {
            anchor, content, ..
        } => {
            each_expanded(anchor, visit);
            each_expanded(content, visit);
        }
        _ => {}
    }
}

fn each_node(ui: &CompiledUi, visit: &mut impl FnMut(&ExpandedNode)) {
    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|cell| &cell.node));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Adaptive { base, steps, .. } => {
                stack.push(base);
                stack.extend(steps.iter().map(|(_, branch)| branch));
            }
            CompiledNode::Module { root, .. } => each_expanded(root, visit),
            _ => {}
        }
    }
}

/// Every addressed node in the compiled UI, as
/// `(path, scoped binding keys)`.
fn controls(ui: &CompiledUi) -> Vec<(&str, Vec<&str>)> {
    let mut out = Vec::new();
    each_node(ui, &mut |node| match node {
        ExpandedNode::Control {
            path, read, write, ..
        } => {
            let keys = [read.as_ref(), write.as_ref()]
                .into_iter()
                .flatten()
                .map(|binding| ui.resolve(binding.key))
                .collect();
            out.push((ui.resolve(*path), keys));
        }
        ExpandedNode::Row {
            surface: Some(surface),
            ..
        }
        | ExpandedNode::Column {
            surface: Some(surface),
            ..
        } => out.push((
            ui.resolve(surface.path),
            vec![ui.resolve(surface.write.key)],
        )),
        ExpandedNode::Pressable { path, press, .. } => {
            out.push((ui.resolve(*path), vec![ui.resolve(press.key)]));
        }
        ExpandedNode::Popover { path, open, .. } => {
            out.push((ui.resolve(*path), vec![ui.resolve(open.key)]));
        }
        _ => {}
    });
    out
}

fn control_paths(ui: &CompiledUi) -> Vec<&str> {
    controls(ui).into_iter().map(|(path, _)| path).collect()
}

/// Containers that catch the wheel, as `(path, scoped write key)`.
fn surfaces(ui: &CompiledUi) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    each_node(ui, &mut |node| {
        if let ExpandedNode::Row {
            surface: Some(surface),
            ..
        }
        | ExpandedNode::Column {
            surface: Some(surface),
            ..
        } = node
        {
            out.push((ui.resolve(surface.path), ui.resolve(surface.write.key)));
        }
    });
    out
}

/// Every module that takes drops, as `(instance, scoped binding keys)`.
fn drop_targets(ui: &CompiledUi) -> Vec<(&str, Vec<&str>)> {
    let mut out = Vec::new();
    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|cell| &cell.node));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Adaptive { base, steps, .. } => {
                stack.push(base);
                stack.extend(steps.iter().map(|(_, branch)| branch));
            }
            CompiledNode::Module {
                instance,
                drop: Some(drop),
                ..
            } => out.push((
                ui.resolve(*instance),
                vec![ui.resolve(drop.write.key), ui.resolve(drop.read.key)],
            )),
            _ => {}
        }
    }
    out
}

#[kithara::test]
fn documents_compile_against_the_registry() {
    for layout in LAYOUTS {
        compile_ui(layout).unwrap();
    }
}

/// The width from which the window lays out every module it has.
/// Every module instance a branch lays out, sorted; a module standing in two
/// branches of one adaptive node is named once per branch.
fn instances<'a>(ui: &'a CompiledUi, node: &'a CompiledNode) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|cell| &cell.node));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Adaptive { base, steps, .. } => {
                stack.push(base);
                stack.extend(steps.iter().map(|(_, branch)| branch));
            }
            CompiledNode::Module { instance, .. } => out.push(ui.resolve(*instance)),
            _ => {}
        }
    }
    out.sort_unstable();
    out
}

fn module<'a>(ui: &'a CompiledUi, want: &str) -> &'a CompiledNode {
    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|cell| &cell.node));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Adaptive { base, steps, .. } => {
                stack.push(base);
                stack.extend(steps.iter().map(|(_, branch)| branch));
            }
            CompiledNode::Module { instance, .. } if ui.resolve(*instance) == want => {
                return node;
            }
            _ => {}
        }
    }
    panic!("no module instance `{want}`");
}

/// The address a bar cell answers to: the cell itself, or the first addressed
/// node behind the wrappers it stands in.
fn cell_id<'a>(ui: &'a CompiledUi, node: &'a ExpandedNode) -> &'a str {
    match node {
        ExpandedNode::Control { path, .. }
        | ExpandedNode::Popover { path, .. }
        | ExpandedNode::Pressable { path, .. } => ui.resolve(*path),
        ExpandedNode::Optional { block, .. } => ui.resolve(block.path),
        ExpandedNode::Adaptive { base, .. } => cell_id(ui, base),
        ExpandedNode::Reveal { child, .. } | ExpandedNode::Scroll { child, .. } => {
            cell_id(ui, child)
        }
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => {
            children.first().map_or("", |child| cell_id(ui, child))
        }
        _ => "",
    }
}

fn module_root<'a>(ui: &'a CompiledUi, instance: &str) -> &'a ExpandedNode {
    let CompiledNode::Module { root, .. } = module(ui, instance) else {
        panic!("`{instance}` is no module");
    };
    root
}

/// The direct cells of a container that measures itself, as
/// `(threshold, cell)` in document order.
fn cells<'a>(
    node: &'a ExpandedNode,
    axis: MeasureAxis,
) -> Vec<((f32, Option<f32>), &'a ExpandedNode)> {
    let (ExpandedNode::Row {
        measure, children, ..
    }
    | ExpandedNode::Column {
        measure, children, ..
    }) = node
    else {
        panic!("a container that measures itself is a row or a column");
    };
    assert_eq!(
        *measure,
        Some(axis),
        "a cell answers a threshold on the axis its container measures",
    );
    children
        .iter()
        .map(|child| match child {
            ExpandedNode::Reveal {
                from, until, child, ..
            } => ((*from, *until), &**child),
            child => ((0.0, None), child),
        })
        .collect()
}

/// The direct cells of a bar, as `(threshold, address)` in document order.
fn bar_cells<'a>(ui: &'a CompiledUi, bar: &'a ExpandedNode) -> Vec<((f32, Option<f32>), &'a str)> {
    cells(bar, MeasureAxis::Width)
        .into_iter()
        .map(|(band, cell)| (band, cell_id(ui, cell)))
        .collect()
}

/// The blocks the window stacks, as `(band, block)` in document order.
fn root_cells(ui: &CompiledUi) -> Vec<((f32, Option<f32>), &CompiledNode)> {
    let CompiledNode::Split {
        measure, children, ..
    } = &ui.root
    else {
        panic!("the window stacks its blocks in one split");
    };
    assert_eq!(
        *measure,
        Some(MeasureAxis::Height),
        "the window reads its own height",
    );
    children
        .iter()
        .map(|cell| ((cell.from, cell.until), &cell.node))
        .collect()
}

/// The blocks standing in a window of `room`, named by their instances.
fn standing<'a>(
    ui: &'a CompiledUi,
    cells: &[((f32, Option<f32>), &'a CompiledNode)],
    room: f32,
) -> Vec<&'a str> {
    let mut out: Vec<&str> = cells
        .iter()
        .filter(|((from, until), _)| *from <= room && until.is_none_or(|until| room < until))
        .flat_map(|(_, node)| instances(ui, node))
        .collect();
    out.sort_unstable();
    out
}

/// What the blocks standing in a window of `room` need of its height.
fn standing_height(cells: &[((f32, Option<f32>), &CompiledNode)], room: f32) -> f32 {
    cells
        .iter()
        .filter(|((from, until), _)| *from <= room && until.is_none_or(|until| room < until))
        .map(|(_, node)| compiled_min(node, builtin::skin_doc()).h.min())
        .sum()
}

/// The bar the window stands while it is too short for the decks.
fn micro_bar<'a>(ui: &'a CompiledUi) -> &'a ExpandedNode {
    module_root(ui, "micro-bar")
}

/// The box the micro bar declares for itself.
fn bar_box(bar: &ExpandedNode) -> SizeSpec {
    let ExpandedNode::Row {
        size: Some(size), ..
    } = bar
    else {
        panic!("the micro bar declares its own box");
    };
    *size
}

/// The height the track list asks for, read as the compiler reads it: the box
/// the document declares for the control, and the skin's box otherwise.
fn list_min(pane: &ExpandedNode) -> f32 {
    let mut found = None;
    each_expanded(pane, &mut |node| {
        if let ExpandedNode::Control { spec, size, .. } = node
            && matches!(spec, ControlSpec::TrackList { .. })
        {
            found = Some(size.unwrap_or_else(|| control_size(spec, builtin::skin_doc())));
        }
    });
    found.expect("the browser panel draws a track list").h.min()
}

/// The first segment of an address names the layout instance that answers it,
/// and `gui::ui::events::route` is the host's own list; a document minting an
/// instance that list does not name sends its controls nowhere.
#[kithara::test]
fn every_address_names_an_instance_the_host_routes() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let mut named: Vec<&str> = control_paths(&ui)
            .into_iter()
            .filter_map(|path| path.split_once('/').map(|(instance, _)| instance))
            .collect();
        named.sort_unstable();
        named.dedup();
        for instance in &named {
            assert!(
                route(instance).is_some(),
                "{layout:?}: `{instance}` addresses controls that `events::route` does not name",
            );
        }

        let mut want = vec!["bar", "deck-a", "library", "micro-bar", "mixer", "overview"];
        if layout == DeckLayout::Dual {
            want.push("deck-b");
        }
        want.sort_unstable();
        assert_eq!(named, want, "{layout:?}");
    }
}

/// Each block arrives once the window is tall enough to hold it under the ones
/// above it, so every threshold is what the blocks standing at it need.
#[kithara::test]
fn every_block_stands_once_the_window_holds_the_ones_above_it() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let cells = root_cells(&ui);

        assert_eq!(ui.size, SizeSpec::new(Dim::Fill, Dim::Fill), "{layout:?}");
        for from in cells.iter().map(|((from, _), _)| *from) {
            if from > 0.0 {
                assert_eq!(
                    standing_height(&cells, from),
                    from,
                    "{layout:?}: the blocks standing at {from}",
                );
            }
        }
    }
}

/// The window takes its blocks one at a time: the bar alone at its narrowest,
/// then the browser, then the overview row, then the decks and the mixer with
/// the full bar in place of the micro one.
#[kithara::test]
fn the_window_takes_its_blocks_one_by_one_as_it_grows_taller() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let cells = root_cells(&ui);
        let mut ladder: Vec<f32> = cells
            .iter()
            .map(|((from, _), _)| *from)
            .filter(|from| *from > 0.0)
            .collect();
        ladder.sort_by(f32::total_cmp);
        ladder.dedup();
        let [browser, overview, decks] = ladder[..] else {
            panic!("{layout:?}: the window climbs three rungs, got {ladder:?}");
        };

        assert_eq!(
            standing(&ui, &cells, ui.min.h.min()),
            ["micro-bar"],
            "{layout:?}",
        );
        assert_eq!(
            standing(&ui, &cells, browser),
            ["library", "micro-bar"],
            "{layout:?}",
        );
        assert_eq!(
            standing(&ui, &cells, overview),
            ["library", "micro-bar", "overview"],
            "{layout:?}",
        );
        let mut want = vec!["bar", "deck-a", "library", "mixer", "overview"];
        if layout == DeckLayout::Dual {
            want.push("deck-b");
        }
        want.sort_unstable();
        assert_eq!(standing(&ui, &cells, decks), want, "{layout:?}");
    }
}

/// The micro bar takes its cells as the window makes room for them; the menu,
/// play, the stretched strip and the window controls stand at every width.
#[kithara::test]
fn the_micro_bar_reveals_its_cells_as_the_window_widens() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();

        assert_eq!(
            bar_cells(&ui, micro_bar(&ui)),
            [
                ((0.0, None), "micro-bar/menu/pop"),
                ((0.0, None), "micro-bar/play"),
                ((590.0, None), "micro-bar/summary"),
                ((0.0, Some(350.0)), "micro-bar/drag"),
                ((350.0, None), "micro-bar/wave"),
                ((670.0, None), "micro-bar/speaker"),
                ((440.0, None), "micro-bar/remain"),
                ((0.0, None), "micro-bar/before-window"),
                ((0.0, None), "micro-bar/window"),
            ],
            "{layout:?}",
        );
    }
}

/// The drag strip and the wave share one place in the bar: the strip ends at
/// the width the wave starts at, so exactly one of them stands at any width
/// and a cell arriving beside the wave narrows it rather than taking it away.
#[kithara::test]
fn the_micro_drag_strip_hands_its_place_to_the_wave() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let cells = bar_cells(&ui, micro_bar(&ui));
        let band = |id: &str| {
            cells
                .iter()
                .find_map(|(band, cell)| (*cell == id).then_some(*band))
                .unwrap_or_else(|| panic!("{layout:?}: the bar names {id}"))
        };

        let (strip_from, strip_until) = band("micro-bar/drag");
        let (wave_from, wave_until) = band("micro-bar/wave");

        assert_eq!(
            strip_from, 0.0,
            "{layout:?}: the strip stands at the narrowest bar"
        );
        assert_eq!(strip_until, Some(wave_from), "{layout:?}");
        assert_eq!(
            wave_until, None,
            "{layout:?}: the wave stands from its width up"
        );
    }
}

/// The bar keeps the telemetry it has room for: the CPU cell and the wordmark
/// stand only in a window wide enough to spare them the space.
#[kithara::test]
fn the_bar_reveals_its_telemetry_as_the_window_widens() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();

        assert_eq!(
            bar_cells(&ui, module_root(&ui, "bar")),
            [
                ((0.0, None), "bar/menu/pop"),
                ((1250.0, None), "bar/brand"),
                ((0.0, None), "bar/drag"),
                ((1120.0, None), "bar/cpu-block"),
                ((0.0, None), "bar/broadcast-block"),
                ((0.0, None), "bar/before-window"),
                ((0.0, None), "bar/window"),
            ],
            "{layout:?}",
        );
    }
}

/// The browser panel joins the micro bar once the window is as tall as the two
/// of them together, so the threshold is the bar's own height plus the list's
/// own minimum.
#[kithara::test]
fn the_browser_panel_stands_once_the_window_is_tall_enough_for_it() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let cells = root_cells(&ui);
        let Some(((from, None), _)) = cells
            .iter()
            .copied()
            .find(|(_, node)| instances(&ui, node) == ["library"])
        else {
            panic!("{layout:?}: the window stands the browser over the bar");
        };

        assert_eq!(
            from,
            bar_box(micro_bar(&ui)).h.min() + list_min(module_root(&ui, "library")),
            "{layout:?}",
        );
    }
}

/// The window may be squeezed to the micro bar and no further, so the tree's
/// minimum is that bar's own. The width is the room the bar's standing cells
/// settle on, which no document names; the height is the box it declares.
#[kithara::test]
fn the_window_minimum_holds_the_micro_bar() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();

        assert_eq!(
            ui.min,
            SizeSpec::new(Dim::Fixed(221.0), Dim::Fixed(42.0)),
            "{layout:?}",
        );
        assert_eq!(
            ui.min,
            compiled_min(module(&ui, "micro-bar"), builtin::skin_doc()),
            "{layout:?}",
        );
    }
}

/// Both node enums are `#[non_exhaustive]`, so every walker here ends at a
/// `_ => {}`: a shape it does not name empties it in silence, and the tests
/// that sweep its result stay true over nothing.
#[kithara::test]
fn every_walker_reaches_the_nodes_the_documents_declare() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let dual = layout == DeckLayout::Dual;
        for (walker, found, floor) in [
            (
                "controls",
                controls(&ui).len(),
                if dual { 213 } else { 156 },
            ),
            ("surfaces", surfaces(&ui).len(), layout.decks()),
            (
                "pressables",
                pressables(&ui).len(),
                if dual { 47 } else { 36 },
            ),
            ("drop_targets", drop_targets(&ui).len(), layout.decks()),
            ("guarded_by", guarded_by(&ui, "ui.module.hidden").len(), 4),
            (
                "optional_modules",
                optional_modules(&ui, "ui.module.hidden").len(),
                3,
            ),
        ] {
            assert!(
                found >= floor,
                "{layout:?}: `{walker}` reached {found} nodes, under the {floor} the documents declare",
            );
        }
    }
}

#[kithara::test]
fn deck_scoped_controls_are_routed_to_the_deck_they_read() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        for (path, keys) in controls(&ui) {
            for key in keys {
                let Some(letter) = key.split_once('@').and_then(|(_, scope)| {
                    scope.split(',').find_map(|pair| pair.strip_prefix("deck="))
                }) else {
                    continue;
                };
                let mut routed = vec![
                    format!("deck-{letter}/"),
                    format!("mixer/{letter}/"),
                    format!("overview/{letter}/"),
                ];
                if letter == MICRO_DECK {
                    routed.push("micro-bar/".to_owned());
                }
                assert!(
                    routed.iter().any(|prefix| path.starts_with(prefix)),
                    "{layout:?}: control `{path}` is bound to `{key}` but is not addressed by deck `{letter}`",
                );
            }
        }
    }
}

#[kithara::test]
fn the_cpu_cell_reads_engine_load_as_a_bar_and_a_number() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let mut bars = Vec::new();
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Control {
                path,
                spec: ControlSpec::Meter,
                read: Some(read),
                ..
            } = node
            {
                bars.push((ui.resolve(*path), ui.resolve(read.key)));
            }
        });

        assert_eq!(bars, [("bar/cpu-bar", "engine.load")]);
        assert!(
            controls(&ui).contains(&("bar/cpu-value", vec!["engine.load"])),
            "{layout:?}: the CPU readout must stay on engine.load",
        );
    }
}

/// The bar cell that takes the mix on air: one press target on the toggle,
/// and a dot that reads whether the stream is serving.
#[kithara::test]
fn the_bar_carries_the_broadcast_cell() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();

        assert!(
            controls(&ui).contains(&("bar/broadcast", vec!["broadcast.toggle"])),
            "{layout:?}: the broadcast cell must press the toggle command",
        );

        let mut dots = Vec::new();
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Control {
                path,
                spec: ControlSpec::StatusDot { active, .. },
                ..
            } = node
            {
                dots.push((
                    ui.resolve(*path),
                    active.as_ref().map(|binding| ui.resolve(binding.key)),
                ));
            }
        });
        assert_eq!(
            dots,
            [("bar/broadcast-dot", Some("broadcast.on_air"))],
            "{layout:?}",
        );
    }
}

/// Whichever shape the window draws, its chrome is the bar that shape carries:
/// each bar owns a drag strip and a set of window controls, and those four
/// addresses are the whole of it.
#[kithara::test]
fn the_bar_owns_the_window_chrome() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let mut seen = Vec::new();
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Control { path, spec, .. } = node
                && matches!(
                    spec,
                    ControlSpec::WindowDrag | ControlSpec::WindowControls { .. }
                )
            {
                seen.push(ui.resolve(*path));
            }
        });
        seen.sort_unstable();
        seen.dedup();

        assert_eq!(
            seen,
            [
                "bar/drag",
                "bar/window",
                "micro-bar/drag",
                "micro-bar/window",
            ],
            "{layout:?}",
        );
        assert!(
            ui.resize_edges,
            "{layout:?}: the window has no other way to be resized"
        );
    }
}

const EQ_KNOBS: [&str; 7] = [
    "high-3",
    "mid-3",
    "low-3",
    "high-4",
    "high-mid-4",
    "low-mid-4",
    "low-4",
];

/// The controls one channel strip carries, named by their leaf id: a knob bank
/// reaches the strip through an include, so the path carries that segment too.
fn strip_controls<'a>(paths: &[&'a str], letter: &str) -> Vec<&'a str> {
    let prefix = format!("mixer/{letter}/");
    paths
        .iter()
        .copied()
        .filter_map(|path| path.strip_prefix(&prefix))
        .filter_map(|path| path.rsplit('/').next())
        .collect()
}

#[kithara::test]
fn every_channel_strip_carries_the_supported_control_set() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let paths = control_paths(&ui);
    for letter in ["a", "b"] {
        let controls = strip_controls(&paths, letter);
        for name in EQ_KNOBS.into_iter().chain(["volume"]) {
            assert!(
                controls.contains(&name),
                "missing control `mixer/{letter}/{name}`"
            );
        }
    }
}

/// Answers one deck's band count and records every endpoint the renderer asks
/// for on the way.
struct BandReads {
    bands: Option<ReadValue<'static>>,
    seen: RefCell<BTreeSet<String>>,
}

impl BandReads {
    /// Renders deck A and reports the EQ endpoints the bank it drew asked for.
    fn banked(bands: Option<ReadValue<'static>>) -> BTreeSet<String> {
        let ui = compile_ui(DeckLayout::Dual).unwrap();
        let reads = Self {
            bands,
            seen: RefCell::default(),
        };

        drop(tree::render(&ui.root, &ui, &reads, builtin::skin()));

        reads.seen.take()
    }
}

impl Reads for BandReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        self.seen.borrow_mut().insert(endpoint.to_owned());
        if endpoint == "deck.eq.bands@deck=a" {
            return self.bands;
        }
        None
    }
}

#[kithara::test]
fn one_band_count_decides_which_eq_bank_a_deck_draws() {
    for (bands, drawn, spare) in [
        (3.0, "deck.eq.mid", "deck.eq.low_mid"),
        (4.0, "deck.eq.low_mid", "deck.eq.mid"),
    ] {
        let seen = BandReads::banked(Some(ReadValue::Scalar(bands)));

        assert!(
            seen.contains(&format!("{drawn}@deck=a")),
            "{bands} bands: `{drawn}` must be drawn, saw {seen:?}",
        );
        assert!(
            !seen.contains(&format!("{spare}@deck=a")),
            "{bands} bands: `{spare}` belongs to the other bank",
        );
    }
}

/// A count nobody states is no measurement, and the strip falls to the bank the
/// document declares as its base.
#[kithara::test]
fn a_band_count_that_is_no_number_draws_the_three_band_bank() {
    for unstated in [
        None,
        Some(ReadValue::Scalar(f64::NAN)),
        Some(ReadValue::Scalar(f64::INFINITY)),
        Some(ReadValue::Bool(true)),
    ] {
        let seen = BandReads::banked(unstated);

        assert!(
            seen.contains("deck.eq.mid@deck=a"),
            "{unstated:?}: the three-band bank draws, saw {seen:?}",
        );
        assert!(
            !seen.contains("deck.eq.low_mid@deck=a"),
            "{unstated:?}: the four-band bank needs a count of four",
        );
    }
}

/// The document builds the scoped key and the host answers it; each side is
/// written by hand, so the two only meet if the compiled key is pinned.
#[kithara::test]
fn the_mode_menu_asks_the_key_the_deck_answers() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let mut asked = Vec::new();
    each_node(&ui, &mut |node| {
        if let ExpandedNode::Row {
            active: Some(binding),
            ..
        } = node
        {
            asked.push(ui.resolve(binding.key));
        }
    });

    for letter in ["a", "b"] {
        for bands in [3, 4] {
            let want = format!("deck.eq.selected@bands={bands},deck={letter}");
            assert!(
                asked.contains(&want.as_str()),
                "no menu row reads `{want}`, saw {asked:?}",
            );
        }
    }
}

#[kithara::test]
fn every_eq_bank_carries_its_pointer_menu() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let paths = control_paths(&ui);
    for letter in ["a", "b"] {
        for name in ["eq-menu-anchor", "eq-3", "eq-4"] {
            let want = format!("mixer/{letter}/{name}");
            assert!(paths.contains(&want.as_str()), "missing control `{want}`");
        }
    }
}

#[kithara::test]
fn eq_banks_stack_their_knobs_from_high_to_low() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let paths = control_paths(&ui);
    for letter in ["a", "b"] {
        let order: Vec<&str> = strip_controls(&paths, letter)
            .into_iter()
            .filter(|name| EQ_KNOBS.contains(name))
            .collect();
        assert_eq!(order, EQ_KNOBS);
    }
}

#[kithara::test]
fn every_eq_bank_centers_its_knobs() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let mut centered = 0;
    each_node(&ui, &mut |node| {
        let ExpandedNode::Column {
            id: Some(id),
            align,
            ..
        } = node
        else {
            return;
        };
        if matches!(
            ui.resolve(*id).rsplit('/').next(),
            Some("eq-3-knobs" | "eq-4-knobs")
        ) {
            assert_eq!(*align, TextAlign::Center);
            centered += 1;
        }
    });
    assert_eq!(centered, 4, "two banks on each of two decks");
}

#[kithara::test]
fn the_app_hides_controls_outside_the_supported_playback_contract() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let paths = control_paths(&ui);
    for path in [
        "deck-a/time",
        "deck-a/keylock",
        "deck-b/time",
        "deck-b/keylock",
        "mixer/a/mute",
        "mixer/b/mute",
        "mixer/master",
    ] {
        assert!(!paths.contains(&path), "unexpected control `{path}`");
    }
}

#[kithara::test]
fn tempo_and_volume_controls_bind_to_the_deck_they_address() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let controls = controls(&ui);
    for letter in ["a", "b"] {
        for (want, endpoint) in [
            (format!("deck-{letter}/tempo"), "deck.tempo.rate"),
            (format!("mixer/{letter}/volume"), "mixer.trim"),
        ] {
            let (_, keys) = controls
                .iter()
                .find(|(path, _)| **path == want)
                .unwrap_or_else(|| panic!("missing control `{want}`"));
            let binding = format!("{endpoint}@deck={letter}");
            assert!(
                keys.contains(&binding.as_str()),
                "`{want}` must bind `{binding}`, got {keys:?}"
            );
        }
    }
}

#[kithara::test]
fn the_deck_tempo_block_is_the_only_writer_of_the_deck_tempo() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let mut writers: Vec<&str> = controls(&ui)
        .into_iter()
        .filter(|(_, keys)| keys.iter().any(|key| key.starts_with("deck.tempo.rate@")))
        .map(|(path, _)| path)
        .collect();
    writers.sort_unstable();

    assert_eq!(writers, ["deck-a/tempo", "deck-b/tempo"]);

    let surfaces = surfaces(&ui);
    for letter in ["a", "b"] {
        let path = format!("deck-{letter}/tempo");
        let key = format!("deck.tempo.rate@deck={letter}");
        assert!(
            surfaces.contains(&(path.as_str(), key.as_str())),
            "the tempo block must catch the wheel for deck {letter}, got {surfaces:?}"
        );
    }
}

#[kithara::test]
fn the_deck_transport_carries_the_zoom_pair() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let controls = controls(&ui);
    for letter in ["a", "b"] {
        for (name, endpoint) in [
            ("zoom-out", "deck.view.zoom_out"),
            ("zoom-in", "deck.view.zoom_in"),
        ] {
            let want = format!("deck-{letter}/{name}");
            let (_, keys) = controls
                .iter()
                .find(|(path, _)| **path == want)
                .unwrap_or_else(|| panic!("missing control `{want}`"));
            let binding = format!("{endpoint}@deck={letter}");
            assert!(
                keys.contains(&binding.as_str()),
                "`{want}` must bind `{binding}`, got {keys:?}"
            );
        }
    }
}

#[kithara::test]
fn a_layout_addresses_only_the_decks_it_lays_out() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let paths = control_paths(&ui);
        let shown = ["a", "b"].into_iter().take(layout.decks());

        for letter in shown {
            for path in [
                format!("deck-{letter}/wave"),
                format!("overview/{letter}/wave"),
                format!("mixer/{letter}/volume"),
            ] {
                assert!(paths.contains(&path.as_str()), "{layout:?}: missing {path}");
            }
        }
        for letter in ["a", "b"].into_iter().skip(layout.decks()) {
            let hidden = [
                format!("deck-{letter}/"),
                format!("overview/{letter}/"),
                format!("mixer/{letter}/"),
            ];
            assert!(
                !paths
                    .iter()
                    .any(|path| hidden.iter().any(|prefix| path.starts_with(prefix))),
                "{layout:?}: still addresses deck {letter}: {paths:?}",
            );
        }
    }
}

#[kithara::test]
fn every_laid_out_deck_takes_dropped_tracks() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let targets = drop_targets(&ui);

        assert_eq!(targets.len(), layout.decks(), "{layout:?}: {targets:?}");
        for letter in ["a", "b"].into_iter().take(layout.decks()) {
            let want = (
                format!("deck-{letter}"),
                vec![
                    format!("deck.queue.load@deck={letter}"),
                    format!("ui.drag.over@deck={letter}"),
                ],
            );
            assert!(
                targets.iter().any(|(instance, keys)| *instance == want.0
                    && keys.iter().copied().eq(want.1.iter().map(String::as_str))),
                "{layout:?}: deck {letter} must take drops, got {targets:?}",
            );
        }
    }
}

#[kithara::test]
fn deck_letter_captions_name_their_deck() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let mut seen = 0_usize;
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Control {
                path,
                spec:
                    ControlSpec::Text {
                        style: TextStyle::DeckLetter,
                        label: Some(label),
                        ..
                    },
                ..
            } = node
            {
                let path = ui.resolve(*path);
                let letter = path.rsplit('/').nth(1).unwrap_or_default();
                assert_eq!(ui.resolve(*label), letter.to_uppercase(), "at `{path}`");
                seen += 1;
            }
        });
        assert!(seen > 0, "{layout:?}: no deck letter caption");
    }
}

fn pressables(ui: &CompiledUi) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    each_node(ui, &mut |node| {
        if let ExpandedNode::Pressable { path, press, .. } = node {
            out.push((ui.resolve(*path), ui.resolve(press.key)));
        }
    });
    out
}

/// Press targets that render only while `key` reads false.
fn guarded_by<'a>(ui: &'a CompiledUi, key: &str) -> Vec<&'a str> {
    fn walk<'a>(
        node: &'a ExpandedNode,
        ui: &'a CompiledUi,
        key: &str,
        guarded: bool,
        out: &mut Vec<&'a str>,
    ) {
        match node {
            ExpandedNode::Optional { block, child } => {
                let guarded = guarded || ui.resolve(block.hidden.key).starts_with(key);
                walk(child, ui, key, guarded, out);
            }
            ExpandedNode::Pressable { path, child, .. } => {
                if guarded {
                    out.push(ui.resolve(*path));
                }
                walk(child, ui, key, guarded, out);
            }
            ExpandedNode::Control { path, .. } => {
                if guarded {
                    out.push(ui.resolve(*path));
                }
            }
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    walk(child, ui, key, guarded, out);
                }
            }
            ExpandedNode::Reveal { child, .. } | ExpandedNode::Scroll { child, .. } => {
                walk(child, ui, key, guarded, out);
            }
            ExpandedNode::Adaptive { base, steps, .. } => {
                walk(base, ui, key, guarded, out);
                for (_, branch) in steps {
                    walk(branch, ui, key, guarded, out);
                }
            }
            ExpandedNode::Popover {
                anchor, content, ..
            } => {
                walk(anchor, ui, key, guarded, out);
                walk(content, ui, key, guarded, out);
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|cell| &cell.node));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Adaptive { base, steps, .. } => {
                stack.push(base);
                stack.extend(steps.iter().map(|(_, branch)| branch));
            }
            CompiledNode::Module { root, .. } => walk(root, ui, key, false, &mut out),
            _ => {}
        }
    }
    out
}

/// Layout panes that render only while a `key`-prefixed read is false.
fn optional_modules<'a>(ui: &'a CompiledUi, key: &str) -> Vec<(&'a str, &'a str)> {
    fn walk<'a>(
        node: &'a CompiledNode,
        ui: &'a CompiledUi,
        key: &str,
        guard: Option<&'a str>,
        out: &mut Vec<(&'a str, &'a str)>,
    ) {
        match node {
            CompiledNode::Split { children, .. } => {
                for cell in children {
                    walk(&cell.node, ui, key, guard, out);
                }
            }
            CompiledNode::Adaptive { base, steps, .. } => {
                walk(base, ui, key, guard, out);
                for (_, branch) in steps {
                    walk(branch, ui, key, guard, out);
                }
            }
            CompiledNode::Optional { block, child } => {
                let bound = ui.resolve(block.hidden.key);
                let guard = if bound.starts_with(key) {
                    Some(bound)
                } else {
                    guard
                };
                walk(child, ui, key, guard, out);
            }
            CompiledNode::Module { instance, .. } => {
                if let Some(bound) = guard {
                    out.push((ui.resolve(*instance), bound));
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    walk(&ui.root, ui, key, None, &mut out);
    out
}

/// One cell per module the app can lay out without, and the pane it names
/// leaves the layout while the cell is off.
#[kithara::test]
fn every_module_cell_switches_the_pane_it_names() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let pressed = pressables(&ui);

        assert!(
            pressed.contains(&("bar/menu/modules-head", "ui.menu.toggle_group@group=mod")),
            "{layout:?}: the modules group must expand from its own head",
        );
        let controls = controls(&ui);
        assert!(
            controls.contains(&("bar/menu/windows-head-count", vec!["ui.window.count"])),
            "{layout:?}: the section over the groups must count the windows",
        );
        for (path, key) in [
            ("bar/menu/window-1/title", "ui.window.title@window=1"),
            ("bar/menu/window-1/caption", "ui.window.caption@window=1"),
        ] {
            assert!(
                controls.contains(&(path, vec![key])),
                "{layout:?}: `{path}` must state the window through `{key}`",
            );
        }
        for module in ["ov", "mix", "lib", "cpu"] {
            let path = format!("bar/menu/module-{module}/cell");
            let key = format!("ui.module.toggle@module={module}");
            assert!(
                pressed.contains(&(path.as_str(), key.as_str())),
                "{layout:?}: `{path}` must toggle `{key}`, got {pressed:?}",
            );
        }

        let guarded = guarded_by(&ui, "ui.module.hidden");
        assert!(
            guarded.contains(&"bar/cpu-bar") && guarded.contains(&"bar/cpu-value"),
            "{layout:?}: the CPU readout must leave the bar with its cell, got {guarded:?}",
        );

        let panes = optional_modules(&ui, "ui.module.hidden");
        for (instance, module) in [("overview", "ov"), ("mixer", "mix"), ("library", "lib")] {
            let key = format!("ui.module.hidden@module={module}");
            assert!(
                panes.contains(&(instance, key.as_str())),
                "{layout:?}: pane `{instance}` must hide on `{key}`, got {panes:?}",
            );
        }
    }
}

/// A build without a packager has nothing for the air controls to command, so
/// the app must not draw them at all.
#[kithara::test]
fn every_air_control_hides_with_the_packager() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let guarded = guarded_by(&ui, "broadcast.hidden");

        for path in ["bar/broadcast", "bar/menu/cast"] {
            assert!(
                guarded.contains(&path),
                "{layout:?}: `{path}` must hide with the packager, guarded: {guarded:?}",
            );
        }
    }
}

/// The burger the bar carries as its first cell: one press target per command
/// the menu offers, plus the popover that reads the state it dismisses.
#[kithara::test]
fn the_bar_carries_the_app_menu() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let pressed = pressables(&ui);

        for (path, key) in [
            ("bar/menu/burger", "ui.menu.toggle"),
            ("bar/menu/header-close", "ui.menu.close"),
            ("bar/menu/layouts-head", "ui.menu.toggle_group@group=lay"),
            ("bar/menu/layout-1", "ui.layout.apply@layout=1"),
            ("bar/menu/layout-2", "ui.layout.apply@layout=2"),
            ("bar/menu/full-screen", "ui.window.toggle_full_screen"),
            ("bar/menu/cast", "broadcast.toggle"),
        ] {
            assert!(
                pressed.contains(&(path, key)),
                "{layout:?}: `{path}` must press `{key}`, got {pressed:?}",
            );
        }

        assert!(
            controls(&ui).contains(&("bar/menu/pop", vec!["ui.menu.open"])),
            "{layout:?}: the popover must read the state its dismissal writes",
        );
    }
}

#[kithara::test]
fn each_deck_picks_its_own_stream_quality() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let letters = ["a", "b"].into_iter().take(layout.decks());
        for letter in letters {
            let cell = format!("deck-{letter}/stream/cell");
            let auto = format!("deck-{letter}/stream/auto/pick");
            let rung = format!("deck-{letter}/stream/variant-0/pick");
            let pressed = pressables(&ui);

            assert!(
                pressed.contains(&(
                    cell.as_str(),
                    format!("deck.stream.toggle_quality_menu@deck={letter}").as_str()
                )),
                "{layout:?}: the cell of deck {letter} must toggle its own menu",
            );
            for (path, variant) in [(&auto, "auto"), (&rung, "0")] {
                assert!(
                    pressed.contains(&(
                        path.as_str(),
                        format!("deck.stream.select_variant@deck={letter},variant={variant}")
                            .as_str()
                    )),
                    "{layout:?}: `{path}` must pick rung `{variant}` on deck {letter}",
                );
            }
        }
    }
}
