use kithara::ui::render::{Node, Scope};

use super::{
    broadcast::BroadcastNode,
    deck::{DeckNode, DecksNode, EngineNode},
    library::LibraryNode,
    mix::{MixNode, PlayerNode, StripsNode},
    stage::{DeckTempo, TempoNode, VisNode},
    ui::{DragNode, UiNode},
};
use crate::{broadcast::Broadcaster, gui::app::Kithara};

pub(in crate::gui) struct ReadRoot<'a> {
    broadcast: BroadcastNode<'a>,
    engine: EngineNode,
    library: LibraryNode<'a>,
    mix: MixNode<'a>,
    player: PlayerNode<'a>,
    mixer: StripsNode<'a>,
    tempo: TempoNode<'a>,
    ui: UiNode<'a>,
    decks: Vec<DeckNode<'a>>,
    vis: VisNode<'a>,
}

impl<'a> ReadRoot<'a> {
    pub(in crate::gui) fn new(state: &'a Kithara) -> Self {
        let cache = &state.ui.cache;
        let library = LibraryNode::new(
            &state.catalog,
            &cache.deck_marks,
            state.selected_track,
            &cache.library,
        );
        let focus = cache.focus_deck();
        let snapshot = &*state.snapshot;
        let decks: Vec<DeckNode<'a>> = snapshot
            .decks
            .iter()
            .zip(cache.decks())
            .enumerate()
            .map(|(at, (deck, deck_cache))| {
                DeckNode::new(deck, deck_cache, snapshot.eq_mode, at == focus)
            })
            .collect();
        let engine = EngineNode::new(&decks);
        let tempos: Vec<DeckTempo> = snapshot
            .decks
            .iter()
            .enumerate()
            .map(|(at, deck)| DeckTempo {
                bpm: deck.analysis.bpm,
                focused: at == focus,
                position: deck.position.max(0.0),
            })
            .collect();
        let drag = DragNode::new(
            cache.drag.and_then(|row| library.title(row)),
            cache.drag_target(),
            decks.len(),
        );

        Self {
            library,
            decks,
            engine,
            broadcast: BroadcastNode::new(
                snapshot.broadcast.is_on_air,
                &snapshot.broadcast.url,
                Broadcaster::is_available(),
            ),
            mix: MixNode::new(&snapshot.mix),
            mixer: StripsNode::new(&snapshot.mix),
            player: PlayerNode::new(&snapshot.mix),
            tempo: TempoNode::new(&cache.stage, &tempos),
            vis: VisNode::new(&cache.stage, &tempos),
            ui: UiNode::new(
                drag,
                cache.layout(),
                &cache.collapsed,
                &cache.menu,
                &cache.modules,
                &cache.window,
            ),
        }
    }
}

