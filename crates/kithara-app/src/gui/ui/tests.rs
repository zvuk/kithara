use std::{cell::RefCell, collections::BTreeSet, path::Path};

use ::kithara::ui::{
    builtin,
    compile::{CompiledNode, CompiledUi, compiled_min},
    error::UiDocError,
    expand::{ControlSpec, ExpandedNode},
    ids::SourceUri,
    module::{ButtonStyle, IconName, MeasureAxis, TextAlign, TextStyle, ViewSet, WaveStyle},
    registry::{EndpointCategory, ValueKind},
    render::{
        Clock, ControlAction, Ctx, Published, ReadValue, Reads, UiEvent, Walk, WriteValue, tree,
    },
    size::{Dim, SizeSpec, control_size},
    source::{FillDocument, SourceResolver, UiConfig},
    view::{self, ViewState},
};
use kithara_app_library::{Document, Endpoint, PAGES, Registration, SourcePage};
use kithara_test_utils::kithara;

use super::{
    cache::DeckLayout,
    compile::{AppUi, compile_package, compile_ui},
};
use crate::gui::{
    library::StartupSource,
    message::Message,
    reads::ReadRoot,
    test_fixture::{self, Probe},
    update::update,
};

const LAYOUTS: [DeckLayout; 2] = [DeckLayout::Single, DeckLayout::Dual];

fn each_expanded(node: &ExpandedNode, visit: &mut impl FnMut(&ExpandedNode)) {
    visit(node);
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. }
        | ExpandedNode::Stage { children, .. } => {
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

/// Every module that takes drops, as `(instance, scoped keys its drop zone writes)`.
fn drop_targets(ui: &CompiledUi) -> Vec<(&str, Vec<String>)> {
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
                drop: true,
                ..
            } => {
                let instance = ui.resolve(*instance);
                let dropped = settle(
                    ui,
                    &mut ViewState::new(),
                    &format!("{instance}/drop"),
                    ControlAction::Record(test_fixture::dragged("https://example.com/a.mp3")),
                );
                let keys = match dropped {
                    Some(UiEvent::Write { key, .. }) => vec![key],
                    _ => Vec::new(),
                };
                out.push((instance, keys));
            }
            _ => {}
        }
    }
    out
}

fn engine_descriptor_kind(spec: &ControlSpec) -> Option<&'static str> {
    match spec {
        ControlSpec::Button {
            icon: Some(IconName::PlayReverse),
            style,
            ..
        } if *style != ButtonStyle::MicroPrimary => None,
        ControlSpec::Button { .. } | ControlSpec::Toggle | ControlSpec::Checkbox => {
            Some("activation")
        }
        ControlSpec::Crossfader { .. } => Some("crossfader"),
        ControlSpec::Knob { .. } => Some("knob"),
        ControlSpec::VuStereo => Some("stereo-meter"),
        ControlSpec::VuVertical { .. } => Some("vertical-vu"),
        ControlSpec::Wave {
            style: WaveStyle::Hero,
            ..
        } => Some("hero-wave"),
        ControlSpec::Wave { .. } => Some("wave"),
        _ => None,
    }
}

fn hosted_engine_claims(ui: &CompiledUi) -> Vec<(&str, &'static str)> {
    let mut claims = Vec::new();
    each_node(ui, &mut |node| {
        let ExpandedNode::Control { path, spec, .. } = node else {
            return;
        };
        let path = ui.resolve(*path);
        if (path.starts_with("deck-")
            || path.starts_with("mixer/")
            || path.starts_with("overview/"))
            && let Some(kind) = engine_descriptor_kind(spec)
        {
            claims.push((path, kind));
        }
    });
    claims.sort_unstable();
    claims
}

#[kithara::test]
fn documents_compile_against_the_registry() {
    for layout in LAYOUTS {
        compile_ui(layout).unwrap();
    }
}

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

/// The room a cell stands in: from a threshold, and below a ceiling when one
/// is set.
type Band = (f32, Option<f32>);

/// A test's rewrite of one shipped package file's text.
type Edit = fn(&str) -> String;

fn cells(node: &ExpandedNode, axis: MeasureAxis) -> Vec<(Band, &ExpandedNode)> {
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

fn bar_cells<'a>(ui: &'a CompiledUi, bar: &'a ExpandedNode) -> Vec<(Band, &'a str)> {
    cells(bar, MeasureAxis::Width)
        .into_iter()
        .map(|(band, cell)| (band, cell_id(ui, cell)))
        .collect()
}

