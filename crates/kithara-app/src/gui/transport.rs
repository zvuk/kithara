use kithara::{
    audio::{BeatGrid, analysis::TrackAnalysis},
    play::{PlayError, Tempo},
};
use kithara_queue::{BeatQuantum, TrackBeat};
use tracing::warn;

use super::app::Kithara;
use crate::deck::DeckId;

struct Consts;

impl Consts {
    /// The session-beat grid SYNC waits for: one bar of four.
    const BAR_BEATS: f64 = 4.0;
    /// The track beat a synced deck aligns to its stamped session beat.
    const DECK_ANCHOR_BEAT: f64 = 0.0;
}

/// Edits to the session's own musical grid, shared by every deck.
///
/// Distinct from [`super::deck::DeckMsg::SetTempo`], which moves one deck's
/// playback speed. This is what "on the beat" means for all of them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TransportMsg {
    /// Place `.0` on the session grid, establishing the grid from that deck's
    /// own analysis when the session has none yet.
    Sync(DeckId),
}

pub(crate) fn handle(state: &mut Kithara, msg: TransportMsg) {
    let TransportMsg::Sync(id) = msg;
    if let Err(error) = sync(state, id) {
        warn!(?id, %error, "the deck could not be placed on the session grid");
    }
}

/// A deck joins the grid, and the first one to ask *is* the grid.
///
/// A DJ does not dial the session tempo in by hand: the first record on
/// defines it and everything after matches. So the first SYNC adopts this
/// deck's analysed tempo and starts the clock, and every later one aligns to
/// what is already committed. The tempo comes from the analysis the app
/// already holds — nothing here invents a number.
fn sync(state: &Kithara, id: DeckId) -> Result<(), PlayError> {
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
            reason: "the deck has no analysed beat grid yet".to_owned(),
        })?;
    let index = deck
        .ui
        .current_track_index
        .ok_or_else(|| PlayError::BindUnavailable {
            reason: "the deck has no current track to place".to_owned(),
        })?;

    establish_grid(state, &analysis)?;

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
        .map_err(|reason| PlayError::BindUnavailable {
            reason: reason.to_string(),
        })
}

/// Commits a grid when the session has none. A session that already carries
/// one is left alone: re-committing would re-anchor it and move every deck
/// already following it.
fn establish_grid(state: &Kithara, analysis: &TrackAnalysis) -> Result<(), PlayError> {
    let session = state.session.session();
    if session.transport().is_ok() {
        return Ok(());
    }
    let bpm = analysis
        .beat()
        .map(BeatGrid::bpm)
        .filter(|bpm| bpm.is_finite())
        .ok_or_else(|| PlayError::BindUnavailable {
            reason: "the analysis carries no tempo to open the session with".to_owned(),
        })?;
    let tempo = Tempo::new(bpm).map_err(|reason| PlayError::BindUnavailable {
        reason: reason.to_string(),
    })?;
    session.set_session_tempo(tempo)?;
    session.set_session_playing(true)
}
