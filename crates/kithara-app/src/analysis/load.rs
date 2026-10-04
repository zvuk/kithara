use kithara::{
    analysis::BeatGridModel,
    events::TrackId,
    platform::{
        sync::Arc,
        tokio::{sync::mpsc, task},
    },
    play::ArtifactLoadError,
    waveform::Waveform,
};
use tracing::{debug, warn};

use super::service::Owner;
use crate::wave_cache::AnalysisTarget;

/// One artifact, as its external source answered for it.
pub(super) enum Loaded {
    BeatGrid(Result<Arc<BeatGridModel>, ArtifactLoadError>),
    Waveform(Result<Arc<Waveform>, ArtifactLoadError>),
}

impl Loaded {
    /// Why the load failed, when it did.
    const fn error(&self) -> Option<&ArtifactLoadError> {
        match self {
            Self::BeatGrid(Err(error)) | Self::Waveform(Err(error)) => Some(error),
            Self::BeatGrid(Ok(_)) | Self::Waveform(Ok(_)) => None,
        }
    }

    /// Hand this artifact back to the owner. `false` once the owner is gone,
    /// which is the whole answer: nothing is left to read for.
    async fn hand(
        self,
        tx: &mpsc::Sender<LoadReply>,
        epoch: u64,
        target: AnalysisTarget,
        track_id: TrackId,
    ) -> bool {
        debug!(?track_id, kind = self.kind(), "analysis: artifact read");
        tx.send(LoadReply {
            epoch,
            target,
            loaded: self,
        })
        .await
        .is_ok()
    }

    /// What the artifact is called in a log line.
    const fn kind(&self) -> &'static str {
        match self {
            Self::BeatGrid(_) => "beat grid",
            Self::Waveform(_) => "waveform",
        }
    }
}

/// An artifact load answering for the entry and the epoch it was started on.
///
/// The epoch is what keeps a late answer out: an entry re-pointed at another
/// track, reloaded, or released has moved on, and the reply is dropped instead
/// of overwriting whatever that entry holds now.
pub(super) struct LoadReply {
    pub(super) loaded: Loaded,
    pub(super) epoch: u64,
    pub(super) target: AnalysisTarget,
}

impl Owner {
    /// Read whatever external artifacts this entry is still waiting on.
    ///
    /// Both artifacts of one track are read by one task, in order: they share
    /// a host and a connection, and neither is on a deadline the other cares
    /// about. The task holds only a cloned configuration, so it outlives
    /// nothing and cancels with the load epoch the configuration carries.
    pub(super) fn start_loads(&self, index: usize) {
        let entry = &self.entries[index];
        let epoch = entry.epoch();
        let config = entry.config().clone();
        let beat_grid = entry
            .prepared()
            .beat_grid
            .is_pending()
            .then(|| config.beat_grid().cloned())
            .flatten();
        let waveform = entry
            .prepared()
            .waveform
            .is_pending()
            .then(|| config.waveform().cloned())
            .flatten();
        if beat_grid.is_none() && waveform.is_none() {
            return;
        }
        let track_id = entry.track_id();
        let target = entry.target().clone();
        let tx = self.loads.clone();
        task::spawn(async move {
            let fetch = config.artifact_fetch();
            if let Some(source) = beat_grid
                && !Loaded::BeatGrid(source.load(&fetch).await)
                    .hand(&tx, epoch, target.clone(), track_id)
                    .await
            {
                return;
            }
            if let Some(source) = waveform {
                Loaded::Waveform(source.load(&fetch).await)
                    .hand(&tx, epoch, target, track_id)
                    .await;
            }
        });
    }

    /// Accept an artifact its source answered with, unless the entry moved on.
    pub(super) fn take_load(&mut self, reply: LoadReply) {
        let LoadReply {
            epoch,
            target,
            loaded,
        } = reply;
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.target().is_same(&target))
        else {
            return;
        };
        if entry.epoch() != epoch {
            debug!(
                kind = loaded.kind(),
                "analysis: artifact answered for a load that is over"
            );
            return;
        }
        if let Some(error) = loaded.error() {
            warn!(
                %error,
                track_id = ?entry.track_id(),
                "analysis: prepared artifact unavailable"
            );
        }
        entry.accept(loaded);
    }
}
