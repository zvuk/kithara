use kithara_ui::render::{Node, Scope};

use super::{
    broadcast::BroadcastNode,
    deck::{DeckNode, DecksNode, EngineNode},
    library::LibraryNode,
    mix::{MixNode, StripsNode},
    ui::{DragNode, UiNode},
};
use crate::gui::app::Kithara;

pub(in crate::gui) struct StudioRoot<'a> {
    broadcast: BroadcastNode<'a>,
    engine: EngineNode,
    library: LibraryNode<'a>,
    mix: MixNode<'a>,
    mixer: StripsNode<'a>,
    ui: UiNode<'a>,
    decks: Vec<DeckNode<'a>>,
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
                DeckNode::new(&deck.ui, deck.view, deck_cache, state.eq_mode, at == focus)
            })
            .collect();
        let engine = EngineNode::new(&decks);
        let drag = DragNode::new(
            cache.drag.and_then(|row| library.title(row)),
            cache.drag_target(),
            decks.len(),
        );

        Self {
            broadcast: BroadcastNode::new(
                state.broadcast.is_on_air(),
                state.broadcast.url().unwrap_or_default(),
            ),
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
            "broadcast" => Box::new(self.broadcast),
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
        deck::EqMode,
        gui::{
            deck::DeckView,
            studio_ui::{
                cache::{CatalogRowMarks, CollapsedModules, DeckCache, DeckLayout},
                endpoints::readable_endpoints,
            },
        },
        mix::MixState,
        state::{AbrVariant, UiState},
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
        eq_mode: EqMode,
        decks: Vec<(UiState, DeckCache)>,
    }

    impl Studio {
        fn new(tempos: [&str; 2]) -> Self {
            Self {
                catalog: Catalog::new(vec!["dropped.mp3".to_string()]),
                marks: CatalogRowMarks::default(),
                collapsed: CollapsedModules::default(),
                mix: MixState::new(tempos.len()),
                eq_mode: EqMode::default(),
                decks: tempos.into_iter().map(deck).collect(),
            }
        }

        fn root(&self) -> StudioRoot<'_> {
            let library = LibraryNode::new(&self.catalog, &self.marks, Some(0));
            let decks: Vec<DeckNode<'_>> = self
                .decks
                .iter()
                .enumerate()
                .map(|(at, (ui, cache))| {
                    DeckNode::new(ui, DeckView::default(), cache, self.eq_mode, at == 0)
                })
                .collect();
            let engine = EngineNode::new(&decks);
            let drag = DragNode::new(library.title(0), Some(1), decks.len());

            StudioRoot {
                broadcast: BroadcastNode::new(false, ""),
                library,
                decks,
                engine,
                mix: MixNode::new(&self.mix),
                mixer: StripsNode::new(&self.mix),
                ui: UiNode::new(drag, DeckLayout::Dual, &self.collapsed),
            }
        }
    }

    fn hls_ladder() -> Vec<AbrVariant> {
        vec![
            AbrVariant {
                index: 0,
                label: "128k".to_string(),
                detail: "128 kbps \u{b7} AAC".to_string(),
            },
            AbrVariant {
                index: 1,
                label: "320k".to_string(),
                detail: "320 kbps \u{b7} AAC".to_string(),
            },
        ]
    }

    fn deck(tempo: &str) -> (UiState, DeckCache) {
        let mut ui = UiState::empty();
        ui.track_name = "Loaded".to_string();
        ui.eq_bands = vec![0.0; 3];
        ui.duration = 120.0;
        ui.abr_variants = hls_ladder();
        let mut cache = DeckCache::default();
        cache.tempo = tempo.to_string();
        cache.remain = "-02:00".to_string();
        cache.subtitle = "file".to_string();
        cache.view.zoom = Some(1.0);
        (ui, cache)
    }

    fn studio_in(mode: EqMode) -> Studio {
        let mut studio = Studio::new(["+2.0%", "-1.0%"]);
        studio.eq_mode = mode;
        for (ui, _) in &mut studio.decks {
            ui.eq_bands = vec![0.0; mode.bands().len()];
        }
        studio
    }

    #[kithara::test]
    fn the_menu_marks_the_rung_in_force_and_hides_the_slots_the_ladder_lacks() {
        let mut studio = Studio::new(["+0.0%", "+0.0%"]);
        studio.decks[0].0.abr_mode_is_auto = false;
        studio.decks[0].0.selected_variant = Some(1);
        let root = studio.root();
        let walk = Walk::new(&root);

        assert_eq!(
            walk.get("deck.stream.quality_hidden@deck=a"),
            Some(ReadValue::Bool(false)),
        );
        assert_eq!(
            walk.get("deck.stream.variant_active@deck=a,variant=1"),
            Some(ReadValue::Bool(true)),
        );
        for absent in [
            "deck.stream.variant_active@deck=a,variant=auto",
            "deck.stream.variant_active@deck=a,variant=0",
        ] {
            assert_eq!(walk.get(absent), Some(ReadValue::Bool(false)), "{absent}");
        }
        assert_eq!(
            walk.get("deck.stream.variant_sub@deck=a,variant=1"),
            Some(ReadValue::Text("320 kbps \u{b7} AAC")),
        );
        assert_eq!(
            walk.get("deck.stream.variant_hidden@deck=a,variant=2"),
            Some(ReadValue::Bool(true)),
            "the ladder has two rungs, so the third slot stays hidden",
        );
    }

    #[kithara::test]
    fn the_studio_tree_answers_every_key_the_renderer_asks_for() {
        let documented = readable_endpoints().map(|(id, scopes)| {
            let scope: Vec<String> = scopes
                .iter()
                .map(|scope| match *scope {
                    "deck" => "deck=a".to_owned(),
                    other => format!("{other}=0"),
                })
                .collect();
            if scope.is_empty() {
                id.to_string()
            } else {
                format!("{id}@{}", scope.join(","))
            }
        });
        let synthesized = DERIVED.into_iter().map(|id| format!("{id}@deck=b"));
        // A mode-scoped endpoint is answered only by the mode that draws it,
        // so ownership is a claim about the modes together.
        let mut unowned: Vec<String> = documented.chain(synthesized).collect();
        for mode in [EqMode::ThreeBand, EqMode::FourBand] {
            let studio = studio_in(mode);
            let root = studio.root();
            let walk = Walk::new(&root);
            unowned.retain(|key| walk.get(key).is_none());
        }
        assert!(unowned.is_empty(), "no owner answers {unowned:?}");

        let studio = Studio::new(["+2.0%", "-1.0%"]);
        let root = studio.root();
        let walk = Walk::new(&root);
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

    #[kithara::test]
    fn every_deck_reads_the_shared_eq_mode() {
        let studio = studio_in(EqMode::FourBand);
        let root = studio.root();
        let walk = Walk::new(&root);

        for deck in ["a", "b"] {
            assert_eq!(
                walk.get(&format!("deck.eq.three_band@deck={deck}")),
                Some(ReadValue::Bool(false))
            );
            assert_eq!(
                walk.get(&format!("deck.eq.four_band@deck={deck}")),
                Some(ReadValue::Bool(true))
            );
            assert!(walk.get(&format!("deck.eq.low_mid@deck={deck}")).is_some());
        }
    }

    #[kithara::test]
    fn a_three_band_studio_answers_no_four_band_key() {
        let studio = studio_in(EqMode::ThreeBand);
        let root = studio.root();
        let walk = Walk::new(&root);

        for deck in ["a", "b"] {
            for band in ["low_mid", "high_mid"] {
                let key = format!("deck.eq.{band}@deck={deck}");
                assert_eq!(walk.get(&key), None, "three-band decks have no `{key}`");
            }
        }
    }
}
