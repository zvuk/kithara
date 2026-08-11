use kithara::{
    audio::analysis::TrackAnalysis,
    play::{PlayError, Tempo},
};
use kithara_queue::{BeatQuantum, TrackBeat};
use tracing::warn;

use super::app::Kithara;
use crate::{beatmatch, deck::DeckId};

struct Consts;

impl Consts {
    /// The host-beat grid SYNC waits for: one bar of four.
    const BAR_BEATS: f64 = 4.0;
    /// The track beat a bound deck aligns to its stamped host beat.
    const DECK_ANCHOR_BEAT: f64 = 0.0;
}

/// Edits to the session's own musical grid, shared by every deck.
///
/// Distinct from [`DeckMsg::SetTempo`], which moves one deck's playback speed.
/// This is what "on the beat" means for all of them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TransportMsg {
    /// Turn `.0`'s grid mode on or off.
    ///
    /// A mode rather than a shot: while it is on, this deck plays at the
    /// session's tempo and on the session's beat, and every play or seek puts
    /// it back there.
    ToggleSync(DeckId),
}

pub(crate) fn handle(state: &mut Kithara, msg: TransportMsg) {
    let TransportMsg::ToggleSync(id) = msg;
    let Some(deck) = state.decks.get_mut(id) else {
        return;
    };
    deck.view.sync = !deck.view.sync;
    if !deck.view.sync {
        state.pending_sync = state.pending_sync.filter(|pending| *pending != id);
        if let Err(error) = deck.controller.queue().unbind_from_grid() {
            deck.view.sync = true;
            warn!(?id, %error, "the deck could not be released from the host grid");
        }
        return;
    }
    state.pending_sync = Some(id);
    join(state);
}

/// Tries the pending join, if there is one.
///
/// Driven from the app's tick as well as from the button, because joining is
/// not a call that either succeeds or fails on the spot: adopting the host
/// tempo schedules a transport commit to a block boundary, and a deck cannot
/// be bound to a grid the graph has not committed yet. One tick later it has.
pub(crate) fn tick(state: &mut Kithara) {
    if state.pending_sync.is_some() {
        join(state);
    }
}

fn join(state: &mut Kithara) {
    let Some(id) = state.pending_sync else {
        return;
    };
    match sync(state, id) {
        Ok(Joined::Yes) => state.pending_sync = None,
        Ok(Joined::WaitingForTheGrid) => {}
        Err(error) => {
            warn!(?id, %error, "the deck could not be bound to the host grid");
            state.pending_sync = None;
        }
    }
}

/// Whether the deck is on the grid, or still waiting for one to exist.
enum Joined {
    Yes,
    WaitingForTheGrid,
}

/// Binds one deck to the host grid.
///
/// The host owns the tempo and the grid; a bound deck derives its source
/// position from the host beat, so it plays at the host tempo by construction
/// rather than by anyone matching a ratio. The first deck to ask adopts its own
/// analysed tempo as the host's, which is the spec's `AdoptPrimaryOnce` rate
/// policy.
fn sync(state: &Kithara, id: DeckId) -> Result<Joined, PlayError> {
    let deck = state
        .decks
        .get(id)
        .ok_or_else(|| PlayError::BindUnavailable {
            reason: format!("deck {id:?} is not in the studio"),
        })?;
    let analysis = deck
        .ui
        .analysis
        .clone()
        .ok_or_else(|| PlayError::BindUnavailable {
            reason: format!("deck {id:?} has no analysed beat grid yet"),
        })?;
    let index = deck
        .ui
        .current_track_index
        .ok_or_else(|| PlayError::BindUnavailable {
            reason: format!("deck {id:?} has no current track to bind"),
        })?;

    if matches!(establish_grid(state, &analysis)?, Joined::WaitingForTheGrid) {
        return Ok(Joined::WaitingForTheGrid);
    }

    let anchor =
        TrackBeat::new(Consts::DECK_ANCHOR_BEAT).map_err(|reason| PlayError::BindUnavailable {
            reason: reason.to_string(),
        })?;
    let quantum =
        BeatQuantum::new(Consts::BAR_BEATS).map_err(|reason| PlayError::BindUnavailable {
            reason: reason.to_string(),
        })?;
    deck.controller
        .queue()
        .start_at_beat(index, &analysis, anchor, quantum)
        .map(|()| Joined::Yes)
        .map_err(|reason| PlayError::BindUnavailable {
            reason: reason.to_string(),
        })
}

/// Commits a grid when the session has none. A session that already carries
/// one is left alone: re-committing would re-anchor it and move every deck
/// already following it.
fn establish_grid(state: &Kithara, analysis: &TrackAnalysis) -> Result<Joined, PlayError> {
    let session = state.session.session();
    if session.transport().is_ok() {
        return Ok(Joined::Yes);
    }
    let bpm = beatmatch::deck_bpm(analysis).ok_or_else(|| PlayError::BindUnavailable {
        reason: "the analysis carries no tempo to open the session with".to_owned(),
    })?;
    let tempo = Tempo::new(bpm).map_err(|reason| PlayError::BindUnavailable {
        reason: reason.to_string(),
    })?;
    session.set_session_tempo(tempo)?;
    session.set_session_playing(true)?;
    // The commit is scheduled to a block boundary, so the grid does not exist
    // until the graph has rendered through it. The caller comes back.
    Ok(Joined::WaitingForTheGrid)
}
