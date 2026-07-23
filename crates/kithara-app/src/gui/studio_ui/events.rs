use kithara_ui::render::{ControlAction, UiEvent};
use num_traits::cast::AsPrimitive;

use super::endpoints::{EQ_MAX_DB, EQ_MIN_DB, TS_RANGES};
use crate::{
    catalog,
    deck::DeckId,
    gui::{app::Kithara, deck::DeckMsg, message::Message, mix::MixMsg},
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
    if let Some(rest) = path.strip_prefix("deck-a/") {
        return deck_control(state, 0, rest, action);
    }
    if let Some(rest) = path.strip_prefix("deck-b/") {
        return deck_control(state, 1, rest, action);
    }
    if let Some(rest) = path.strip_prefix("mixer/") {
        return mixer_control(state, rest, action);
    }
    if let Some(rest) = path.strip_prefix("library/") {
        return library_control(state, rest, action);
    }
    None
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
        ("range", ControlAction::SelectIndex(index)) => {
            DeckMsg::SetRange(*TS_RANGES.get(*index)?)
        }
        ("ts-tempo", ControlAction::SetScalar(normalized)) => {
            let range = f32::from(state.decks.get(id)?.view.timestretch.range);
            let normalized: f32 = normalized.clamp(0.0, 1.0).as_();
            DeckMsg::SetTempo(normalized.mul_add(range * 2.0, -range))
        }
        ("reset", ControlAction::Activate) => DeckMsg::ResetTempo,
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
        ("low", ControlAction::SetScalar(value)) => eq_msg(0, *value),
        ("mid", ControlAction::SetScalar(value)) => eq_msg(1, *value),
        ("high", ControlAction::SetScalar(value)) => eq_msg(2, *value),
        _ => return None,
    };
    Some(Message::Deck(id, msg))
}

fn mixer_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    let msg = match (control, action) {
        ("trim-a", ControlAction::SetScalar(trim)) => {
            MixMsg::Trim(deck_id(state, 0)?, trim.clamp(0.0, 1.0).as_())
        }
        ("trim-b", ControlAction::SetScalar(trim)) => {
            MixMsg::Trim(deck_id(state, 1)?, trim.clamp(0.0, 1.0).as_())
        }
        ("mute-a", ControlAction::Activate) => toggle_mute(state, 0)?,
        ("mute-b", ControlAction::Activate) => toggle_mute(state, 1)?,
        ("xfade", ControlAction::SetScalar(position)) => {
            MixMsg::Crossfader(position.clamp(0.0, 1.0).as_())
        }
        ("master", ControlAction::SetScalar(master)) => {
            MixMsg::GroupMaster(master.clamp(0.0, 1.0).as_())
        }
        _ => return None,
    };
    Some(Message::Mix(msg))
}

/// Track-list assign chips carry the deck letter in the path; the letter is
/// the deck's position in the session, matching the channel order. A chip is
/// a toggle: it loads the row onto its deck, or removes it when already on.
fn library_control(state: &Kithara, control: &str, action: &ControlAction) -> Option<Message> {
    match (control, action) {
        ("tracks", ControlAction::SelectIndex(index)) => Some(Message::SelectCatalogTrack(*index)),
        (_, ControlAction::SelectIndex(row)) => {
            let letter = control.strip_prefix("tracks/assign/")?;
            let deck = letter.bytes().next()?.checked_sub(b'a')?;
            let id = deck_id(state, deck.into())?;
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

fn toggle_mute(state: &Kithara, index: usize) -> Option<MixMsg> {
    let muted = state.session.mix().strips.get(index)?.muted;
    Some(MixMsg::Mute(deck_id(state, index)?, !muted))
}

fn deck_id(state: &Kithara, index: usize) -> Option<DeckId> {
    state.session.decks().get(index).map(|deck| deck.id)
}

fn eq_msg(band: usize, normalized: f64) -> DeckMsg {
    let normalized: f32 = normalized.clamp(0.0, 1.0).as_();
    DeckMsg::EqBandChanged(band, normalized.mul_add(EQ_MAX_DB - EQ_MIN_DB, EQ_MIN_DB))
}
