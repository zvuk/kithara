use std::{cell::RefCell, collections::BTreeSet};

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi},
    expand::{ControlSpec, ExpandedNode},
    module::{MeasureAxis, TextAlign, TextStyle},
    render::{ReadValue, Reads, tree},
    size::{Dim, SizeSpec},
};

use super::{cache::DeckLayout, compile::compile_ui, scope::MICRO_DECK};
use crate::gui::frontend::consts::{WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH};
const LAYOUTS: [DeckLayout; 2] = [DeckLayout::Single, DeckLayout::Dual];

fn each_node(ui: &CompiledUi, visit: &mut impl FnMut(&ExpandedNode)) {
    fn walk(node: &ExpandedNode, visit: &mut impl FnMut(&ExpandedNode)) {
        visit(node);
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    walk(child, visit);
                }
            }
            ExpandedNode::Optional { child, .. }
            | ExpandedNode::Pressable { child, .. }
            | ExpandedNode::Reveal { child, .. }
            | ExpandedNode::Scroll { child, .. } => {
                walk(child, visit);
            }
            ExpandedNode::Adaptive { base, steps, .. } => {
                walk(base, visit);
                for (_, branch) in steps {
                    walk(branch, visit);
                }
            }
            ExpandedNode::Popover {
                anchor, content, ..
            } => {
                walk(anchor, visit);
                walk(content, visit);
            }
            _ => {}
        }
    }

    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|(_, child)| child));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Adaptive { base, steps, .. } => {
                stack.push(base);
                stack.extend(steps.iter().map(|(_, branch)| branch));
            }
            CompiledNode::Module { root, .. } => walk(root, visit),
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
                stack.extend(children.iter().map(|(_, child)| child));
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
const WIDE_FROM: f32 = 1080.0;

/// Every module instance a branch lays out, sorted; a module standing in two
/// branches of one adaptive node is named once per branch.
fn instances<'a>(ui: &'a CompiledUi, node: &'a CompiledNode) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|(_, child)| child));
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
                stack.extend(children.iter().map(|(_, child)| child));
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

/// The direct cells of a bar, as `(threshold, address)` in document order.
fn bar_cells<'a>(ui: &'a CompiledUi, instance: &str) -> Vec<(Option<f32>, &'a str)> {
    let CompiledNode::Module { root, .. } = module(ui, instance) else {
        panic!("`{instance}` is no module");
    };
    let ExpandedNode::Row {
        measure, children, ..
    } = &**root
    else {
        panic!("`{instance}` must be rooted in a row");
    };
    assert_eq!(
        *measure,
        Some(MeasureAxis::Width),
        "`{instance}` answers a threshold only while it measures its own width",
    );
    children
        .iter()
        .map(|child| match child {
            ExpandedNode::Reveal { from, .. } => (Some(*from), cell_id(ui, child)),
            _ => (None, cell_id(ui, child)),
        })
        .collect()
}

/// The first segment of an address names the layout instance that answers it,
/// and `gui::ui::events::control` routes exactly these; a document minting a
/// new one needs a new arm there or its controls go nowhere.
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

        let mut want = vec!["bar", "bar-micro", "deck-a", "library", "mixer", "overview"];
        if layout == DeckLayout::Dual {
            want.push("deck-b");
        }
        want.sort_unstable();
        assert_eq!(named, want, "{layout:?}");
    }
}

/// The window draws one of two shapes, picked by the width it is given, and
/// answers the box it declares whichever it draws.
#[kithara::test]
fn the_window_takes_its_shape_from_the_width_it_is_given() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let CompiledNode::Adaptive {
            axis, size, steps, ..
        } = &ui.root
        else {
            panic!("{layout:?}: expected an adaptive root, got {:?}", ui.root);
        };

        assert_eq!(*axis, MeasureAxis::Width, "{layout:?}");
        assert_eq!(*size, SizeSpec::new(Dim::Fill, Dim::Fill), "{layout:?}");
        assert_eq!(ui.size, SizeSpec::new(Dim::Fill, Dim::Fill), "{layout:?}");
        assert_eq!(steps.len(), 1, "{layout:?}");
        assert_eq!(steps[0].0, WIDE_FROM, "{layout:?}");
    }
}

