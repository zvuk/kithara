use kithara_ui::render::{Node, Scope};

use super::{
    broadcast::BroadcastNode,
    deck::{DeckNode, DecksNode, EngineNode},
    library::LibraryNode,
    mix::{MixNode, StripsNode},
    ui::{DragNode, UiNode},
};
use crate::{broadcast::Broadcaster, gui::app::Kithara};

pub(in crate::gui) struct ReadRoot<'a> {
    broadcast: BroadcastNode<'a>,
    engine: EngineNode,
    library: LibraryNode<'a>,
    mix: MixNode<'a>,
    mixer: StripsNode<'a>,
    ui: UiNode<'a>,
    decks: Vec<DeckNode<'a>>,
}

impl<'a> ReadRoot<'a> {
    pub(in crate::gui) fn new(state: &'a Kithara) -> Self {
        let cache = &state.ui.cache;
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
                Broadcaster::is_available(),
            ),
            library,
            decks,
            engine,
            mix: MixNode::new(state.session.mix()),
            mixer: StripsNode::new(state.session.mix()),
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
            "ui" => Box::new(self.ui),
            _ => return None,
        };
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use iced::Size;
    use kithara_test_utils::kithara;
    use kithara_ui::render::{ReadValue, Reads, Walk};

    use super::*;
    use crate::{
        catalog::Catalog,
        deck::EqMode,
        gui::{
            deck::DeckView,
            ui::{
                cache::{CatalogRowMarks, CollapsedModules, DeckCache, DeckLayout},
                endpoints::readable_endpoints,
                menu::MenuState,
                modules::Modules,
                window::WindowState,
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

    struct Fixture {
        catalog: Catalog,
        marks: CatalogRowMarks,
        collapsed: CollapsedModules,
        menu: MenuState,
        modules: Modules,
        window: WindowState,
        mix: MixState,
        eq_mode: EqMode,
        broadcast_available: bool,
        decks: Vec<(UiState, DeckCache)>,
    }

    impl Fixture {
        fn new(tempos: [&str; 2]) -> Self {
            Self {
                catalog: Catalog::new(vec!["dropped.mp3".to_string()]),
                marks: CatalogRowMarks::default(),
                collapsed: CollapsedModules::default(),
                menu: MenuState::default(),
                modules: Modules::default(),
                window: WindowState::default(),
                broadcast_available: false,
                mix: MixState::new(tempos.len()),
                eq_mode: EqMode::default(),
                decks: tempos.into_iter().map(deck).collect(),
            }
        }

        fn root(&self) -> ReadRoot<'_> {
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

            ReadRoot {
                broadcast: BroadcastNode::new(false, "", self.broadcast_available),
                library,
                decks,
                engine,
                mix: MixNode::new(&self.mix),
                mixer: StripsNode::new(&self.mix),
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

    fn fixture_in(mode: EqMode) -> Fixture {
        let mut fixture = Fixture::new(["+2.0%", "-1.0%"]);
        fixture.eq_mode = mode;
        for (ui, _) in &mut fixture.decks {
            ui.eq_bands = vec![0.0; mode.bands().len()];
        }
        fixture
    }

    #[kithara::test]
    fn the_menu_marks_the_rung_in_force_and_hides_the_slots_the_ladder_lacks() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        fixture.decks[0].0.abr_mode_is_auto = false;
        fixture.decks[0].0.selected_variant = Some(1);
        let root = fixture.root();
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
            let root = fixture.root();
            let walk = Walk::new(&root);
            unowned.retain(|key| walk.get(key).is_none());
        }
        assert!(unowned.is_empty(), "no owner answers {unowned:?}");

        let fixture = Fixture::new(["+2.0%", "-1.0%"]);
        let root = fixture.root();
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
        let root = fixture.root();

        assert_eq!(
            Walk::new(&root).get("broadcast.hidden"),
            Some(ReadValue::Bool(true)),
            "the stand-in packager is the one a build without the feature has",
        );

        let mut fixture = fixture;
        fixture.broadcast_available = true;
        let root = fixture.root();

        assert_eq!(
            Walk::new(&root).get("broadcast.hidden"),
            Some(ReadValue::Bool(false))
        );
    }

    #[kithara::test]
    fn the_menu_states_what_the_only_window_draws() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        {
            let root = fixture.root();
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
        let root = fixture.root();

        assert_eq!(
            Walk::new(&root).get("ui.window.caption@window=1"),
            Some(ReadValue::Text("1600 \u{d7} 900 \u{b7} 3 MOD.")),
        );
    }

    #[kithara::test]
    fn a_module_the_menu_switches_off_leaves_the_layout() {
        let mut fixture = Fixture::new(["+0.0%", "+0.0%"]);
        {
            let root = fixture.root();
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
        let root = fixture.root();
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
        fixture.menu.toggle();
        fixture.menu.toggle_layouts();
        let root = fixture.root();
        let walk = Walk::new(&root);

        assert_eq!(walk.get("ui.menu.open"), Some(ReadValue::Bool(true)));
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
            let root = fixture.root();
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
        let root = fixture.root();
        let walk = Walk::new(&root);

        for deck in ["a", "b"] {
            for band in ["low_mid", "high_mid"] {
                let key = format!("deck.eq.{band}@deck={deck}");
                assert_eq!(walk.get(&key), None, "three-band decks have no `{key}`");
            }
        }
    }
}