fn root_cells(ui: &CompiledUi) -> Vec<(Band, &CompiledNode)> {
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

fn standing<'a>(ui: &'a CompiledUi, cells: &[(Band, &'a CompiledNode)], room: f32) -> Vec<&'a str> {
    let mut out: Vec<&str> = cells
        .iter()
        .filter(|((from, until), _)| *from <= room && until.is_none_or(|until| room < until))
        .flat_map(|(_, node)| instances(ui, node))
        .collect();
    out.sort_unstable();
    out
}

fn standing_height(cells: &[(Band, &CompiledNode)], room: f32) -> f32 {
    cells
        .iter()
        .filter(|((from, until), _)| *from <= room && until.is_none_or(|until| room < until))
        .map(|(_, node)| compiled_min(node, builtin::skin_doc()).h.min())
        .sum()
}

fn micro_bar(ui: &CompiledUi) -> &ExpandedNode {
    module_root(ui, "micro-bar")
}

fn bar_box(bar: &ExpandedNode) -> SizeSpec {
    let ExpandedNode::Row {
        size: Some(size), ..
    } = bar
    else {
        panic!("the micro bar declares its own box");
    };
    *size
}

fn list_min(pane: &ExpandedNode) -> f32 {
    let mut found = None;
    each_expanded(pane, &mut |node| {
        if let ExpandedNode::Control { spec, size, .. } = node
            && matches!(spec, ControlSpec::Table { .. })
        {
            found = Some(size.unwrap_or_else(|| control_size(spec, builtin::skin_doc())));
        }
    });
    found.expect("the browser panel draws a track list").h.min()
}

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
                ((0.0, None), "micro-bar/window-block"),
            ],
            "{layout:?}",
        );
    }
}

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
                ((0.0, None), "bar/window-block"),
            ],
            "{layout:?}",
        );
    }
}

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

struct BandReads {
    bands: Option<ReadValue<'static>>,
    seen: RefCell<BTreeSet<String>>,
}

impl BandReads {
    fn banked(bands: Option<ReadValue<'static>>) -> BTreeSet<String> {
        let ui = compile_ui(DeckLayout::Dual).unwrap();
        let reads = Self {
            bands,
            seen: RefCell::default(),
        };

        drop(tree::render(
            &ui.root,
            &ui,
            &reads,
            &view::EMPTY,
            builtin::skin(),
            Clock::default(),
            None,
        ));

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
fn hosted_studio_controls_claimed_by_the_engine_keep_descriptor_shapes() {
    const SINGLE_HOSTED_CLAIMS: [(&str, &str); 15] = [
        ("deck-a/next", "activation"),
        ("deck-a/play", "activation"),
        ("deck-a/prev", "activation"),
        ("deck-a/wave", "hero-wave"),
        ("deck-a/zoom-in", "activation"),
        ("deck-a/zoom-out", "activation"),
        ("mixer/a/four-band/high-4", "knob"),
        ("mixer/a/four-band/high-mid-4", "knob"),
        ("mixer/a/four-band/low-4", "knob"),
        ("mixer/a/four-band/low-mid-4", "knob"),
        ("mixer/a/three-band/high-3", "knob"),
        ("mixer/a/three-band/low-3", "knob"),
        ("mixer/a/three-band/mid-3", "knob"),
        ("mixer/a/volume", "vertical-vu"),
        ("overview/a/wave", "wave"),
    ];

    const DUAL_HOSTED_CLAIMS: [(&str, &str); 31] = [
        ("deck-a/next", "activation"),
        ("deck-a/play", "activation"),
        ("deck-a/prev", "activation"),
        ("deck-a/wave", "hero-wave"),
        ("deck-a/zoom-in", "activation"),
        ("deck-a/zoom-out", "activation"),
        ("deck-b/next", "activation"),
        ("deck-b/play", "activation"),
        ("deck-b/prev", "activation"),
        ("deck-b/wave", "hero-wave"),
        ("deck-b/zoom-in", "activation"),
        ("deck-b/zoom-out", "activation"),
        ("mixer/a/four-band/high-4", "knob"),
        ("mixer/a/four-band/high-mid-4", "knob"),
        ("mixer/a/four-band/low-4", "knob"),
        ("mixer/a/four-band/low-mid-4", "knob"),
        ("mixer/a/three-band/high-3", "knob"),
        ("mixer/a/three-band/low-3", "knob"),
        ("mixer/a/three-band/mid-3", "knob"),
        ("mixer/a/volume", "vertical-vu"),
        ("mixer/b/four-band/high-4", "knob"),
        ("mixer/b/four-band/high-mid-4", "knob"),
        ("mixer/b/four-band/low-4", "knob"),
        ("mixer/b/four-band/low-mid-4", "knob"),
        ("mixer/b/three-band/high-3", "knob"),
        ("mixer/b/three-band/low-3", "knob"),
        ("mixer/b/three-band/mid-3", "knob"),
        ("mixer/b/volume", "vertical-vu"),
        ("mixer/xfade", "crossfader"),
        ("overview/a/wave", "wave"),
        ("overview/b/wave", "wave"),
    ];

    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let expected = match layout {
            DeckLayout::Single => SINGLE_HOSTED_CLAIMS.as_slice(),
            DeckLayout::Dual => DUAL_HOSTED_CLAIMS.as_slice(),
        };
        assert_eq!(
            hosted_engine_claims(&ui),
            expected,
            "{layout:?}: the engine-claimed descriptor inventory changed; unported controls, \
             passive controls, and containers are intentionally absent"
        );
    }
}

fn settle(
    ui: &CompiledUi,
    view: &mut ViewState,
    path: &str,
    action: ControlAction,
) -> Option<UiEvent> {
    let reads = BandReads {
        bands: None,
        seen: RefCell::default(),
    };
    let published = Published::Gesture {
        action,
        path: path.to_owned(),
    };
    ui.views().settle(published, &reads, view)
}

#[kithara::test]
fn a_secondary_press_opens_the_eq_menu_of_its_own_strip_and_a_mode_row_shuts_it() {
    let mode = |bands: u8, deck: &str| UiEvent::Write {
        key: format!("deck.eq.mode@bands={bands},deck={deck}"),
        value: WriteValue::Trigger,
    };
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let mut view = ViewState::default();

    let opened = settle(
        &ui,
        &mut view,
        "mixer/a/eq-menu-anchor",
        ControlAction::SecondaryActivate,
    );
    assert_eq!(opened, None);
    let picked = settle(&ui, &mut view, "mixer/b/eq-3", ControlAction::Activate);
    assert_eq!(picked, Some(mode(3, "b")));
    assert!(
        view.flag("mixer/a"),
        "deck B's row leaves deck A's menu open"
    );
    assert!(!view.flag("mixer/b"));

    let picked = settle(&ui, &mut view, "mixer/a/eq-4", ControlAction::Activate);
    assert_eq!(picked, Some(mode(4, "a")));
    assert!(
        !view.flag("mixer/a"),
        "a mode row shuts the menu it sits in"
    );
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
fn the_hosted_deck_tempo_surface_remains_on_iced() {
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
            "hosted deck {letter} must keep its still-iced tempo wheel surface, got {surfaces:?}"
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
                vec![format!("deck.queue.load@deck={letter}")],
            );
            assert!(
                targets
                    .iter()
                    .any(|(instance, keys)| *instance == want.0 && *keys == want.1),
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
            | ExpandedNode::Slot { children, .. }
            | ExpandedNode::Stage { children, .. } => {
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
            pressed.contains(&("bar/menu/modules-head", "bar/group-mod")),
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

#[kithara::test]
fn the_add_folder_row_hides_without_a_folder_picker() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let guarded = guarded_by(&ui, "library.add_folder.hidden");

        assert!(
            guarded.contains(&"bar/menu/add-folder"),
            "{layout:?}: Add folder must hide with the folder picker, guarded: {guarded:?}",
        );
    }
}

#[kithara::test]
fn every_window_control_hides_with_the_chrome() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let mut windows = Vec::new();
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Control {
                path,
                spec: ControlSpec::WindowControls { .. },
                ..
            } = node
            {
                windows.push(ui.resolve(*path));
            }
        });
        windows.sort_unstable();
        windows.dedup();
        assert_eq!(windows, ["bar/window", "micro-bar/window"], "{layout:?}");

        let guarded = guarded_by(&ui, "ui.window.chrome_hidden@window=1");
        for path in windows {
            assert!(
                guarded.contains(&path),
                "{layout:?}: `{path}` must hide with the chrome, guarded: {guarded:?}",
            );
        }
    }
}

