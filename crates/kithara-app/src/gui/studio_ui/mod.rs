mod cache;
mod endpoints;
mod events;
mod reads;
mod scope;

use iced::Element;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::tree,
    source::UiConfig,
};

use self::reads::StudioReads;
pub(crate) use self::{
    cache::{DeckLayout, StudioCache},
    events::translate,
};
use super::{app::Kithara, message::Message};

const COMPILES: &str = "embedded studio documents must compile";

const DOCS: &[(&str, &str)] = &[
    (
        "studio.klayout.ron",
        include_str!("../../../assets/ui/studio.klayout.ron"),
    ),
    (
        "studio-single.klayout.ron",
        include_str!("../../../assets/ui/studio-single.klayout.ron"),
    ),
    (
        "modules/studio-bar.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-bar.kmodule.ron"),
    ),
    (
        "modules/studio-deck.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-deck.kmodule.ron"),
    ),
    (
        "modules/studio-overview.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview.kmodule.ron"),
    ),
    (
        "modules/studio-overview-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview-single.kmodule.ron"),
    ),
    (
        "modules/studio-overview-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview-row.kmodule.ron"),
    ),
    (
        "modules/studio-mixer.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-mixer.kmodule.ron"),
    ),
    (
        "modules/studio-mixer-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-mixer-single.kmodule.ron"),
    ),
    (
        "modules/studio-strip.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip.kmodule.ron"),
    ),
    (
        "modules/studio-library.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-library.kmodule.ron"),
    ),
];

/// The compiled studio UI plus the host-owned view state it reads back. Both
/// deck layouts are compiled once; the top bar picks which one renders.
pub(crate) struct StudioUi {
    single: CompiledUi,
    dual: CompiledUi,
    pub(crate) cache: StudioCache,
}

impl StudioUi {
    /// Compile the embedded studio documents. Panicking here is sanctioned:
    /// the documents are compile-time assets validated by unit tests, so a
    /// failure is a build defect, not a runtime condition.
    pub(crate) fn new() -> Self {
        Self {
            single: compile_studio(DeckLayout::Single).expect(COMPILES),
            dual: compile_studio(DeckLayout::Dual).expect(COMPILES),
            cache: StudioCache::default(),
        }
    }

    fn compiled(&self, layout: DeckLayout) -> &CompiledUi {
        match layout {
            DeckLayout::Single => &self.single,
            DeckLayout::Dual => &self.dual,
        }
    }
}

