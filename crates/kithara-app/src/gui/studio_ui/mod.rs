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

use self::{cache::DeckLayout, reads::StudioReads};
pub(crate) use self::{cache::StudioCache, events::translate};
use super::{app::Kithara, message::Message};

const DOCS: &[(&str, &str)] = &[
    (
        "studio.klayout.ron",
        include_str!("../../../assets/ui/studio.klayout.ron"),
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
        "modules/studio-overview-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview-row.kmodule.ron"),
    ),
    (
        "modules/studio-mixer.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-mixer.kmodule.ron"),
    ),
    (
        "modules/studio-strip.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip.kmodule.ron"),
    ),
    (
        "studio-single.klayout.ron",
        include_str!("../../../assets/ui/studio-single.klayout.ron"),
    ),
    (
        "modules/studio-overview-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview-single.kmodule.ron"),
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

const COMPILES: &str = "embedded studio documents must compile";

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
        controls(ui).into_iter().map(|(path, _)| path).collect()
    }

    /// Number of segments the segmented control at `want` declares.
    fn segments(ui: &CompiledUi, want: &str) -> Option<usize> {
        let mut found = None;
        each_control(ui, &mut |node| {
            if let ExpandedNode::Control {
                path,
                spec: ControlSpec::Segmented { items },
                ..
            } = node
                && ui.resolve(*path) == want
            {
                found = Some(items.len());
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

    #[kithara::test]
    fn every_channel_strip_carries_the_full_control_set() {
        let ui = compile_studio(DeckLayout::Dual).unwrap();
        let paths = control_paths(&ui);
        for letter in ["a", "b"] {
            for name in ["trim", "low", "mid", "high", "tempo", "mute"] {
                let want = format!("mixer/{letter}/{name}");
                assert!(paths.contains(&want.as_str()), "missing control `{want}`");
            }
        }
    }

    /// The single-deck layout lays out deck A alone: nothing addresses deck B,
    /// in the deck body or in the overview.
    #[kithara::test]
    fn the_single_layout_lays_out_one_deck() {
        let ui = compile_studio(DeckLayout::Single).unwrap();
        let paths = control_paths(&ui);

        assert!(paths.contains(&"deck-a/wave"));
        assert!(paths.contains(&"overview/a/wave"));
        assert!(
            !paths
                .iter()
                .any(|path| path.starts_with("deck-b/") || path.starts_with("overview/b/")),
            "the single-deck layout still addresses deck B: {paths:?}",
        );
    }

    /// The top-bar switch offers exactly the layouts the host can select.
    #[kithara::test]
    fn the_deck_switch_offers_every_layout() {
        for layout in LAYOUTS {
            let ui = compile_studio(layout).unwrap();
            assert_eq!(segments(&ui, "bar/decks"), Some(LAYOUTS.len()));
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
