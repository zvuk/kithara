use kithara::{
    effects::GainDb,
    ui::render::{
        ControlAction, DEFAULT_ZOOM, DragPhase, UiEvent, WindowCommand, Zoom, zoom_in, zoom_out,
    },
};
use num_traits::cast::AsPrimitive;

use super::{
    cache::{DeckLayout, ViewCache, WindowEdge},
    scope::{consts::MICRO_DECK, deck_index, eq_band},
};
use crate::{
    deck::{DeckId, EqMode, TempoPercent},
    engine::MixCmd,
    gui::{
        app::Kithara,
        deck::{DeckMsg, consts::TEMPO_STEP},
        message::Message,
    },
};

/// Translate a compiled-UI event into an app message, applying host-owned
/// view state (zoom, module collapse, deck layout) in place. Control paths
/// come from the app documents and may contain nested include segments.
pub(crate) fn translate(state: &mut Kithara, event: UiEvent) -> Option<Message> {
    match event {
        UiEvent::Control { path, action } => {
            if matches!(action, ControlAction::Activate) {
                state.ui.press(&path);
            }
            control(state, &path, &action)
        }
        UiEvent::ToggleModule(module) => {
            state.ui.cache.toggle_module(module);
            None
        }
        UiEvent::LibraryQuery(query) => {
            state.ui.cache.library.query = query;
            None
        }
        UiEvent::Window(command) => Some(Message::Window(command)),
        _ => None,
    }
}

pub(super) enum Route {
    Bar,
    Deck(usize),
    Library,
    MicroBar,
    Mixer,
    Overview,
}

pub(super) fn route(instance: &str) -> Option<Route> {
    match instance {
        "micro-bar" => Some(Route::MicroBar),
        "bar" => Some(Route::Bar),
        "mixer" => Some(Route::Mixer),
        "library" => Some(Route::Library),
        "overview" => Some(Route::Overview),
        deck => deck_index(deck.strip_prefix("deck-")?).map(Route::Deck),
    }
}

/// A match guard would read more clearly here, but `if let` guards are not available at the crate's
/// MSRV.
fn control(state: &mut Kithara, path: &str, action: &ControlAction) -> Option<Message> {
    let (instance, rest) = path.split_once('/')?;
    let target = route(instance)?;
    if let Some(row) = rest.strip_prefix("menu/")
        && matches!(target, Route::MicroBar | Route::Bar)
    {
        return menu_control(&mut state.ui.cache, row, action);
    }
    match target {
        Route::MicroBar => micro_control(state, rest, action),
        Route::Bar => bar_control(rest, action),
        Route::Mixer => mixer_control(state, rest, action),
        Route::Library => library_control(state, rest, action),
        Route::Overview => {
            let (letter, control) = rest.split_once('/')?;
            deck_control(state, deck_index(letter)?, control, action)
        }
        Route::Deck(index) => deck_control(state, index, rest, action),
    }
}

fn micro_control(state: &mut Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    let index = deck_index(MICRO_DECK)?;
    match control {
        "volume" => volume_control(state, index, action),
        control => deck_control(state, index, control, action),
    }
}

fn volume_control(state: &Kithara, index: usize, action: &ControlAction) -> Option<Message> {
    let ControlAction::SetScalar(trim) = action else {
        return None;
    };
    Some(Message::Mix(MixCmd::Trim(
        deck_id(state, index)?,
        trim.clamp(0.0, 1.0).as_(),
    )))
}