/// The burger the bar carries as its first cell: one press target per command
/// the menu offers, plus the surface it opens - which costs the application no
/// endpoint at all, because whether a menu stands open is not its business.
#[kithara::test]
fn the_bar_carries_the_app_menu() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let pressed = pressables(&ui);

        for (path, open, opened) in [
            ("bar/menu/burger", ViewSet::Off, true),
            ("bar/menu/burger", ViewSet::On, false),
            ("bar/menu/header-close", ViewSet::On, false),
            ("bar/menu/pop", ViewSet::On, false),
        ] {
            let mut view = ViewState::new();
            view.set("bar/menu", open);
            let host = settle(&ui, &mut view, path, ControlAction::Activate);
            assert_eq!(
                (host, view.flag("bar/menu")),
                (None, opened),
                "{layout:?}: `{path}` must turn the menu's own state",
            );
        }

        for (path, key) in [
            ("bar/menu/layouts-head", "bar/group-lay"),
            ("bar/menu/layout-1/apply", "ui.layout.apply@layout=1"),
            ("bar/menu/layout-2/apply", "ui.layout.apply@layout=2"),
            ("bar/menu/full-screen", "ui.window.toggle_full_screen"),
            ("bar/menu/cast", "broadcast.toggle"),
        ] {
            assert!(
                pressed.contains(&(path, key)),
                "{layout:?}: `{path}` must press `{key}`, got {pressed:?}",
            );
        }
    }
}

