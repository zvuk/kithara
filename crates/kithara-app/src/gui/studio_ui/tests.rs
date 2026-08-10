use kithara_test_utils::kithara;
use kithara_ui::{
    compile::{CompiledNode, CompiledUi},
    expand::{ControlSpec, ExpandedNode},
    module::{TextAlign, TextStyle},
};

use super::{cache::DeckLayout, compile::compile_studio};
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
            ExpandedNode::Optional { child, .. } | ExpandedNode::Pressable { child, .. } => {
                walk(child, visit);
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
            CompiledNode::Module { root, .. } => walk(root, visit),
            _ => {}
        }
    }
}

/// Every addressed node in the compiled studio, as
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

/// Labels the segmented control at `want` declares, in document order.
fn segments<'a>(ui: &'a CompiledUi, want: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    each_node(ui, &mut |node| {
        if let ExpandedNode::Control {
            path,
            spec: ControlSpec::Segmented { items },
            ..
        } = node
            && ui.resolve(*path) == want
        {
            found = items.iter().map(|item| ui.resolve(*item)).collect();
        }
    });
    found
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
fn studio_documents_compile_against_the_registry() {
    for layout in LAYOUTS {
        compile_studio(layout).unwrap();
    }
}

#[kithara::test]
fn deck_scoped_controls_are_routed_to_the_deck_they_read() {
    for layout in LAYOUTS {
        let ui = compile_studio(layout).unwrap();
        for (path, keys) in controls(&ui) {
            for key in keys {
                let Some(letter) = key.split_once('@').and_then(|(_, scope)| {
                    scope.split(',').find_map(|pair| pair.strip_prefix("deck="))
                }) else {
                    continue;
                };
                let routed = [
                    format!("deck-{letter}/"),
                    format!("mixer/{letter}/"),
                    format!("overview/{letter}/"),
                ];
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
        let ui = compile_studio(layout).unwrap();
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
        let ui = compile_studio(layout).unwrap();

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
        let ui = compile_studio(layout).unwrap();
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

        assert_eq!(seen, ["bar/drag", "bar/window"], "{layout:?}");
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
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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

#[kithara::test]
fn every_eq_bank_carries_its_pointer_menu() {
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
fn studio_hides_controls_outside_the_supported_playback_contract() {
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
    let ui = compile_studio(DeckLayout::Dual).unwrap();
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
        let ui = compile_studio(layout).unwrap();
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
        let ui = compile_studio(layout).unwrap();
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
fn the_deck_switch_offers_every_layout_in_order() {
    for layout in LAYOUTS {
        let ui = compile_studio(layout).unwrap();
        let items = segments(&ui, "bar/decks");
        assert_eq!(items.len(), LAYOUTS.len());
        for (offered, label) in [(DeckLayout::Single, "1"), (DeckLayout::Dual, "2")] {
            assert_eq!(items.get(offered.index()).copied(), Some(label));
        }
    }
}

#[kithara::test]
fn deck_letter_captions_name_their_deck() {
    for layout in LAYOUTS {
        let ui = compile_studio(layout).unwrap();
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

#[kithara::test]
fn each_deck_picks_its_own_stream_quality() {
    for layout in LAYOUTS {
        let ui = compile_studio(layout).unwrap();
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