fn deck_control(
    state: &mut Kithara,
    index: usize,
    control: &str,
    action: &ControlAction,
) -> Option<Message> {
    if zoom_control(&mut state.ui.cache, index, control, action).is_some() {
        return None;
    }
    if let Some(rest) = control.strip_prefix("stream/") {
        return stream_control(state, index, rest, action);
    }
    let id = deck_id(state, index)?;
    let msg = match (control, action) {
        ("drop", ControlAction::Drag(DragPhase::Over(over))) => {
            state.ui.cache.set_hover_deck(index, *over);
            return None;
        }
        ("wave", ControlAction::SetScalar(position)) => DeckMsg::SeekTo(position.clamp(0.0, 1.0)),
        ("wave/zoom", ControlAction::SetScalar(zoom)) => {
            let zoom: f32 = zoom.as_();
            state.ui.cache.deck_mut(index)?.view.zoom =
                Some(f64::from(f32::from(Zoom::from(zoom))));
            return None;
        }
        ("tempo", ControlAction::StepScalar(steps)) => DeckMsg::SetTempo(TempoPercent::from(
            steps.mul_add(TEMPO_STEP, f32::from(state.snapshot.deck(id)?.tempo)),
        )),
        ("tempo", ControlAction::Activate) => DeckMsg::SetTempo(TempoPercent::DEFAULT),
        ("play", ControlAction::Activate) => DeckMsg::TogglePlayPause,
        ("prev", ControlAction::Activate) => DeckMsg::Prev,
        ("next", ControlAction::Activate) => DeckMsg::Next,
        _ => return None,
    };
    Some(Message::Deck(id, msg))
}

fn stream_control(
    state: &mut Kithara,
    index: usize,
    control: &str,
    action: &ControlAction,
) -> Option<Message> {
    if !matches!(action, ControlAction::Activate) {
        return None;
    }
    let open = match control {
        "cell" => !state.ui.cache.deck_mut(index)?.view.quality_menu,
        "pop" => false,
        row => {
            let msg = quality_msg(state, index, row)?;
            state.ui.cache.deck_mut(index)?.view.quality_menu = false;
            return Some(Message::Deck(deck_id(state, index)?, msg));
        }
    };
    state.ui.cache.deck_mut(index)?.view.quality_menu = open;
    None
}

fn quality_msg(state: &Kithara, index: usize, path: &str) -> Option<DeckMsg> {
    let (row, _) = path.split_once('/')?;
    if row == "auto" {
        return Some(DeckMsg::SetQuality(None));
    }
    let slot: usize = row.strip_prefix("variant-")?.parse().ok()?;
    let id = deck_id(state, index)?;
    let rung = state.snapshot.deck(id)?.stream.variants.get(slot)?.index;
    Some(DeckMsg::SetQuality(Some(rung)))
}

fn zoom_control(
    cache: &mut ViewCache,
    index: usize,
    control: &str,
    action: &ControlAction,
) -> Option<()> {
    let step: fn(Zoom) -> Zoom = match (control, action) {
        ("zoom-in", ControlAction::Activate) => zoom_in,
        ("zoom-out", ControlAction::Activate) => zoom_out,
        _ => return None,
    };
    let deck = cache.deck_mut(index)?;
    let current: f32 = deck.view.zoom.map_or(DEFAULT_ZOOM, AsPrimitive::as_);
    deck.view.zoom = Some(f64::from(f32::from(step(current.into()))));
    Some(())
}

fn bar_control(control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("broadcast", ControlAction::Activate) => Some(Message::BroadcastToggle),
        _ => None,
    }
}

/// The app menu owns its own surface and hands everything else to the host:
/// window mode, the air, and the layout its rows name by deck count.
///
/// A grid cell reaches the host through its own include, so the module name is always the path's
/// first segment.
fn menu_control(cache: &mut ViewCache, control: &str, action: &ControlAction) -> Option<Message> {
    if !matches!(action, ControlAction::Activate) {
        return None;
    }
    match control {
        "layouts-head" => cache.menu.toggle_layouts(),
        "modules-head" => cache.menu.toggle_modules(),
        "full-screen" => return Some(Message::Window(WindowCommand::ToggleFullScreen)),
        "cast" => return Some(Message::BroadcastToggle),
        row => {
            let (row, _) = row.split_once('/').unwrap_or((row, ""));
            if let Some(module) = row.strip_prefix("module-") {
                cache.modules.toggle(module);
                return None;
            }
            let decks = row.strip_prefix("layout-")?.parse().ok()?;
            cache.set_layout(DeckLayout::from_decks(decks)?);
            return Some(Message::PauseHiddenDecks);
        }
    }
    None
}