fn compile_studio(layout: DeckLayout) -> Result<CompiledUi, kithara_ui::error::UiDocError> {
    let mut resolver = builtin::resolver();
    for (path, text) in DOCS {
        resolver.insert(path, text);
    }
    let entry = match layout {
        DeckLayout::Single => "studio-single.klayout.ron",
        DeckLayout::Dual => "studio.klayout.ron",
    };
    compile(
        entry,
        &resolver,
        &endpoints::StudioRegistry::new(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let reads = StudioReads::new(state);
    let compiled = state.studio.compiled(state.studio.cache.layout);
    tree::render(&compiled.root, compiled, &reads, builtin::skin()).map(Message::Ui)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::CompiledNode,
        expand::{ControlSpec, ExpandedNode},
        module::TextStyle,
    };

    use super::*;

    fn each_control(ui: &CompiledUi, visit: &mut impl FnMut(&ExpandedNode)) {
        fn walk(node: &ExpandedNode, visit: &mut impl FnMut(&ExpandedNode)) {
            match node {
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. } => {
                    for child in children {
                        walk(child, visit);
                    }
                }
                control @ ExpandedNode::Control { .. } => visit(control),
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

    /// Every control in the compiled studio, as `(path, scoped binding keys)`.
    fn controls(ui: &CompiledUi) -> Vec<(&str, Vec<&str>)> {
        let mut out = Vec::new();
        each_control(ui, &mut |node| {
            if let ExpandedNode::Control {
                path, read, write, ..
            } = node
            {
                let keys = [read.as_ref(), write.as_ref()]
                    .into_iter()
                    .flatten()
                    .map(|binding| ui.resolve(binding.key()))
                    .collect();
                out.push((ui.resolve(*path), keys));
            }
        });
        out
    }

    fn control_paths(ui: &CompiledUi) -> Vec<&str> {
        let mut out = Vec::new();
        each_control(ui, &mut |node| {
            if let ExpandedNode::Control { path, .. } = node {
                out.push(ui.resolve(*path));
            }
        });
        out
    }

    /// Labels the segmented control at `want` declares, in document order.
    fn segments<'a>(ui: &'a CompiledUi, want: &str) -> Vec<&'a str> {
        let mut found = Vec::new();
        each_control(ui, &mut |node| {
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

    const LAYOUTS: [DeckLayout; 2] = [DeckLayout::Single, DeckLayout::Dual];

    #[kithara::test]
    fn studio_documents_compile_against_the_registry() {
        for layout in LAYOUTS {
            compile_studio(layout).unwrap();
        }
    }

    /// A deck-scoped control is routed by the letter in its path, and read by
    /// the letter in its binding scope. The two must be the same letter, or
    /// the control silently reads one deck and writes another.
    #[kithara::test]
    fn deck_scoped_controls_are_routed_to_the_deck_they_read() {
        for layout in LAYOUTS {
            let ui = compile_studio(layout).unwrap();
            for (path, keys) in controls(&ui) {
                for key in keys {
                    let Some(letter) = key
                        .split_once('@')
                        .and_then(|(_, scope)| scope.strip_prefix("deck="))
                    else {
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

    /// The CPU cell shows engine load twice — as a bar and as a percentage —
    /// so both must read the same endpoint.
    #[kithara::test]
    fn the_cpu_cell_reads_engine_load_as_a_bar_and_a_number() {
        for layout in LAYOUTS {
            let ui = compile_studio(layout).unwrap();
            let mut bars = Vec::new();
            each_control(&ui, &mut |node| {
                if let ExpandedNode::Control {
                    path,
                    spec: ControlSpec::Meter,
                    read: Some(read),
                    ..
                } = node
                {
                    bars.push((ui.resolve(*path), ui.resolve(read.key())));
                }
            });

            assert_eq!(bars, [("bar/cpu-bar", "engine.load")]);
            assert!(
                controls(&ui).contains(&("bar/cpu-value", vec!["engine.load"])),
                "{layout:?}: the CPU readout must stay on engine.load",
            );
        }
    }

    /// The studio window carries no system decorations, so the bar owns the
    /// window chrome: a drag surface and the three window commands.
    #[kithara::test]
    fn the_bar_owns_the_window_chrome() {
        for layout in LAYOUTS {
            let ui = compile_studio(layout).unwrap();
            let mut seen = Vec::new();
            each_control(&ui, &mut |node| {
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
        }
    }

    #[kithara::test]
    fn every_channel_strip_carries_the_supported_control_set() {
        let ui = compile_studio(DeckLayout::Dual).unwrap();
        let paths = control_paths(&ui);
        for letter in ["a", "b"] {
            for name in ["low", "mid", "high", "tempo", "volume"] {
                let want = format!("mixer/{letter}/{name}");
                assert!(paths.contains(&want.as_str()), "missing control `{want}`");
            }
        }
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
    fn tempo_and_volume_controls_bind_to_the_deck_mixer_state() {
        let ui = compile_studio(DeckLayout::Dual).unwrap();
        let controls = controls(&ui);
        for letter in ["a", "b"] {
            for (name, endpoint) in [("tempo", "deck.ts.tempo"), ("volume", "mixer.trim")] {
                let want = format!("mixer/{letter}/{name}");
                let (_, keys) = controls
                    .iter()
                    .find(|(path, _)| *path == want)
                    .unwrap_or_else(|| panic!("missing control `{want}`"));
                let binding = format!("{endpoint}@deck={letter}");
                assert!(
                    keys.contains(&binding.as_str()),
                    "`{want}` must bind `{binding}`, got {keys:?}"
                );
            }
        }
    }

    /// A layout lays out exactly the decks it declares: body, overview row and
    /// channel strip, and nothing addressing a deck it hides.
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

    /// The top-bar switch offers exactly the layouts the host can select, in
    /// the order the host indexes them.
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

    /// A deck letter caption names the deck its control path routes to.
    #[kithara::test]
    fn deck_letter_captions_name_their_deck() {
        for layout in LAYOUTS {
            let ui = compile_studio(layout).unwrap();
            let mut seen = 0_usize;
            each_control(&ui, &mut |node| {
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

    #[kithara::test]
    fn layout_indices_round_trip() {
        for layout in LAYOUTS {
            assert_eq!(DeckLayout::from_index(layout.index()), Some(layout));
        }
        assert_eq!(DeckLayout::from_index(LAYOUTS.len()), None);
    }
}
