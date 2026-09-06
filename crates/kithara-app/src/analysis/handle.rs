use std::num::NonZeroU32;

use kithara::{
    analysis::AnalysisProgress,
    events::TrackId,
    platform::tokio::sync::{mpsc, oneshot, watch},
};
use tracing::debug;

use crate::pools::{AppQueueControl, AppTrackSource};

pub(crate) enum Request {
    Subscribe {
        queue: AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
        axis: NonZeroU32,
        reply: oneshot::Sender<watch::Receiver<Option<AnalysisProgress>>>,
    },
    Warm {
        queue: AppQueueControl,
        track_ids: Vec<TrackId>,
        axis: NonZeroU32,
    },
}

#[derive(Clone)]
pub(crate) struct AnalysisHandle {
    tx: mpsc::Sender<Request>,
}

impl AnalysisHandle {
    const QUEUE_DEPTH: usize = 32;

    pub(crate) fn channel() -> (Self, mpsc::Receiver<Request>) {
        let (tx, rx) = mpsc::channel(Self::QUEUE_DEPTH);
        (Self { tx }, rx)
    }

    pub(crate) async fn subscribe(
        &self,
        queue: AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
        axis: NonZeroU32,
    ) -> Option<watch::Receiver<Option<AnalysisProgress>>> {
        let (reply, receiver) = oneshot::channel();
        self.tx
            .send(Request::Subscribe {
                queue,
                track_id,
                source,
                axis,
                reply,
            })
            .await
            .ok()?;
        receiver.await.ok()
    }

    pub(crate) async fn warm(
        &self,
        queue: AppQueueControl,
        track_ids: Vec<TrackId>,
        axis: NonZeroU32,
    ) {
        if self
            .tx
            .send(Request::Warm {
                queue,
                track_ids,
                axis,
            })
            .await
            .is_err()
        {
            debug!("analysis: warm request dropped; the owner is gone");
        }
    }
}
