use kithara_ui::render::{ControlAction, UiEvent};
use num_traits::cast::AsPrimitive;

use super::{
    endpoints::{EQ_MAX_DB, EQ_MIN_DB},
    scope::deck_index,
};
use crate::{
    catalog,
    deck::DeckId,
    gui::{
        app::Kithara,
        deck::{DeckMsg, TEMPO_RANGE},
        message::Message,
        mix::MixMsg,
    },
};

/// Translate a compiled-UI event into an app message, applying host-owned
/// view state (zoom, module collapse) in place. Control paths come from the
/// studio documents: `<instance>/<control-id>`.
pub(crate) fn translate(state: &mut Kithara, event: UiEvent) -> Option<Message> {
    match event {
        UiEvent::Control { path, action } => control(state, &path, &action),
        UiEvent::ToggleModule(module) => {
            state.studio.cache.toggle_module(module);
            None
        }
        _ => None,
    }
}

fn control(state: &mut Kithara, path: &str, action: &ControlAction) -> Option<Message> {
    let (instance, rest) = path.split_once('/')?;
    match instance {
        "mixer" => mixer_control(state, rest, action),
        "library" => library_control(state, rest, action),
        _ => {
            let index = deck_index(instance.strip_prefix("deck-")?)?;
            deck_control(state, index, rest, action)
        }
    }
}

fn deck_control(
    state: &mut Kithara,
    index: usize,
    control: &str,
    action: &ControlAction,
) -> Option<Message> {
    let id = deck_id(state, index)?;
    let msg = match (control, action) {
        ("wave", ControlAction::SetScalar(position)) => {
            let duration = state.decks.get(id)?.ui.duration.max(0.0);
            DeckMsg::SeekTo(position.clamp(0.0, 1.0) * duration)
        }
        ("wave/zoom", ControlAction::SetScalar(zoom)) => {
            state.studio.cache.deck_mut(index)?.zoom = Some(zoom.clamp(0.0, 1.0));
            return None;
        }
        ("play", ControlAction::Activate) => DeckMsg::TogglePlayPause,
        ("prev", ControlAction::Activate) => DeckMsg::Prev,
        ("next", ControlAction::Activate) => DeckMsg::Next,
        ("keylock", ControlAction::Activate) => {
            #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
            {
                DeckMsg::ToggleKeyLock
            }
            #[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
            {
                return None;
            }
        }
        _ => return None,
    };
    Some(Message::Deck(id, msg))
}

fn mixer_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("xfade", ControlAction::SetScalar(position)) => Some(Message::Mix(MixMsg::Crossfader(
            position.clamp(0.0, 1.0).as_(),
        ))),
        ("master", ControlAction::SetScalar(master)) => Some(Message::Mix(MixMsg::GroupMaster(
            master.clamp(0.0, 1.0).as_(),
        ))),
        _ => strip_control(state, control, action),
    }
}

/// The channel strip owns both mix-side controls and the deck's tone and
/// tempo, so its instance letter addresses the deck.
fn strip_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    let (letter, name) = control.split_once('/')?;
    let index = deck_index(letter)?;
    let id = deck_id(state, index)?;
    let msg = match (name, action) {
        ("trim", ControlAction::SetScalar(trim)) => {
            Message::Mix(MixMsg::Trim(id, trim.clamp(0.0, 1.0).as_()))
        }
        ("mute", ControlAction::Activate) => {
            let muted = state.session.mix().strips.get(index)?.muted;
            Message::Mix(MixMsg::Mute(id, !muted))
        }
        ("low", ControlAction::SetScalar(value)) => Message::Deck(id, eq_msg(0, *value)),
        ("mid", ControlAction::SetScalar(value)) => Message::Deck(id, eq_msg(1, *value)),
        ("high", ControlAction::SetScalar(value)) => Message::Deck(id, eq_msg(2, *value)),
        ("tempo", ControlAction::SetScalar(normalized)) => {
            let normalized: f32 = normalized.clamp(0.0, 1.0).as_();
            let tempo = normalized.mul_add(TEMPO_RANGE * 2.0, -TEMPO_RANGE);
            Message::Deck(id, DeckMsg::SetTempo(tempo))
        }
        _ => return None,
    };
    Some(msg)
}

/// Track-list assign chips carry the deck letter in the path; the letter is
/// the deck's position in the session, matching the channel order. A chip is
/// a toggle: it loads the row onto its deck, or removes it when already on.
fn library_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("tracks", ControlAction::SelectIndex(index)) => Some(Message::SelectCatalogTrack(*index)),
        (_, ControlAction::SelectIndex(row)) => {
            let letter = control.strip_prefix("tracks/assign/")?;
            let id = deck_id(state, deck_index(letter)?)?;
            let entry = state.catalog.get(*row)?;
            let queue = state.decks.get(id)?.controller.queue();
            if catalog::is_loaded(queue, entry) {
                Some(Message::UnloadFromDeck(*row, id))
            } else {
                Some(Message::LoadOntoDeck(*row, id))
            }
        }
        _ => None,
    }
}

fn deck_id(state: &Kithara, index: usize) -> Option<DeckId> {
    state.session.decks().get(index).map(|deck| deck.id)
}

fn eq_msg(band: usize, normalized: f64) -> DeckMsg {
    let normalized: f32 = normalized.clamp(0.0, 1.0).as_();
    DeckMsg::EqBandChanged(band, normalized.mul_add(EQ_MAX_DB - EQ_MIN_DB, EQ_MIN_DB))
}