fn mixer_control(state: &mut Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("xfade", ControlAction::SetScalar(position)) => Some(Message::Mix(MixCmd::Crossfader(
            position.clamp(0.0, 1.0).as_(),
        ))),
        ("master", ControlAction::SetScalar(gain)) => {
            Some(Message::Mix(MixCmd::Master(gain.clamp(0.0, 1.0).as_())))
        }
        ("window/min" | "window/max", ControlAction::SetScalar(at)) => {
            let edge = if control.ends_with("min") {
                WindowEdge::Min
            } else {
                WindowEdge::Max
            };
            state.ui.cache.stage.set_edge(edge, at.as_());
            None
        }
        _ => strip_control(state, control, action),
    }
}

/// The channel strip owns both mix-side controls and the deck's tone, so its
/// instance letter addresses the deck.
fn strip_control(state: &mut Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    let (letter, control) = control.split_once('/')?;
    let name = control.rsplit('/').next()?;
    let index = deck_index(letter)?;
    match (name, action) {
        ("eq-menu-anchor", ControlAction::SecondaryActivate) => {
            state.ui.cache.set_eq_menu_open(index, true)?;
            None
        }
        ("eq-menu", ControlAction::Activate) => {
            state.ui.cache.set_eq_menu_open(index, false)?;
            None
        }
        ("eq-3", ControlAction::Activate) => {
            state.ui.cache.close_eq_menus();
            Some(Message::SetEqMode(EqMode::ThreeBand))
        }
        ("eq-4", ControlAction::Activate) => {
            state.ui.cache.close_eq_menus();
            Some(Message::SetEqMode(EqMode::FourBand))
        }
        ("mute", ControlAction::Activate) => {
            let muted = state.snapshot.mix.strips.get(index)?.muted;
            Some(Message::Mix(MixCmd::Muted(deck_id(state, index)?, !muted)))
        }
        ("volume", _) => volume_control(state, index, action),
        (_, ControlAction::SetScalar(value)) => Some(Message::Deck(
            deck_id(state, index)?,
            eq_msg(eq_band(state.snapshot.eq_mode, name)?, *value),
        )),
        _ => None,
    }
}

/// The list reports the drag, the deck reports the pointer crossing it, and the
/// host joins them here. A row is a position in its group, resolved back to a
/// catalog entry through the scope the rows were drawn from.
fn library_control(state: &mut Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("browser" | "context", ControlAction::SelectIndex(row)) => {
            let picked = state.ui.cache.library.groups().nth(*row)?;
            state.ui.cache.library.scope = picked;
            None
        }
        ("tracks", ControlAction::SelectIndex(row)) => {
            let index = state.ui.cache.library.catalog_index(&state.catalog, *row)?;
            Some(Message::SelectCatalogTrack(index))
        }
        ("tracks", ControlAction::Drag(DragPhase::Start(row))) => {
            state.ui.cache.drag = Some(*row);
            None
        }
        ("tracks", ControlAction::Drag(DragPhase::Drop)) => {
            let (row, deck) = state.ui.cache.take_drop()?;
            let index = state.ui.cache.library.catalog_index(&state.catalog, row)?;
            Some(Message::LoadOntoDeck(index, deck_id(state, deck)?))
        }
        _ => None,
    }
}

fn deck_id(state: &Kithara, index: usize) -> Option<DeckId> {
    state.snapshot.decks.get(index).map(|deck| deck.id)
}

