use kithara_ui::render::{ControlAction, DEFAULT_ZOOM, DragPhase, UiEvent, zoom_in, zoom_out};
use num_traits::cast::AsPrimitive;

use super::{
    cache::{DeckLayout, StudioCache},
    endpoints::{EQ_MAX_DB, EQ_MIN_DB},
    scope::deck_index,
};
use crate::{
    deck::{DeckId, EQ_BANDS},
    gui::{
        app::Kithara,
        deck::{DeckMsg, TEMPO_STEP},
        message::Message,
        mix::MixMsg,
    },
};

/// Translate a compiled-UI event into an app message, applying host-owned
/// view state (zoom, module collapse, deck layout) in place. Control paths
/// come from the studio documents: `<instance>/<control-id>`.
pub(crate) fn translate(state: &mut Kithara, event: UiEvent) -> Option<Message> {
    match event {
        UiEvent::Control { path, action } => control(state, &path, &action),
        UiEvent::ToggleModule(module) => {
            state.studio.cache.toggle_module(module);
            None
        }
        UiEvent::Window(command) => Some(Message::Window(command)),
        _ => None,
    }
}

fn control(state: &mut Kithara, path: &str, action: &ControlAction) -> Option<Message> {
    let (instance, rest) = path.split_once('/')?;
    match instance {
        "bar" => bar_control(&mut state.studio.cache, rest, action),
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
    if zoom_control(&mut state.studio.cache, index, control, action).is_some() {
        return None;
    }
    if let Some(rest) = control.strip_prefix("stream/") {
        return stream_control(state, index, rest, action);
    }
    let id = deck_id(state, index)?;
    let msg = match (control, action) {
        ("drop", ControlAction::Drag(DragPhase::Over(over))) => {
            state.studio.cache.set_hover_deck(index, *over);
            return None;
        }
        ("wave", ControlAction::SetScalar(position)) => {
            let duration = state.decks.get(id)?.ui.duration.max(0.0);
            DeckMsg::SeekTo(position.clamp(0.0, 1.0) * duration)
        }
        ("wave/zoom", ControlAction::SetScalar(zoom)) => {
            state.studio.cache.deck_mut(index)?.view.zoom = Some(zoom.clamp(0.0, 1.0));
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
        "cell" => !state.studio.cache.deck_mut(index)?.view.quality_menu,
        "pop" => false,
        row => {
            let msg = quality_msg(state, index, row)?;
            state.studio.cache.deck_mut(index)?.view.quality_menu = false;
            return Some(Message::Deck(deck_id(state, index)?, msg));
        }
    };
    state.studio.cache.deck_mut(index)?.view.quality_menu = open;
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
    cache: &mut StudioCache,
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

/// The top bar owns the studio's own view state. Narrowing the layout also
/// silences the decks it stops laying out: a deck the user cannot see must
/// not keep playing.
fn bar_control(cache: &mut StudioCache, control: &str, action: &ControlAction) -> Option<Message> {
    let ("decks", ControlAction::SelectIndex(index)) = (control, action) else {
        return None;
    };
    cache.set_layout(DeckLayout::from_index(*index)?);
    Some(Message::PauseHiddenDecks)
}

fn mixer_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("xfade", ControlAction::SetScalar(position)) => Some(Message::Mix(MixMsg::Crossfader(
            position.clamp(0.0, 1.0).as_(),
        ))),
        _ => strip_control(state, control, action),
    }
}

/// The channel strip owns both mix-side controls and the deck's tone, so its
/// instance letter addresses the deck.
fn strip_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    let (letter, name) = control.split_once('/')?;
    let index = deck_index(letter)?;
    let id = deck_id(state, index)?;
    let msg = match (name, action) {
        ("volume", ControlAction::SetScalar(trim)) => {
            Message::Mix(MixMsg::Trim(id, trim.clamp(0.0, 1.0).as_()))
        }
        (_, ControlAction::SetScalar(value)) => Message::Deck(id, eq_msg(eq_band(name)?, *value)),
        _ => return None,
    };
    Some(msg)
}

/// The library hands a row to whichever deck the pointer released it over.
/// Neither side knows the other: the list reports the drag it started, the
/// deck reports the pointer crossing it, and the host joins them here.
fn library_control(state: &mut Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("tracks", ControlAction::SelectIndex(index)) => Some(Message::SelectCatalogTrack(*index)),
        ("tracks", ControlAction::Drag(DragPhase::Start(row))) => {
            state.studio.cache.drag = Some(*row);
            None
        }
        ("tracks", ControlAction::Drag(DragPhase::Drop)) => {
            let (row, deck) = state.studio.cache.take_drop()?;
            Some(Message::LoadOntoDeck(row, deck_id(state, deck)?))
        }
        _ => None,
    }
}

fn deck_id(state: &Kithara, index: usize) -> Option<DeckId> {
    state.session.decks().get(index).map(|deck| deck.id)
}

fn eq_band(knob: &str) -> Option<usize> {
    EQ_BANDS.iter().position(|name| *name == knob)
}

fn eq_msg(band: usize, normalized: f64) -> DeckMsg {
    let normalized: f32 = normalized.clamp(0.0, 1.0).as_();
    DeckMsg::EqBandChanged(band, normalized.mul_add(EQ_MAX_DB - EQ_MIN_DB, EQ_MIN_DB))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn select_layout(cache: &mut StudioCache, layout: DeckLayout) -> Option<Message> {
        bar_control(cache, "decks", &ControlAction::SelectIndex(layout.index()))
    }

    fn press_zoom(cache: &mut StudioCache, control: &str) -> f64 {
        zoom_control(cache, 0, control, &ControlAction::Activate);
        cache.deck_mut(0).and_then(|deck| deck.view.zoom).unwrap()
    }

    #[kithara::test]
    fn the_zoom_buttons_step_the_wave_window_and_stop_at_its_bounds() {
        const PRESSES: usize = 40;

        let mut cache = StudioCache::with_decks(1);

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
    fn narrowing_the_layout_drops_a_hover_it_stops_laying_out() {
        let mut cache = StudioCache::default();
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
        let mut cache = StudioCache::default();
        cache.set_hover_deck(0, true);

        select_layout(&mut cache, DeckLayout::Single);
        select_layout(&mut cache, DeckLayout::Dual);

        cache.drag = Some(3);
        assert_eq!(cache.take_drop(), Some((3, 0)));
    }

    #[kithara::test]
    fn narrowing_the_layout_moves_a_focus_it_stops_laying_out() {
        let mut cache = StudioCache::default();
        cache.set_hover_deck(1, true);
        cache.drag = Some(3);
        cache.take_drop();
        assert_eq!(cache.focus_deck(), 1);

        select_layout(&mut cache, DeckLayout::Single);

        assert_eq!(cache.focus_deck(), 0, "the key must reach a deck on screen");
    }
}
