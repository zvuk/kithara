use std::num::NonZeroU32;

use kithara::{
    analysis::{AnalysisProducer, AnalysisProgress},
    events::TrackId,
    platform::tokio::{
        sync::watch,
        task::{self, JoinHandle},
    },
};
use tracing::{debug, warn};

use super::{
    entry::{Stage, settled_for},
    service::Owner,
};
use crate::{
    pools::AppQueueControl,
    wave_cache::{AnalysisPersistenceError, token_for},
};

pub(super) enum Activity {
    Running(Run),
    Committing(JoinHandle<Result<(), AnalysisPersistenceError>>),
}

pub(super) struct Run {
    pub(super) entry: usize,
    pub(super) axis: NonZeroU32,
    pub(super) rx: watch::Receiver<Option<AnalysisProgress>>,
    /// Set when the owner ended the pass itself; the entry goes back in line.
    pub(super) requeue: bool,
}

impl Owner {
    /// Open a pass for the entry unless the value it holds is settled. A
    /// resumable seed resumes; a rejected checkpoint is no seed, so the pass
    /// opens fresh above the revision the entry holds.
    pub(super) fn open_run(&mut self, index: usize, axis: NonZeroU32) -> Option<Run> {
        self.seed(index, axis);
        let fingerprint = self.runner.fingerprint();
        let entry = &mut self.entries[index];
        let track_id = entry.track_id();
        let held = entry.is_held();
        let seed = entry.value_for(axis);
        if seed
            .as_ref()
            .is_some_and(|progress| settled_for(progress, fingerprint))
        {
            entry.set_stage(Stage::Ended(axis));
            entry.release();
            return None;
        }
        let queue = entry.queue().clone();
        let config = entry.config().clone();
        let revision = seed
            .as_ref()
            .map_or(0, |progress| progress.analysis().revision());
        let resumed = seed
            .filter(AnalysisProgress::is_resumable)
            .and_then(|progress| {
                self.runner
                    .resume(config.clone(), progress, deliver(&queue, track_id))
                    .inspect_err(|error| {
                        warn!(%error, ?track_id, "analysis: cached checkpoint rejected");
                    })
                    .ok()
            });
        let fresh = resumed.is_none();
        let rx = resumed.unwrap_or_else(|| {
            self.runner.analyze(
                config,
                token_for(entry.target().key()),
                axis,
                revision,
                deliver(&queue, track_id),
            )
        });
        debug!(
            ?track_id,
            held,
            fresh,
            axis = axis.get(),
            "analysis: pass opened"
        );
        entry.set_stage(Stage::Running);
        Some(Run {
            entry: index,
            axis,
            rx,
            requeue: false,
        })
    }

    /// Await the pass's next publication or the commit of its last one.
    /// Parks while the runner is idle.
    pub(super) async fn drive(&mut self) {
        match &mut self.active {
            Some(Activity::Running(run)) => {
                if run.rx.changed().await.is_err() {
                    self.finish_run();
                    self.pump();
                } else {
                    self.publish();
                }
            }
            Some(Activity::Committing(task)) => {
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!(%error, "analysis: final checkpoint commit failed"),
                    Err(error) => warn!(%error, "analysis: final checkpoint task failed"),
                }
                self.active = None;
                self.pump();
            }
            None => std::future::pending().await,
        }
    }

    /// Hand the pass's latest revision to the entry, the cache, and (best
    /// effort) persistence.
    pub(super) fn publish(&mut self) {
        let Some(Activity::Running(run)) = &self.active else {
            return;
        };
        let Some(progress) = run.rx.borrow().clone() else {
            return;
        };
        let entry = &self.entries[run.entry];
        let revision = progress.analysis().revision();
        let complete = progress.analysis().is_complete();
        let target = entry.target().clone();
        self.cache.put(target.clone(), progress.clone());
        let queued = self.persistence.try_store(target, progress.clone());
        let sent = entry.offer(progress);
        debug!(
            track_id = ?entry.track_id(),
            revision,
            complete,
            held = entry.is_held(),
            sent,
            queued,
            "analysis: revision published"
        );
    }

    /// The pass closed: keep its last value everywhere and commit it before
    /// the runner takes the next entry. The entry is done only when the pass
    /// ran its course; a close with nothing settled leaves it for the next
    /// request.
    pub(super) fn finish_run(&mut self) {
        let Some(Activity::Running(run)) = self.active.take() else {
            return;
        };
        let progress = run.rx.borrow().clone();
        let ran_its_course = progress
            .as_ref()
            .is_some_and(|progress| progress.analysis().is_settled());
        let entry = &mut self.entries[run.entry];
        let track_id = entry.track_id();
        if run.requeue {
            entry.set_stage(Stage::Queued);
            self.pending.push_back(run.entry);
        } else if ran_its_course {
            entry.set_stage(Stage::Ended(run.axis));
        } else {
            entry.set_stage(Stage::Idle);
        }
        let Some(progress) = progress else {
            debug!(
                ?track_id,
                requeued = run.requeue,
                "analysis: pass closed without a value"
            );
            entry.release();
            return;
        };
        let target = entry.target().clone();
        self.cache.put(target.clone(), progress.clone());
        let sent = entry.offer(progress.clone());
        entry.release();
        debug!(
            ?track_id,
            revision = progress.analysis().revision(),
            complete = progress.analysis().is_complete(),
            held = entry.is_held(),
            sent,
            requeued = run.requeue,
            "analysis: final published"
        );
        let persistence = self.persistence.clone();
        let commit = task::spawn(async move { persistence.store(target, progress).await });
        self.active = Some(Activity::Committing(commit));
    }
}

/// Where a pass hands its producer: the playback path of the track it was
/// opened for.
fn deliver(queue: &AppQueueControl, track_id: TrackId) -> impl FnOnce(AnalysisProducer) {
    let queue = queue.clone();
    move |producer| queue.attach_observer(track_id, producer)
}
