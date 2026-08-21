use kithara_ui::render::{
    ControlAction, DEFAULT_ZOOM, DragPhase, UiEvent, WindowCommand, zoom_in, zoom_out,
};
use num_traits::cast::AsPrimitive;

use super::{
    cache::{DeckLayout, ViewCache},
    endpoints::db_from_knob,
    scope::{MICRO_DECK, deck_index, eq_band},
};
use crate::{
    deck::{DeckId, EqMode},
    gui::{
        app::Kithara,
        deck::{DeckMsg, TEMPO_STEP},
        message::Message,
        mix::MixMsg,
    },
};

/// Translate a compiled-UI event into an app message, applying host-owned
/// view state (zoom, module collapse, deck layout) in place. Control paths
/// come from the app documents and may contain nested include segments.
pub(crate) fn translate(state: &mut Kithara, event: UiEvent) -> Option<Message> {
    match event {
        UiEvent::Control { path, action } => control(state, &path, &action),
        UiEvent::ToggleModule(module) => {
            state.ui.cache.toggle_module(module);
            None
        }
        UiEvent::Window(command) => Some(Message::Window(command)),
        _ => None,
    }
}

fn control(state: &mut Kithara, path: &str, action: &ControlAction) -> Option<Message> {
    let (instance, rest) = path.split_once('/')?;
    // The micro form is one module, so the parts it carries answer a segment deeper.
    let micro = instance == "micro";
    let (instance, rest) = if micro {
        rest.split_once('/')?
    } else {
        (instance, rest)
    };
    match instance {
        "bar" if let Some(row) = rest.strip_prefix("menu/") => {
            menu_control(&mut state.ui.cache, row, action)
        }
        "bar" if micro => deck_control(state, deck_index(MICRO_DECK)?, rest, action),
        "bar" => bar_control(rest, action),
        "mixer" => mixer_control(state, rest, action),
        "library" => library_control(state, rest, action),
        "overview" => {
            let (letter, control) = rest.split_once('/')?;
            deck_control(state, deck_index(letter)?, control, action)
        }
        deck => deck_control(
            state,
            deck_index(deck.strip_prefix("deck-")?)?,
            rest,
            action,
        ),
    }
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
        ("wave", ControlAction::SetScalar(position)) => {
            let duration = state.decks.get(id)?.ui.duration.max(0.0);
            DeckMsg::SeekTo(position.clamp(0.0, 1.0) * duration)
        }
        ("wave/zoom", ControlAction::SetScalar(zoom)) => {
            state.ui.cache.deck_mut(index)?.view.zoom = Some(zoom.clamp(0.0, 1.0));
            return None;
        }
        ("tempo", ControlAction::StepScalar(steps)) => DeckMsg::SetTempo(
            steps.mul_add(TEMPO_STEP, state.decks.get(id)?.view.timestretch.tempo),
        ),
        ("tempo", ControlAction::Activate) => DeckMsg::SetTempo(0.0),
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
    let rung = state.decks.get(id)?.ui.abr_variants.get(slot)?.index;
    Some(DeckMsg::SetQuality(Some(rung)))
}

fn zoom_control(
    cache: &mut ViewCache,
    index: usize,
    control: &str,
    action: &ControlAction,
) -> Option<()> {
    let step: fn(f32) -> f32 = match (control, action) {
        ("zoom-in", ControlAction::Activate) => zoom_in,
        ("zoom-out", ControlAction::Activate) => zoom_out,
        _ => return None,
    };
    let deck = cache.deck_mut(index)?;
    deck.view.zoom = Some(step(deck.view.zoom.map_or(DEFAULT_ZOOM, AsPrimitive::as_)).into());
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
fn menu_control(cache: &mut ViewCache, control: &str, action: &ControlAction) -> Option<Message> {
    if !matches!(action, ControlAction::Activate) {
        return None;
    }
    match control {
        "burger" => cache.menu.toggle(),
        // The popover publishes its dismissal on its own path.
        "pop" | "header-close" => cache.menu.close(),
        "layouts-head" => cache.menu.toggle_layouts(),
        "modules-head" => cache.menu.toggle_modules(),
        "full-screen" => return Some(Message::Window(WindowCommand::ToggleFullScreen)),
        "cast" => return Some(Message::BroadcastToggle),
        row => {
            // A grid cell reaches the host through its own include, so the
            // module is the first segment of the path.
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
        ("xfade", ControlAction::SetScalar(position)) => Some(Message::Mix(MixMsg::Crossfader(
            position.clamp(0.0, 1.0).as_(),
        ))),
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
        ("volume", ControlAction::SetScalar(trim)) => Some(Message::Mix(MixMsg::Trim(
            deck_id(state, index)?,
            trim.clamp(0.0, 1.0).as_(),
        ))),
        (_, ControlAction::SetScalar(value)) => Some(Message::Deck(
            deck_id(state, index)?,
            eq_msg(eq_band(state.eq_mode, name)?, *value),
        )),
        _ => None,
    }
}

/// The library hands a row to whichever deck the pointer released it over.
/// Neither side knows the other: the list reports the drag it started, the
/// deck reports the pointer crossing it, and the host joins them here.
fn library_control(state: &mut Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("tracks", ControlAction::SelectIndex(index)) => Some(Message::SelectCatalogTrack(*index)),
        ("tracks", ControlAction::Drag(DragPhase::Start(row))) => {
            state.ui.cache.drag = Some(*row);
            None
        }
        ("tracks", ControlAction::Drag(DragPhase::Drop)) => {
            let (row, deck) = state.ui.cache.take_drop()?;
            Some(Message::LoadOntoDeck(row, deck_id(state, deck)?))
        }
        _ => None,
    }
}

fn deck_id(state: &Kithara, index: usize) -> Option<DeckId> {
    state.session.decks().get(index).map(|deck| deck.id)
}

fn eq_msg(band: usize, knob: f64) -> DeckMsg {
    DeckMsg::EqBandChanged(band, db_from_knob(knob.as_()))
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
    fn the_burger_opens_the_menu_and_both_dismissals_close_it() {
        let mut cache = ViewCache::default();
        assert!(!cache.menu.is_open());

        press_menu(&mut cache, "burger");
        assert!(cache.menu.is_open());
        press_menu(&mut cache, "burger");
        assert!(!cache.menu.is_open(), "the burger is also the way out");

        press_menu(&mut cache, "burger");
        press_menu(&mut cache, "pop");
        assert!(
            !cache.menu.is_open(),
            "a press outside the surface dismisses"
        );

        press_menu(&mut cache, "burger");
        press_menu(&mut cache, "header-close");
        assert!(!cache.menu.is_open());
    }

    #[kithara::test]
    fn the_layouts_group_applies_the_deck_layout_its_row_names() {
        let mut cache = ViewCache::default();
        assert!(!cache.menu.are_layouts_open());

        press_menu(&mut cache, "layouts-head");
        assert!(cache.menu.are_layouts_open());

        assert!(matches!(
            press_menu(&mut cache, "layout-1"),
            Some(Message::PauseHiddenDecks)
        ));
        assert_eq!(cache.layout(), DeckLayout::Single);
        assert!(
            cache.menu.are_layouts_open(),
            "applying a layout leaves the menu where it was"
        );

        press_menu(&mut cache, "layout-2");
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
}