/// A window too narrow for both deck panes lays out the bar and the browser;
/// the decks, the mixer and the overview row leave with the shape they belong
/// to.
#[kithara::test]
fn a_narrow_window_lays_out_the_bar_and_the_browser() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let CompiledNode::Adaptive { base, steps, .. } = &ui.root else {
            panic!("{layout:?}: expected an adaptive root");
        };

        assert_eq!(
            instances(&ui, base),
            ["bar-micro", "bar-micro", "library"],
            "{layout:?}",
        );
        let wide = instances(&ui, &steps[0].1);
        let mut want = vec!["bar", "library", "mixer", "overview", "deck-a"];
        if layout == DeckLayout::Dual {
            want.push("deck-b");
        }
        want.sort_unstable();
        assert_eq!(wide, want, "{layout:?}");
    }
}

/// The micro bar takes its cells as the window makes room for them; the menu,
/// play, the drag strip and the window controls stand at every width.
#[kithara::test]
fn the_micro_bar_reveals_its_cells_as_the_window_widens() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();

        assert_eq!(
            bar_cells(&ui, "bar-micro"),
            [
                (None, "bar-micro/menu/pop"),
                (None, "bar-micro/play"),
                (None, "bar-micro/drag"),
                (Some(350.0), "bar-micro/wave"),
                (Some(440.0), "bar-micro/remain"),
                (None, "bar-micro/before-window"),
                (None, "bar-micro/window"),
            ],
            "{layout:?}",
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
            bar_cells(&ui, "bar"),
            [
                (None, "bar/menu/pop"),
                (Some(1250.0), "bar/brand"),
                (None, "bar/drag"),
                (Some(1120.0), "bar/cpu-block"),
                (None, "bar/broadcast-block"),
                (None, "bar/before-window"),
                (None, "bar/window"),
            ],
            "{layout:?}",
        );
    }
}

/// The browser panel joins the micro bar once the window is as tall as the two
/// of them together, so the threshold is that form's own minimum height.
#[kithara::test]
fn the_browser_panel_stands_once_the_window_is_tall_enough_for_it() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let CompiledNode::Adaptive { base, .. } = &ui.root else {
            panic!("{layout:?}: expected an adaptive root");
        };
        let CompiledNode::Adaptive {
            axis,
            base: bar_only,
            steps,
            ..
        } = &**base
        else {
            panic!("{layout:?}: the micro shape must answer the height it is given");
        };

        assert_eq!(*axis, MeasureAxis::Height, "{layout:?}");
        assert_eq!(instances(&ui, bar_only), ["bar-micro"], "{layout:?}");
        assert_eq!(steps.len(), 1, "{layout:?}");
        let (from, tall) = &steps[0];
        assert_eq!(instances(&ui, tall), ["bar-micro", "library"], "{layout:?}");
        let CompiledNode::Split { size, .. } = tall else {
            panic!("{layout:?}: the tall form stacks the bar over the panel");
        };
        assert_eq!(*from, size.h.min(), "{layout:?}");
    }
}

/// The window may be squeezed to the micro bar and no further: below its own
/// minimum the cells that make the window usable would overflow the bar.
#[kithara::test]
fn the_window_minimum_holds_the_micro_bar() {
    for layout in LAYOUTS {
        let ui = compile_ui(layout).unwrap();
        let CompiledNode::Module { size, .. } = module(&ui, "bar-micro") else {
            panic!("{layout:?}: `bar-micro` is no module");
        };

        assert!(
            WINDOW_MIN_WIDTH >= size.w.min(),
            "{layout:?}: a window of {WINDOW_MIN_WIDTH} overflows a bar of {}",
            size.w.min(),
        );
        assert!(
            WINDOW_MIN_HEIGHT >= size.h.min(),
            "{layout:?}: a window of {WINDOW_MIN_HEIGHT} overflows a bar of {}",
            size.h.min(),
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
                if dual { 263 } else { 205 },
            ),
            (
                "control_paths",
                control_paths(&ui).len(),
                if dual { 263 } else { 205 },
            ),
            ("surfaces", surfaces(&ui).len(), layout.decks()),
            (
                "pressables",
                pressables(&ui).len(),
                if dual { 59 } else { 48 },
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
                    routed.push("bar-micro/".to_owned());
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
/// the micro bar stands in both branches of the height it answers, so it names
/// its pair twice.
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

        assert_eq!(
            seen,
            [
                "bar-micro/drag",
                "bar-micro/drag",
                "bar-micro/window",
                "bar-micro/window",
                "bar/drag",
                "bar/window",
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
                stack.extend(children.iter().map(|(_, child)| child));
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
                for (_, child) in children {
                    walk(child, ui, key, guard, out);
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
