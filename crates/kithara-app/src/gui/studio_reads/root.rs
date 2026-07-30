use kithara_ui::render::{Node, Scope};

use super::{
    deck::{DeckNode, DecksNode, EngineNode},
    library::LibraryNode,
    mix::{MixNode, StripsNode},
    ui::{DragNode, UiNode},
};
use crate::gui::app::Kithara;

pub(in crate::gui) struct StudioRoot<'a> {
    library: LibraryNode<'a>,
    decks: Vec<DeckNode<'a>>,
    engine: EngineNode,
    mix: MixNode<'a>,
    mixer: StripsNode<'a>,
    ui: UiNode<'a>,
}

impl<'a> StudioRoot<'a> {
    pub(in crate::gui) fn new(state: &'a Kithara) -> Self {
        let cache = &state.studio.cache;
        let library = LibraryNode::new(&state.catalog, &cache.deck_marks, state.selected_track);
        let focus = cache.focus_deck();
        let decks: Vec<DeckNode<'a>> = state
            .decks
            .iter()
            .zip(cache.decks())
            .enumerate()
            .map(|(at, (deck, deck_cache))| {
                DeckNode::new(&deck.ui, deck.view, deck_cache, at == focus)
            })
            .collect();
        let engine = EngineNode::new(&decks);
        let drag = DragNode::new(
            cache.drag.and_then(|row| library.title(row)),
            cache.drag_target(),
            decks.len(),
        );

        Self {
            library,
            decks,
            engine,
            mix: MixNode::new(state.session.mix()),
            mixer: StripsNode::new(state.session.mix()),
            ui: UiNode::new(drag, cache.layout(), &cache.collapsed),
        }
    }
}

impl<'a, 'b: 'a> Node<'a> for &'a StudioRoot<'b> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let node: Box<dyn Node<'a> + 'a> = match segment {
            "library" => Box::new(&self.library),
            "deck" => Box::new(DecksNode::new(&self.decks)),
            "engine" => Box::new(self.engine),
            "mix" => Box::new(self.mix),
            "mixer" => Box::new(self.mixer),
            "ui" => Box::new(self.ui),
            _ => return None,
        };
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::render::{ReadValue, Reads, Walk};

    use super::*;
    use crate::{
        catalog::Catalog,
        gui::{
            deck::DeckView,
            studio_ui::{
                cache::{CatalogRowMarks, CollapsedModules, DeckCache, DeckLayout},
                endpoints::readable_endpoints,
            },
        },
        mix::MixState,
        state::UiState,
    };

    const DERIVED: [&str; 5] = [
        "deck.track.title",
        "deck.track.source_kind",
        "deck.playback.position_secs",
        "deck.playback.duration_secs",
        "deck.playback.position_normalized",
    ];

    struct Studio {
        catalog: Catalog,
        marks: CatalogRowMarks,
        collapsed: CollapsedModules,
        mix: MixState,
        decks: Vec<(UiState, DeckCache)>,
    }

    impl Studio {
        fn new(tempos: [&str; 2]) -> Self {
            Self {
                catalog: Catalog::new(vec!["dropped.mp3".to_string()]),
                marks: CatalogRowMarks::default(),
                collapsed: CollapsedModules::default(),
                mix: MixState::new(tempos.len()),
                decks: tempos.into_iter().map(deck).collect(),
            }
        }

        fn root(&self) -> StudioRoot<'_> {
            let library = LibraryNode::new(&self.catalog, &self.marks, Some(0));
            let decks: Vec<DeckNode<'_>> = self
                .decks
                .iter()
                .enumerate()
                .map(|(at, (ui, cache))| DeckNode::new(ui, DeckView::default(), cache, at == 0))
                .collect();
            let engine = EngineNode::new(&decks);
            let drag = DragNode::new(library.title(0), Some(1), decks.len());

            StudioRoot {
                library,
                decks,
                engine,
                mix: MixNode::new(&self.mix),
                mixer: StripsNode::new(&self.mix),
                ui: UiNode::new(drag, DeckLayout::Dual, &self.collapsed),
            }
        }
    }

    fn deck(tempo: &str) -> (UiState, DeckCache) {
        let mut ui = UiState::empty();
        ui.track_name = "Loaded".to_string();
        ui.eq_bands = vec![0.0; 3];
        ui.duration = 120.0;
        let mut cache = DeckCache::default();
        cache.tempo = tempo.to_string();
        cache.remain = "-02:00".to_string();
        cache.subtitle = "file".to_string();
        cache.zoom = Some(1.0);
        (ui, cache)
    }

    #[kithara::test]
    fn the_studio_tree_answers_every_key_the_renderer_asks_for() {
        let studio = Studio::new(["+2.0%", "-1.0%"]);
        let root = studio.root();
        let walk = Walk::new(&root);

        let documented = readable_endpoints().map(|(id, deck_scoped)| {
            if deck_scoped {
                format!("{id}@deck=a")
            } else {
                id.to_string()
            }
        });
        let synthesized = DERIVED.into_iter().map(|id| format!("{id}@deck=b"));
        for key in documented.chain(synthesized) {
            assert!(walk.get(&key).is_some(), "no owner answers `{key}`");
        }

        assert_eq!(
            walk.get("deck.playback.tempo@deck=a"),
            Some(ReadValue::Text("+2.0%")),
        );
        assert_eq!(
            walk.get("deck.playback.tempo@deck=b"),
            Some(ReadValue::Text("-1.0%")),
        );
        assert_eq!(
            walk.get("ui.drag.over@deck=c"),
            None,
            "the session has two decks",
        );
    }
}