/// The app menu is a panel of switches: several of them are turned in one
/// opening, so a write it delivers leaves it standing.
#[kithara::test]
fn a_write_from_inside_the_app_menu_leaves_it_open() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();

        for (path, key) in [
            ("bar/menu/module-ov/cell", "ui.module.toggle@module=ov"),
            ("bar/menu/module-cpu/cell", "ui.module.toggle@module=cpu"),
            ("bar/menu/layout-2/apply", "ui.layout.apply@layout=2"),
            ("bar/menu/full-screen", "ui.window.toggle_full_screen"),
            ("bar/menu/cast", "broadcast.toggle"),
        ] {
            let mut view = ViewState::new();
            view.set("bar/menu", ViewSet::On);
            let host = settle(&ui, &mut view, path, ControlAction::Activate);
            assert_eq!(
                host,
                Some(UiEvent::Write {
                    key: key.to_owned(),
                    value: WriteValue::Trigger,
                }),
                "{layout:?}: `{path}` must deliver `{key}`",
            );
            assert!(
                view.flag("bar/menu"),
                "{layout:?}: `{path}` must leave the menu open",
            );
        }
    }
}

#[kithara::test]
fn the_layouts_head_opens_and_folds_the_layouts_block() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();
    let reads = BandReads {
        bands: None,
        seen: RefCell::default(),
    };
    let hidden = |view: &ViewState| {
        let mut hidden = None;
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Optional { block, .. } = node
                && ui.resolve(block.path) == "bar/menu/layouts-block"
            {
                let ctx = Ctx::new(&ui, &reads, view, builtin::skin_doc(), Clock::default());
                hidden = Some(ctx.flag(Some(&block.hidden)));
            }
        });
        hidden.unwrap_or_else(|| panic!("the app menu lays out its layouts block"))
    };
    let mut view = ViewState::new();
    assert!(hidden(&view), "the layouts group starts folded");

    let opened = settle(
        &ui,
        &mut view,
        "bar/menu/layouts-head",
        ControlAction::Activate,
    );
    assert_eq!(opened, None);
    assert!(!hidden(&view), "a press on its head opens the group");

    let folded = settle(
        &ui,
        &mut view,
        "bar/menu/layouts-head",
        ControlAction::Activate,
    );
    assert_eq!(folded, None);
    assert!(hidden(&view), "a second press folds it again");
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
                pressed.contains(&(cell.as_str(), format!("deck-{letter}/quality").as_str())),
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

#[kithara::test]
fn a_quality_choice_shuts_the_menu_it_was_picked_from() {
    let ui = compile_ui(DeckLayout::Dual).unwrap();

    for (row, variant) in [("auto", "auto"), ("variant-0", "0")] {
        let mut view = ViewState::new();
        view.set("deck-a/quality", ViewSet::On);
        view.set("deck-b/quality", ViewSet::On);
        let host = settle(
            &ui,
            &mut view,
            &format!("deck-a/stream/{row}/pick"),
            ControlAction::Activate,
        );
        assert_eq!(
            host,
            Some(UiEvent::Write {
                key: format!("deck.stream.select_variant@deck=a,variant={variant}"),
                value: WriteValue::Trigger,
            }),
        );
        assert!(!view.flag("deck-a/quality"), "`{row}` shuts its own menu");
        assert!(view.flag("deck-b/quality"), "`{row}` leaves deck B's open");
    }
}

/// The package this application ships is read from disk the way a release
/// reads it, so drift between the documents on disk and what the build
/// embeds cannot hide behind the embedded copy.
#[kithara::test]
fn the_shipped_package_compiles_from_disk() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let package =
        test_fixture::package(Some(&root)).expect("the shipped package must load from disk");
    let runtime = test_fixture::runtime();
    drop(
        AppUi::new(package, &UiConfig::default(), runtime.handle().clone())
            .expect("the shipped package must compile from disk"),
    );
}