impl<'a, 'b: 'a> Node<'a> for &'a ReadRoot<'b> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let node: Box<dyn Node<'a> + 'a> = match segment {
            "broadcast" => Box::new(self.broadcast),
            "library" => Box::new(&self.library),
            "deck" => Box::new(DecksNode::new(&self.decks)),
            "engine" => Box::new(self.engine),
            "mix" => Box::new(self.mix),
            "mixer" => Box::new(self.mixer),
            "player" => Box::new(self.player),
            "tempo" => Box::new(&self.tempo),
            "vis" => Box::new(self.vis),
            "ui" => Box::new(self.ui),
            _ => return None,
        };
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use ::kithara::{
        abr::AbrMode,
        effects::GainDb,
        ui::render::{ReadValue, Reads, Walk},
    };
    use iced::Size;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        catalog::Catalog,
        deck::{DeckId, EqMode},
        engine::{DeckSettings, DeckSnapshot},
        gui::ui::{
            cache::{
                CatalogRowMarks, CollapsedModules, DeckCache, DeckLayout, LibraryView, StageView,
            },
            endpoints::readable_endpoints,
            menu::MenuState,
            modules::Modules,
            window::WindowState,
        },
        mix::MixState,
        state::{AbrVariant, UiState, covered},
    };

    struct Fixture {
        catalog: Catalog,
        marks: CatalogRowMarks,
        collapsed: CollapsedModules,
        eq_mode: EqMode,
        library: LibraryView,
        menu: MenuState,
        mix: MixState,
        modules: Modules,
        stage: StageView,
        decks: Vec<(UiState, DeckSettings, DeckCache)>,
        window: WindowState,
        broadcast_available: bool,
    }

    impl Fixture {
        fn new(tempos: [&str; 2]) -> Self {
            Self {
                catalog: Catalog::new(vec!["dropped.mp3".to_string()]),
                marks: CatalogRowMarks::default(),
                collapsed: CollapsedModules::default(),
                library: LibraryView::default(),
                menu: MenuState::default(),
                modules: Modules::default(),
                window: WindowState::default(),
                broadcast_available: false,
                mix: MixState::new(tempos.len()),
                stage: StageView::default(),
                eq_mode: EqMode::default(),
                decks: tempos.into_iter().map(deck).collect(),
            }
        }

        fn root<'a>(&'a self, shown: &'a [DeckSnapshot]) -> ReadRoot<'a> {
            let library = LibraryNode::new(&self.catalog, &self.marks, Some(0), &self.library);
            let decks: Vec<DeckNode<'_>> = shown
                .iter()
                .zip(&self.decks)
                .enumerate()
                .map(|(at, (deck, (_, _, cache)))| {
                    DeckNode::new(deck, cache, self.eq_mode, at == 0)
                })
                .collect();
            let engine = EngineNode::new(&decks);
            let tempos: Vec<DeckTempo> = shown
                .iter()
                .enumerate()
                .map(|(at, deck)| DeckTempo {
                    bpm: deck.analysis.bpm,
                    focused: at == 0,
                    position: deck.position.max(0.0),
                })
                .collect();
            let drag = DragNode::new(library.title(0), Some(1), decks.len());

            ReadRoot {
                library,
                decks,
                engine,
                broadcast: BroadcastNode::new(false, "", self.broadcast_available),
                mix: MixNode::new(&self.mix),
                mixer: StripsNode::new(&self.mix),
                player: PlayerNode::new(&self.mix),
                tempo: TempoNode::new(&self.stage, &tempos),
                vis: VisNode::new(&self.stage, &tempos),
                ui: UiNode::new(
                    drag,
                    DeckLayout::Dual,
                    &self.collapsed,
                    &self.menu,
                    &self.modules,
                    &self.window,
                ),
            }
        }

        fn shown(&self) -> Vec<DeckSnapshot> {
            self.decks
                .iter()
                .enumerate()
                .map(|(at, (ui, settings, _))| DeckSnapshot::new(DeckId(at), ui, settings))
                .collect()
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

    fn deck(tempo: &str) -> (UiState, DeckSettings, DeckCache) {
        let mut ui = UiState::empty();
        ui.track_name = "Loaded".to_string();
        ui.duration = 120.0;
        ui.abr_variants = hls_ladder();
        let mut cache = DeckCache::default();
        cache.tempo = tempo.to_string();
        cache.remain = "-02:00".to_string();
        cache.subtitle = "file".to_string();
        cache.view.zoom = Some(1.0);
        (ui, DeckSettings::new(3), cache)
    }

    fn fixture_in(mode: EqMode) -> Fixture {
        let mut fixture = Fixture::new(["+2.0%", "-1.0%"]);
        fixture.eq_mode = mode;
        for (_, settings, _) in &mut fixture.decks {
            settings.eq_bands = vec![GainDb::default(); mode.bands().len()];
        }
        fixture
    }

    /// The waveform read is where a renderer learns what the analysis has not
    /// covered; a snapshot that holds the whole track leaves it empty.
    #[kithara::test]
    fn the_waveform_read_carries_what_the_snapshot_has_not_covered() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        fixture.decks[0]
            .0
            .set_analysis(Some(covered(&[(0, 200), (400, 1_000)], Some(1_000)).into()));
        fixture.decks[1]
            .0
            .set_analysis(Some(covered(&[(0, 1_000)], Some(1_000)).into()));
        let shown = fixture.shown();
        let root = fixture.root(&shown);
        let walk = Walk::new(&root);

        let Some(ReadValue::Waveform(partial)) = walk.get("deck.playback.waveform@deck=a") else {
            panic!("the deck publishes a waveform");
        };
        assert_eq!(partial.unready, [[0.2, 0.4]]);

        let Some(ReadValue::Waveform(whole)) = walk.get("deck.playback.waveform@deck=b") else {
            panic!("the deck publishes a waveform");
        };
        assert!(whole.unready.is_empty(), "{:?}", whole.unready);
    }

    #[kithara::test]
    fn the_menu_marks_the_rung_in_force_and_hides_the_slots_the_ladder_lacks() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        fixture.decks[0].0.abr_mode = Some(AbrMode::manual(1));
        let shown = fixture.shown();
        let root = fixture.root(&shown);
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
    fn the_read_tree_answers_every_key_the_renderer_asks_for() {
        const DERIVED: [&str; 1] = ["deck.playback.position_normalized"];

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
            let fixture = fixture_in(mode);
            let shown = fixture.shown();
            let root = fixture.root(&shown);
            let walk = Walk::new(&root);
            unowned.retain(|key| walk.get(key).is_none());
        }
        assert!(unowned.is_empty(), "no owner answers {unowned:?}");

        let fixture = Fixture::new(["+2.0%", "-1.0%"]);
        let shown = fixture.shown();
        let root = fixture.root(&shown);
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
    fn the_air_controls_hide_when_the_build_carries_no_packager() {
        let fixture = Fixture::new(["+0.0%", "+0.0%"]);
        let shown = fixture.shown();
        let root = fixture.root(&shown);

        assert_eq!(
            Walk::new(&root).get("broadcast.hidden"),
            Some(ReadValue::Bool(true)),
            "the stand-in packager is the one a build without the feature has",
        );

        let mut fixture = fixture;
        fixture.broadcast_available = true;
        let shown = fixture.shown();
        let root = fixture.root(&shown);

        assert_eq!(
            Walk::new(&root).get("broadcast.hidden"),
            Some(ReadValue::Bool(false))
        );
    }

    #[kithara::test]
    fn the_menu_states_what_the_only_window_draws() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        {
            let shown = fixture.shown();
            let root = fixture.root(&shown);
            let walk = Walk::new(&root);

            assert_eq!(
                walk.get("ui.window.title@window=1"),
                Some(ReadValue::Text("WINDOW 1 \u{b7} 2 DECKS"))
            );
            assert_eq!(
                walk.get("ui.window.caption@window=1"),
                Some(ReadValue::Text("1280 \u{d7} 760 \u{b7} 4 MOD."))
            );
            assert_eq!(
                walk.get("ui.window.active@window=1"),
                Some(ReadValue::Bool(true))
            );
            assert_eq!(
                walk.get("ui.window.close_hidden@window=1"),
                Some(ReadValue::Bool(true)),
                "the only window offers no way to close itself from the list",
            );
        }

        fixture.modules.toggle("ov");
        fixture.window.set_size(Size::new(1600.0, 900.0));
        fixture.window.refresh(DeckLayout::Single, &fixture.modules);
        let shown = fixture.shown();
        let root = fixture.root(&shown);

        assert_eq!(
            Walk::new(&root).get("ui.window.caption@window=1"),
            Some(ReadValue::Text("1600 \u{d7} 900 \u{b7} 3 MOD.")),
        );
    }

    #[kithara::test]
    fn a_module_the_menu_switches_off_leaves_the_layout() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        {
            let shown = fixture.shown();
            let root = fixture.root(&shown);
            let walk = Walk::new(&root);

            assert_eq!(
                walk.get("ui.module.on@module=ov"),
                Some(ReadValue::Bool(true)),
                "every module the app lays out starts on",
            );
            assert_eq!(
                walk.get("ui.module.hidden@module=ov"),
                Some(ReadValue::Bool(false))
            );
            assert_eq!(
                walk.get("ui.modules.count"),
                Some(ReadValue::Text("4 OF 4"))
            );
        }

        fixture.modules.toggle("ov");
        let shown = fixture.shown();
        let root = fixture.root(&shown);
        let walk = Walk::new(&root);

        assert_eq!(
            walk.get("ui.module.on@module=ov"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            walk.get("ui.module.hidden@module=ov"),
            Some(ReadValue::Bool(true))
        );
        assert_eq!(
            walk.get("ui.modules.count"),
            Some(ReadValue::Text("3 OF 4"))
        );
        assert_eq!(
            walk.get("ui.module.on@module=mix"),
            Some(ReadValue::Bool(true)),
            "one cell switches one pane",
        );
    }

    #[kithara::test]
    fn the_menu_reads_its_own_state_and_the_layout_in_force() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        fixture.menu.toggle_layouts();
        let shown = fixture.shown();
        let root = fixture.root(&shown);
        let walk = Walk::new(&root);

        assert_eq!(
            walk.get("ui.menu.group_open@group=lay"),
            Some(ReadValue::Bool(true))
        );
        assert_eq!(
            walk.get("ui.menu.group_hidden@group=lay"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            walk.get("ui.menu.group_open@group=mod"),
            Some(ReadValue::Bool(false)),
            "the menu offers no module group yet",
        );
        assert_eq!(
            walk.get("ui.layout.selected@layout=2"),
            Some(ReadValue::Bool(true)),
            "the app lays out two decks",
        );
        assert_eq!(
            walk.get("ui.layout.selected@layout=1"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            walk.get("ui.layouts.active"),
            Some(ReadValue::Text("2 DECKS"))
        );
        assert_eq!(
            walk.get("ui.app.version"),
            Some(ReadValue::Text(env!("CARGO_PKG_VERSION")))
        );
    }

    #[kithara::test]
    fn every_deck_reads_the_shared_eq_mode() {
        for (mode, bands) in [(EqMode::ThreeBand, 3.0), (EqMode::FourBand, 4.0)] {
            let fixture = fixture_in(mode);
            let shown = fixture.shown();
            let root = fixture.root(&shown);
            let walk = Walk::new(&root);

            for deck in ["a", "b"] {
                assert_eq!(
                    walk.get(&format!("deck.eq.bands@deck={deck}")),
                    Some(ReadValue::Scalar(bands)),
                    "the strip draws as many bands as the mode has",
                );
                for rung in [3.0, 4.0] {
                    assert_eq!(
                        walk.get(&format!("deck.eq.selected@bands={rung},deck={deck}")),
                        Some(ReadValue::Bool(rung == bands)),
                        "the menu marks the mode in force",
                    );
                }
            }
        }
    }

    #[kithara::test]
    fn a_three_band_app_has_no_mid_band_gains() {
        let fixture = fixture_in(EqMode::ThreeBand);
        let shown = fixture.shown();
        let root = fixture.root(&shown);
        let walk = Walk::new(&root);

        for deck in ["a", "b"] {
            for band in ["low_mid", "high_mid"] {
                let key = format!("deck.eq.{band}@deck={deck}");
                assert_eq!(walk.get(&key), None, "three-band decks have no `{key}`");
            }
        }
    }
}
