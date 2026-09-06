use std::num::NonZeroU32;

use kithara::{
    analysis::{AnalysisFingerprint, AnalysisProgress},
    events::TrackId,
    platform::tokio::sync::watch,
};

use crate::{
    pools::{AppQueueControl, AppResourceConfig},
    wave_cache::AnalysisTarget,
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, vis = "pub(crate)")]
pub(crate) struct Entry {
    #[field(get)]
    target: AnalysisTarget,
    #[field(get)]
    config: AppResourceConfig,
    #[field(get)]
    queue: AppQueueControl,
    #[field(get, copy)]
    track_id: TrackId,
    tx: watch::Sender<Option<AnalysisProgress>>,
    #[field(get, copy)]
    stage: Stage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Idle,
    Queued,
    Running,
    Ended(NonZeroU32),
}

impl Entry {
    pub(crate) fn new(
        target: AnalysisTarget,
        config: AppResourceConfig,
        queue: AppQueueControl,
        track_id: TrackId,
    ) -> Self {
        Self {
            target,
            config,
            queue,
            track_id,
            tx: watch::channel(None).0,
            stage: Stage::Idle,
        }
    }

    pub(crate) fn set_stage(&mut self, stage: Stage) {
        self.stage = stage;
    }

    pub(crate) fn point_at(
        &mut self,
        config: AppResourceConfig,
        queue: AppQueueControl,
        track_id: TrackId,
    ) {
        self.config = config;
        self.queue = queue;
        self.track_id = track_id;
    }

    delegate::delegate! {
        to self.tx {
            pub(crate) fn subscribe(&self) -> watch::Receiver<Option<AnalysisProgress>>;
            #[call(receiver_count)]
            #[expr($ > 0)]
            pub(crate) fn is_held(&self) -> bool;
        }
    }

    pub(crate) fn value_for(&self, axis: NonZeroU32) -> Option<AnalysisProgress> {
        self.tx
            .borrow()
            .as_ref()
            .filter(|progress| progress.analysis().source_sample_rate() == axis)
            .cloned()
    }

    pub(crate) fn release(&self) {
        if !self.is_held() {
            self.tx.send_replace(None);
        }
    }

    pub(crate) fn offer(&self, progress: AnalysisProgress) -> bool {
        let same = self.tx.borrow().as_ref().is_some_and(|held| {
            let held = held.analysis();
            let next = progress.analysis();
            held.token() == next.token() && held.revision() == next.revision()
        });
        if same {
            return false;
        }
        self.tx.send_replace(Some(progress));
        true
    }
}

pub(crate) fn settled_for(progress: &AnalysisProgress, fingerprint: &AnalysisFingerprint) -> bool {
    let analysis = progress.analysis();
    let waveform = fingerprint.waveform().is_none() || analysis.waveform().is_some();
    let beat = fingerprint.beat().is_none() || analysis.beat().is_some();
    analysis.is_settled() && waveform && beat
}