#[kithara::test]
fn the_source_additions_list_exactly_the_registered_sources() {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    for root in [None, Some(shipped.as_path())] {
        for with_probe in [true, false] {
            let mut registered = vec![StartupSource::registered(Vec::new())];
            if with_probe {
                registered.push(Probe::registered("menu.module.library").0);
            }
            let (package, _) = test_fixture::mount(root, registered).expect("the sources mount");
            for layout in LAYOUTS {
                let ui = compile_package(&package, layout).expect("the mounted pages compile");
                let controls = controls(&ui);
                for (source, registered) in [("startup", true), (Probe::ID, with_probe)] {
                    let scoped = format!("@source={source}");
                    let bound: Vec<&str> = controls
                        .iter()
                        .filter(|(_, keys)| keys.iter().any(|key| key.ends_with(&scoped)))
                        .map(|(path, _)| *path)
                        .collect();
                    if registered {
                        assert!(
                            !bound.is_empty(),
                            "{root:?} {layout:?}: `{source}` must draw its page",
                        );
                    } else {
                        assert!(
                            bound.is_empty(),
                            "{root:?} {layout:?}: unregistered `{source}` is bound at {bound:?}",
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn a_fill_of_a_collection_no_slot_shows_fails_loading_the_package() {
    let page = SourcePage {
        id: "probe",
        endpoints: Vec::new(),
        texts: Vec::new(),
    };
    let registration = Registration::new(page, |_| {
        Err(UiDocError::NotFound {
            origin: SourceUri("probe".to_owned()),
            rel: String::new(),
        })
    })
    .fill(
        "app-library/missing",
        FillDocument::Path("modules/library/source-page.kmodule.ron".to_owned()),
    );

    let Err(error) = test_fixture::mount(
        None,
        vec![StartupSource::registered(Vec::new()), registration],
    ) else {
        panic!("a fill of a collection no slot shows must fail loading");
    };

    let message = error.to_string();
    assert!(
        message.contains("app-library/missing") && message.contains("probe"),
        "the error names the address and the plugin: {message}"
    );
}

/// A refused screen names each required write no control on it declares.
#[kithara::test]
fn a_write_the_screen_does_not_declare_is_named() {
    let ui = compile_ui(DeckLayout::Dual).expect("the shipped screen must compile");
    let origin = SourceUri("app.klayout.ron".to_owned());

    let error = ui
        .require_writes(
            &[
                "deck.transport.toggle_play@deck=a",
                "deck.transport.eject@deck=a",
            ],
            &origin,
        )
        .expect_err("a screen declaring no eject write must be refused");

    assert!(
        matches!(
            &error,
            UiDocError::MissingWrites { writes, .. } if writes == &["deck.transport.eject@deck=a"]
        ),
        "the refusal must name the write the screen does not declare, not {error}"
    );
}

/// Nothing laid out is not a defect: the documents this build carries draw,
/// which is what a developer running from a build directory sees.
#[kithara::test]
fn a_package_path_that_was_never_laid_out_leaves_the_built_in_documents_drawing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui-that-was-never-laid-out");
    let package = test_fixture::package(Some(&root)).expect("a package nobody laid out must load");
    let runtime = test_fixture::runtime();
    drop(
        AppUi::new(package, &UiConfig::default(), runtime.handle().clone())
            .expect("a package nobody laid out must leave the build drawing"),
    );
}

/// A package dresses the pages it ships: the skin its manifest names is the
/// one every page is compiled and painted against, which is what lets a
/// package change how the application looks without a rebuild.
#[kithara::test]
fn the_skin_the_manifest_names_is_the_one_the_pages_wear() {
    let root = tempfile::tempdir().expect("a temporary package root");
    std::fs::write(
        root.path().join("package.kpackage.ron"),
        r#"(
    schema: "kithara.package",
    version: 1,
    id: "kithara-app",
    contract: 1,
    skin: "kithara-neon.kskin.ron",
    text: "app-en.ktext.ron",
    screens: {
        "deck-dual": "app.klayout.ron",
        "deck-single": "app-single.klayout.ron",
    },
)"#,
    )
    .expect("the manifest must be written");
    let package =
        test_fixture::package(Some(root.path())).expect("a package naming a skin must load");
    assert_eq!(
        package.skin().document().id.0,
        "kithara-neon",
        "the manifest names the neon skin"
    );
}

/// A package that names no skin wears the built-in one rather than refusing to
/// load, so a package may carry pages and nothing else.
#[kithara::test]
fn a_package_naming_no_skin_wears_the_built_in_one() {
    let root = tempfile::tempdir().expect("a temporary package root");
    std::fs::write(
        root.path().join("package.kpackage.ron"),
        r#"(
    schema: "kithara.package",
    version: 1,
    id: "kithara-app",
    contract: 1,
    screens: {
        "deck-dual": "app.klayout.ron",
        "deck-single": "app-single.klayout.ron",
    },
)"#,
    )
    .expect("the manifest must be written");
    let package =
        test_fixture::package(Some(root.path())).expect("a package naming no skin must load");
    assert_eq!(
        package.skin().document().id,
        builtin::skin().document().id,
        "a package naming no skin wears the built-in one"
    );
}

/// What the disk says about a role wins over what the build embeds: a manifest
/// laid out beside the executable is the one that answers.
#[kithara::test]
fn a_manifest_on_disk_answers_before_the_one_this_build_embeds() {
    let root = tempfile::tempdir().expect("a temporary package root");
    std::fs::write(
        root.path().join("package.kpackage.ron"),
        r#"(
    schema: "kithara.package",
    version: 1,
    id: "kithara-app",
    contract: 1,
    screens: {
        "deck-dual": "app.klayout.ron",
    },
)"#,
    )
    .expect("the manifest must be written");
    let Err(error) = test_fixture::package(Some(root.path())) else {
        panic!("the disk manifest names one role only, so this must not compile");
    };
    assert!(
        matches!(&error, UiDocError::MissingRole { role, .. } if role == "deck-single"),
        "the disk manifest must be the one asked for the missing role, got {error}"
    );
}

fn edited_package(edits: &[(&str, Edit)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temporary package root");
    for (path, edit) in edits {
        let shipped = super::package::embedded()
            .load(None, path)
            .unwrap_or_else(|error| panic!("`{path}` must ship: {error}"))
            .text;
        let edited = edit(&shipped);
        assert_ne!(edited, shipped, "the edit must change `{path}`");
        let file = root.path().join(path);
        std::fs::create_dir_all(file.parent().expect("a document sits in a folder"))
            .expect("the folder must be made");
        std::fs::write(file, edited).expect("the document must be written");
    }
    root
}

fn without_write(text: &str, id: &str) -> String {
    let declared = format!("write: Command(id: \"{id}\"");
    text.lines()
        .filter(|line| !line.contains(&declared))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn compiled_from(root: &Path) -> Result<AppUi, UiDocError> {
    let package = test_fixture::package(Some(root)).expect("the edited package must load");
    AppUi::new(
        package,
        &UiConfig::default(),
        test_fixture::runtime().handle().clone(),
    )
}

#[kithara::test]
fn a_package_renaming_the_play_control_still_compiles() {
    let root = edited_package(&[("modules/app-deck.kmodule.ron", |text| {
        text.replace("id: \"play\"", "id: \"start\"")
    })]);

    if let Err(error) = compiled_from(root.path()) {
        panic!("a renamed play control keeps the write the app requires: {error}");
    }
}

#[kithara::test]
fn a_package_that_cannot_play_deck_a_is_refused_by_the_write_it_lacks() {
    let root = edited_package(&[
        ("modules/app-deck.kmodule.ron", |text| {
            without_write(text, "deck.transport.toggle_play")
        }),
        ("modules/app-bar-micro.kmodule.ron", |text| {
            without_write(text, "deck.transport.toggle_play")
        }),
    ]);

    let Err(error) = compiled_from(root.path()) else {
        panic!("a screen that cannot start deck A must be refused");
    };
    assert!(
        matches!(
            &error,
            UiDocError::MissingWrites { writes, .. } if writes == &["deck.transport.toggle_play@deck=a"]
        ),
        "the refusal must name the write the screen lacks, not {error}"
    );
}

#[kithara::test]
fn a_package_that_cannot_seek_deck_a_is_refused_by_the_write_it_lacks() {
    let root = edited_package(&[
        ("modules/app-deck.kmodule.ron", |text| {
            without_write(text, "deck.transport.seek_normalized")
        }),
        ("modules/app-bar-micro.kmodule.ron", |text| {
            without_write(text, "deck.transport.seek_normalized")
        }),
        ("modules/deck/overview-row.kmodule.ron", |text| {
            without_write(text, "deck.transport.seek_normalized")
        }),
    ]);

    let Err(error) = compiled_from(root.path()) else {
        panic!("a screen that cannot move deck A's position must be refused");
    };
    assert!(
        matches!(
            &error,
            UiDocError::MissingWrites { writes, .. } if writes == &["deck.transport.seek_normalized@deck=a"]
        ),
        "the refusal must name the write the screen lacks, not {error}"
    );
}

const fn publishes(spec: &ControlSpec) -> bool {
    matches!(
        spec,
        ControlSpec::Button { .. }
            | ControlSpec::NavItem { .. }
            | ControlSpec::TabLarge { .. }
            | ControlSpec::Toggle
            | ControlSpec::Checkbox
            | ControlSpec::Chip { .. }
            | ControlSpec::SettingsButton
            | ControlSpec::Crossfader { .. }
            | ControlSpec::Fader { .. }
            | ControlSpec::Knob { .. }
            | ControlSpec::Vis
            | ControlSpec::VuStereo
            | ControlSpec::VuVertical { .. }
            | ControlSpec::Wave { .. }
            | ControlSpec::Range
            | ControlSpec::Segmented { .. }
            | ControlSpec::ContextBar { .. }
            | ControlSpec::Tree { .. }
            | ControlSpec::Select { .. }
            | ControlSpec::PresetSelector
    )
}

#[kithara::test]
fn every_control_the_studio_draws_declares_the_write_it_makes() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let mut unbound = Vec::new();
        each_node(&ui, &mut |node| {
            if let ExpandedNode::Control {
                path,
                spec,
                write: None,
                ..
            } = node
                && publishes(spec)
            {
                unbound.push(ui.resolve(*path));
            }
        });
        assert!(
            unbound.is_empty(),
            "{layout:?}: these controls publish a gesture no write answers: {unbound:?}"
        );
    }
}

#[cfg(not(feature = "broadcast"))]
mod answered {
    use std::collections::{BTreeMap, BTreeSet};

    use ::kithara::ui::{
        expand::{Binding, BindingKind},
        geom::Pt,
        render::{Scope, WindowCommand},
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        deck::{DeckId, EqMode},
        engine::MixCmd,
        gui::{
            app::Kithara,
            deck::DeckMsg,
            message::Message,
            rig::Rig,
            ui::{events::translate, scope::deck_index},
        },
        state::AbrVariant,
    };

    fn host_writes(ui: &CompiledUi) -> BTreeMap<String, WriteValue> {
        let reads = BandReads {
            bands: None,
            seen: RefCell::default(),
        };
        let mut paths: Vec<String> = control_paths(ui).into_iter().map(str::to_owned).collect();
        paths.extend(
            drop_targets(ui)
                .into_iter()
                .map(|(instance, _)| format!("{instance}/drop")),
        );
        each_node(ui, &mut |node| match node {
            ExpandedNode::Control {
                path,
                spec: ControlSpec::Wave { .. },
                ..
            } => paths.push(format!("{}/zoom", ui.resolve(*path))),
            ExpandedNode::Control {
                path,
                spec: ControlSpec::Tree { .. },
                ..
            } => paths.push(format!("{}/toggle", ui.resolve(*path))),
            _ => {}
        });
        let actions = [
            ControlAction::Activate,
            ControlAction::SecondaryActivate,
            ControlAction::SetScalar(0.5),
            ControlAction::StepScalar(1.0),
            ControlAction::SelectIndex(0),
            ControlAction::Text(String::new()),
            ControlAction::Record(test_fixture::dragged("https://example.com/a.mp3")),
            ControlAction::Place(Pt { x: 0.0, y: 0.0 }),
        ];
        let mut out = BTreeMap::new();
        for path in paths {
            for action in &actions {
                let published = Published::Gesture {
                    action: action.clone(),
                    path: path.clone(),
                };
                if let Some(UiEvent::Write { key, value }) =
                    ui.views().settle(published, &reads, &mut ViewState::new())
                {
                    out.insert(key, value);
                }
            }
        }
        out
    }

    fn tree_writes(ui: &CompiledUi) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        let mut note = |binding: &Binding| {
            if !matches!(
                binding.kind,
                BindingKind::View { .. } | BindingKind::Page { .. }
            ) {
                out.insert(ui.resolve(binding.key));
            }
        };
        each_node(ui, &mut |node| match node {
            ExpandedNode::Control {
                write: Some(write), ..
            } => note(write),
            ExpandedNode::Pressable { press, .. } => note(press),
            ExpandedNode::Row {
                surface: Some(surface),
                ..
            }
            | ExpandedNode::Column {
                surface: Some(surface),
                ..
            } => note(&surface.write),
            _ => {}
        });
        out
    }

    fn drawn_tree(state: &Kithara) -> Vec<(u8, String, Option<bool>, bool)> {
        state
            .library
            .tree()
            .into_iter()
            .map(|row| (row.depth, row.label.to_owned(), row.expanded, row.selected))
            .collect()
    }

    fn zoom(state: &mut Kithara, deck: Option<usize>) -> Option<f64> {
        state.ui.cache.deck_mut(deck?)?.view.zoom
    }

    fn answers(key: &str, value: WriteValue) -> bool {
        let (id, scope) = Scope::split(key);
        let deck = scope.get("deck").and_then(deck_index);
        let mut rig = Rig::offline();
        if matches!(id, "deck.eq.low_mid" | "deck.eq.high_mid") {
            rig.message(Message::SetEqMode(EqMode::FourBand));
            rig.pump();
            rig.ui.refresh();
        }
        if let (Some(index), Some(slot)) = (
            deck,
            scope.get("variant").and_then(|slot| slot.parse().ok()),
        ) {
            rig.shows(DeckId(index), |shown| {
                shown.abr_variants = (0..=slot)
                    .map(|index| AbrVariant {
                        index,
                        label: String::new(),
                        detail: String::new(),
                    })
                    .collect();
            });
        }
        let state = &mut rig.ui;
        if let (Some(index), "deck.queue.load") = (deck, id) {
            state.ui.cache.focus(1 - index);
        }
        let zoomed = zoom(state, deck);
        let module = scope.get("module");
        let shown = module.map(|module| state.ui.cache.modules.is_on(module));
        // Row 1 is selected at mount and opens nothing; row 2 does both.
        let value = match id {
            "library.select" | "library.toggle" => WriteValue::Index(2),
            _ => value,
        };
        let tree = drawn_tree(state);
        let row = match value {
            WriteValue::Index(row) => Some(row),
            _ => None,
        };

        let answer = translate(
            state,
            UiEvent::Write {
                value,
                key: key.to_owned(),
            },
        );

        match id {
            "deck.view.zoom" | "deck.view.zoom_in" | "deck.view.zoom_out" => {
                answer.is_none() && zoom(state, deck) != zoomed
            }
            "deck.queue.load" => answer.is_none() && Some(state.ui.cache.focus_deck()) == deck,
            "ui.module.toggle" => {
                answer.is_none()
                    && module.map(|module| state.ui.cache.modules.is_on(module)) != shown
            }
            "ui.layout.apply" => matches!(answer, Some(Message::PauseHiddenDecks)),
            "ui.window.toggle_full_screen" => matches!(
                answer,
                Some(Message::Window(WindowCommand::ToggleFullScreen))
            ),
            "broadcast.toggle" => matches!(answer, Some(Message::BroadcastToggle)),
            "ui.library.add_folder" => matches!(answer, Some(Message::AddMusicFolder)),
            "mix.crossfader" => matches!(answer, Some(Message::Mix(MixCmd::Crossfader(_)))),
            "mixer.trim" => {
                matches!(answer, Some(Message::Mix(MixCmd::Trim(DeckId(index), _))) if Some(index) == deck)
            }
            "library.select" => {
                answer.is_none()
                    && state.library.tree().get(2).is_some_and(|row| row.selected)
                    && drawn_tree(state) != tree
            }
            "library.toggle" => answer.is_none() && drawn_tree(state) != tree,
            "source.select" => {
                let Some(row) = row else {
                    return false;
                };
                answer.is_none()
                    && state
                        .library
                        .sources()
                        .enumerate()
                        .find(|(_, source)| Some(source.id()) == scope.get("source"))
                        .is_some_and(|(at, source)| {
                            state.library.selected_row(at) == source.row_key(row)
                        })
            }
            "deck.eq.mode" => matches!(answer, Some(Message::SetEqMode(_))),
            _ => {
                let Some(Message::Deck(DeckId(index), msg)) = answer else {
                    return false;
                };
                Some(index) == deck
                    && match id {
                        "deck.transport.toggle_play" => matches!(msg, DeckMsg::TogglePlayPause),
                        "deck.transport.prev" => matches!(msg, DeckMsg::Prev),
                        "deck.transport.next" => matches!(msg, DeckMsg::Next),
                        "deck.transport.seek_normalized" => matches!(msg, DeckMsg::SeekTo(_)),
                        "deck.tempo.rate" | "deck.tempo.reset" => {
                            matches!(msg, DeckMsg::SetTempo(_))
                        }
                        "deck.eq.low" | "deck.eq.low_mid" | "deck.eq.mid" | "deck.eq.high_mid"
                        | "deck.eq.high" => matches!(msg, DeckMsg::EqBandChanged(..)),
                        "deck.stream.select_variant" => matches!(msg, DeckMsg::SetQuality(_)),
                        _ => panic!("`{key}` is declared and names no answer this test knows"),
                    }
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn every_write_the_studio_declares_is_answered() {
        let mut declared = BTreeMap::new();
        for layout in LAYOUTS {
            let ui = compile_ui(layout).unwrap();
            let writes = host_writes(&ui);
            let unreached: Vec<&str> = tree_writes(&ui)
                .into_iter()
                .filter(|key| !writes.contains_key(*key))
                .collect();
            assert!(
                unreached.is_empty(),
                "{layout:?}: no gesture reaches {unreached:?}"
            );
            declared.extend(writes);
        }
        for key in [
            "deck.tempo.reset@deck=a",
            "deck.view.zoom@deck=a",
            "deck.queue.load@deck=b",
            "library.select",
            "library.toggle",
        ] {
            assert!(declared.contains_key(key), "the studio declares `{key}`");
        }

        let unanswered: Vec<String> = declared
            .into_iter()
            .filter(|(key, value)| !answers(key, value.clone()))
            .map(|(key, _)| key)
            .collect();
        assert!(
            unanswered.is_empty(),
            "the app leaves these declared writes unanswered: {unanswered:?}"
        );
    }
}

fn custom_search_source() -> Registration {
    let origin = SourceUri("modules/custom-search.kmodule.ron".to_owned());
    let page = FillDocument::parse(
        r#"(
    schema: "kithara.module", version: 1, id: "custom-search", chrome: Plain, parameters: ["source"],
    root: Column(id: "body", children: [
        Text(id: "caption", label: "@probe.caption"),
        Search(
            id: "query",
            read: Model(id: "source.query", with: { "source": "$source" }),
            write: Command(id: "source.query", with: { "source": "$source" }),
        ),
    ]),
)"#, origin)
    .expect("the probe page parses");
    Registration::new(
        SourcePage {
            id: Probe::ID,
            texts: vec![Document {
                path: "texts/probe.ktext.ron",
                text: r#"(schema: "kithara.text", version: 1, id: "probe", entries: { "probe.caption": "Catalogue" })"#,
            }],
            endpoints: [EndpointCategory::Model, EndpointCategory::Command]
                .into_iter()
                .map(|category| Endpoint {
                    category,
                    name: "query",
                    value: ValueKind::Text,
                })
                .collect(),
        },
        |text| Probe::registered("menu.module.library").0.build(text),
    )
    .fill(PAGES, page)
}

/// A gesture on a source's page reaches that source through the app's write
/// dispatch, and the read root answers the source's read under the same scope.
#[kithara::test]
fn a_source_page_gesture_reaches_its_source_through_the_app_dispatch() {
    let runtime = test_fixture::runtime();
    let (package, library) = test_fixture::mount(None, vec![custom_search_source()])
        .expect("the probe source mounts with its page");
    let mut state = test_fixture::mounted(package, library, runtime.handle());

    drop(update(
        &mut state,
        Message::Ui(Published::Gesture {
            path: "library/pages/probe/query".to_owned(),
            action: ControlAction::Text("needle".to_owned()),
        }),
    ));

    assert_eq!(
        Walk::new(&ReadRoot::new(&state)).get("source.query@source=probe"),
        Some(ReadValue::Text("needle"))
    );
}
