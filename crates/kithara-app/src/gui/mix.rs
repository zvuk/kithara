use kithara::play::PlayError;
use tracing::error;

use super::app::Kithara;
use crate::deck::DeckId;

/// Session-mix edits. Every one of them is a level, never a track volume.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MixMsg {
    Crossfader(f32),
    Trim(DeckId, f32),
}

/// A rejected edit leaves the stored mix untouched (`DeckSet::commit` is
/// transactional), so a failure only needs reporting.
pub(crate) fn handle(state: &mut Kithara, msg: MixMsg) {
    let result = match msg {
        MixMsg::Crossfader(position) => state.session.set_crossfader(position),
        MixMsg::Trim(id, trim) => state.session.set_trim(id, trim),
    };
    if let Err(e) = result {
        report(&e, msg);
    }
}

fn report(error: &PlayError, msg: MixMsg) {
    error!(?msg, %error, "mix edit rejected");
}