fn eq_msg(band: usize, knob: f64) -> DeckMsg {
    DeckMsg::EqBandChanged(band, GainDb::at_knob(knob.as_()))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn select_layout(cache: &mut ViewCache, layout: DeckLayout) -> Option<Message> {
        press_menu(cache, &format!("layout-{}", layout.decks()))
    }

    fn press_menu(cache: &mut ViewCache, row: &str) -> Option<Message> {
        menu_control(cache, row, &ControlAction::Activate)
    }

    fn press_zoom(cache: &mut ViewCache, control: &str) -> f64 {
        zoom_control(cache, 0, control, &ControlAction::Activate);
        cache.deck_mut(0).and_then(|deck| deck.view.zoom).unwrap()
    }

    #[kithara::test]
    fn the_zoom_buttons_step_the_wave_window_and_stop_at_its_bounds() {
        const PRESSES: usize = 40;

        let mut cache = ViewCache::with_decks(1);

        let narrowed = press_zoom(&mut cache, "zoom-in");
        let widened = press_zoom(&mut cache, "zoom-out");
        assert!(narrowed < f64::from(DEFAULT_ZOOM), "zoom in must narrow");
        assert!(widened > narrowed, "zoom out must widen");

        for _ in 0..PRESSES {
            press_zoom(&mut cache, "zoom-in");
        }
        let floor = press_zoom(&mut cache, "zoom-in");
        assert!(floor > 0.0, "the window never closes");
        assert_eq!(press_zoom(&mut cache, "zoom-in"), floor);

        for _ in 0..PRESSES {
            press_zoom(&mut cache, "zoom-out");
        }
        let ceiling = press_zoom(&mut cache, "zoom-out");
        assert!(ceiling < 1.0, "the window never spans the whole track");
        assert_eq!(press_zoom(&mut cache, "zoom-out"), ceiling);
    }

    #[kithara::test]
    fn the_layouts_group_applies_the_deck_layout_its_row_names() {
        let mut cache = ViewCache::default();
        assert!(!cache.menu.are_layouts_open());

        press_menu(&mut cache, "layouts-head");
        assert!(cache.menu.are_layouts_open());

        assert!(matches!(
            press_menu(&mut cache, "layout-1/apply"),
            Some(Message::PauseHiddenDecks)
        ));
        assert_eq!(cache.layout(), DeckLayout::Single);
        assert!(
            cache.menu.are_layouts_open(),
            "applying a layout leaves the menu where it was"
        );

        press_menu(&mut cache, "layout-2/apply");
        assert_eq!(cache.layout(), DeckLayout::Dual);

        press_menu(&mut cache, "layouts-head");
        assert!(!cache.menu.are_layouts_open());
    }

    #[kithara::test]
    fn a_module_cell_switches_its_own_pane_and_the_group_opens_on_its_head() {
        let mut cache = ViewCache::default();
        assert!(!cache.menu.are_modules_open());

        press_menu(&mut cache, "modules-head");
        assert!(cache.menu.are_modules_open());

        assert!(cache.modules.is_on("ov"));
        press_menu(&mut cache, "module-ov/cell");
        assert!(!cache.modules.is_on("ov"));
        assert!(cache.modules.is_on("mix"), "one cell switches one pane");

        press_menu(&mut cache, "module-ov/cell");
        assert!(cache.modules.is_on("ov"));
    }

    #[kithara::test]
    fn the_menu_asks_the_host_for_full_screen_and_for_the_air() {
        let mut cache = ViewCache::default();

        assert!(matches!(
            press_menu(&mut cache, "full-screen"),
            Some(Message::Window(WindowCommand::ToggleFullScreen))
        ));
        assert!(matches!(
            press_menu(&mut cache, "cast"),
            Some(Message::BroadcastToggle)
        ));
    }

    #[kithara::test]
    fn narrowing_the_layout_drops_a_hover_it_stops_laying_out() {
        let mut cache = ViewCache::default();
        cache.set_hover_deck(1, true);

        assert!(matches!(
            select_layout(&mut cache, DeckLayout::Single),
            Some(Message::PauseHiddenDecks)
        ));

        cache.drag = Some(3);
        assert_eq!(
            cache.take_drop(),
            None,
            "deck B no longer renders, so it can never report the pointer leaving"
        );
    }

    #[kithara::test]
    fn widening_the_layout_keeps_a_hover_it_lays_out() {
        let mut cache = ViewCache::default();
        cache.set_hover_deck(0, true);

        select_layout(&mut cache, DeckLayout::Single);
        select_layout(&mut cache, DeckLayout::Dual);

        cache.drag = Some(3);
        assert_eq!(cache.take_drop(), Some((3, 0)));
    }

    #[kithara::test]
    fn narrowing_the_layout_moves_a_focus_it_stops_laying_out() {
        let mut cache = ViewCache::default();
        cache.set_hover_deck(1, true);
        cache.drag = Some(3);
        cache.take_drop();
        assert_eq!(cache.focus_deck(), 1);

        select_layout(&mut cache, DeckLayout::Single);

        assert_eq!(cache.focus_deck(), 0, "the key must reach a deck on screen");
    }

    #[cfg(not(feature = "broadcast"))]
    mod routing {
        use ::kithara::ui::render::{ControlAction, DragPhase, UiEvent, WindowCommand};
        use kithara_test_utils::kithara;

        use super::super::translate;
        use crate::{
            deck::{DeckId, EqMode, TempoPercent},
            engine::MixCmd,
            gui::{app::Kithara, deck::DeckMsg, message::Message, rig::Rig},
            state::AbrVariant,
        };

        fn send(state: &mut Kithara, path: &str, action: ControlAction) -> Option<Message> {
            translate(
                state,
                UiEvent::Control {
                    action,
                    path: path.to_string(),
                },
            )
        }

        #[kithara::test(native, flash(false))]
        fn deck_and_bar_controls_translate_to_their_owned_messages() {
            let mut rig = Rig::offline();
            rig.message(Message::Deck(
                DeckId(0),
                DeckMsg::SetTempo(TempoPercent::from(3.0)),
            ));
            let state = &mut rig.ui;

            assert!(matches!(
                send(state, "deck-a/play", ControlAction::Activate),
                Some(Message::Deck(DeckId(0), DeckMsg::TogglePlayPause))
            ));
            assert!(matches!(
                send(state, "overview/b/next", ControlAction::Activate),
                Some(Message::Deck(DeckId(1), DeckMsg::Next))
            ));
            assert!(matches!(
                send(
                    state,
                    "deck-a/wave",
                    ControlAction::SetScalar(0.25)
                ),
                Some(Message::Deck(DeckId(0), DeckMsg::SeekTo(fraction)))
                    if (fraction - 0.25).abs() < f64::EPSILON
            ));
            assert!(matches!(
                send(
                    state,
                    "deck-a/tempo",
                    ControlAction::StepScalar(2.0)
                ),
                Some(Message::Deck(DeckId(0), DeckMsg::SetTempo(tempo)))
                    if (f32::from(tempo) - 6.0).abs() < f32::EPSILON
            ));
            assert!(matches!(
                send(state, "bar/broadcast", ControlAction::Activate),
                Some(Message::BroadcastToggle)
            ));
            assert!(matches!(
                send(
                    state,
                    "micro-bar/volume",
                    ControlAction::SetScalar(2.0)
                ),
                Some(Message::Mix(MixCmd::Trim(DeckId(0), trim)))
                    if (trim - 1.0).abs() < f32::EPSILON
            ));
            assert!(send(state, "unknown/play", ControlAction::Activate).is_none());
        }

        #[kithara::test(native, flash(false))]
        fn stream_controls_own_the_quality_menu_and_selected_rung() {
            let mut rig = Rig::offline();
            rig.shows(DeckId(0), |deck| {
                deck.abr_variants = vec![AbrVariant {
                    index: 7,
                    label: "320k".to_string(),
                    detail: "320 kbps".to_string(),
                }];
            });
            let state = &mut rig.ui;

            assert!(send(state, "deck-a/stream/cell", ControlAction::Activate).is_none());
            assert!(state.ui.cache.deck_mut(0).unwrap().view.quality_menu);
            assert!(matches!(
                send(
                    state,
                    "deck-a/stream/variant-0/cell",
                    ControlAction::Activate
                ),
                Some(Message::Deck(DeckId(0), DeckMsg::SetQuality(Some(7))))
            ));
            assert!(!state.ui.cache.deck_mut(0).unwrap().view.quality_menu);
            assert!(matches!(
                send(state, "deck-a/stream/auto/cell", ControlAction::Activate),
                Some(Message::Deck(DeckId(0), DeckMsg::SetQuality(None)))
            ));
            assert!(
                send(
                    state,
                    "deck-a/stream/cell",
                    ControlAction::SecondaryActivate
                )
                .is_none()
            );
        }

        #[kithara::test(native, flash(false))]
        fn mixer_controls_translate_levels_eq_and_stage_window() {
            let mut rig = Rig::offline();
            let state = &mut rig.ui;

            assert!(matches!(
                send(
                    state,
                    "mixer/xfade",
                    ControlAction::SetScalar(1.5)
                ),
                Some(Message::Mix(MixCmd::Crossfader(position)))
                    if (position - 1.0).abs() < f32::EPSILON
            ));
            assert!(matches!(
                send(
                    state,
                    "mixer/master",
                    ControlAction::SetScalar(-1.0)
                ),
                Some(Message::Mix(MixCmd::Master(gain))) if gain.abs() < f32::EPSILON
            ));
            assert!(send(state, "mixer/window/min", ControlAction::SetScalar(0.7)).is_none());
            assert!(send(state, "mixer/window/max", ControlAction::SetScalar(0.2)).is_none());
            assert_eq!(state.ui.cache.stage.window, (0.7, 0.7));

            assert!(matches!(
                send(state, "mixer/a/mute", ControlAction::Activate),
                Some(Message::Mix(MixCmd::Muted(DeckId(0), true)))
            ));
            assert!(matches!(
                send(
                    state,
                    "mixer/a/volume",
                    ControlAction::SetScalar(0.25)
                ),
                Some(Message::Mix(MixCmd::Trim(DeckId(0), trim)))
                    if (trim - 0.25).abs() < f32::EPSILON
            ));
            assert!(matches!(
                send(state, "mixer/a/low-3", ControlAction::SetScalar(1.0)),
                Some(Message::Deck(DeckId(0), DeckMsg::EqBandChanged(0, _)))
            ));
            assert!(
                send(
                    state,
                    "mixer/a/eq-menu-anchor",
                    ControlAction::SecondaryActivate
                )
                .is_none()
            );
            assert!(state.ui.cache.deck_mut(0).unwrap().view.eq_menu_open);
            assert!(matches!(
                send(state, "mixer/a/eq-4", ControlAction::Activate),
                Some(Message::SetEqMode(EqMode::FourBand))
            ));
            assert!(!state.ui.cache.deck_mut(0).unwrap().view.eq_menu_open);
        }

        #[kithara::test(native, flash(false))]
        fn library_and_host_events_update_view_state_and_keep_row_identity() {
            let mut rig = Rig::offline();
            let state = &mut rig.ui;

            assert!(translate(state, UiEvent::LibraryQuery("loc".to_string())).is_none());
            assert_eq!(state.ui.cache.library.query, "loc");
            state.ui.cache.library.query.clear();
            assert!(send(state, "library/browser", ControlAction::SelectIndex(1)).is_none());
            assert!(matches!(
                send(state, "library/tracks", ControlAction::SelectIndex(0)),
                Some(Message::SelectCatalogTrack(0))
            ));
            assert!(
                send(
                    state,
                    "library/tracks",
                    ControlAction::Drag(DragPhase::Start(0))
                )
                .is_none()
            );
            state.ui.cache.set_hover_deck(1, true);
            assert!(matches!(
                send(
                    state,
                    "library/tracks",
                    ControlAction::Drag(DragPhase::Drop)
                ),
                Some(Message::LoadOntoDeck(0, DeckId(1)))
            ));

            let was_collapsed = state.ui.cache.collapsed.contains("ov");
            assert!(translate(state, UiEvent::ToggleModule("ov".to_string())).is_none());
            assert_ne!(state.ui.cache.collapsed.contains("ov"), was_collapsed);
            assert!(matches!(
                translate(state, UiEvent::Window(WindowCommand::ToggleFullScreen)),
                Some(Message::Window(WindowCommand::ToggleFullScreen))
            ));
            assert!(translate(state, UiEvent::OpenSettings).is_none());
        }
    }
}
