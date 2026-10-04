use std::{
    future::{Future, poll_fn},
    num::NonZeroU32,
    task::Poll,
};

use kithara::{
    analysis::{AnalysisProducer, AnalysisProgress},
    events::TrackId,
    platform::tokio::{
        self,
        sync::watch,
        task::{self, JoinError, JoinHandle},
    },
};
use tracing::{debug, warn};

use super::{
    entry::{Entry, Stage},
    load::LoadReply,
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
    pub(super) axis: NonZeroU32,
    pub(super) rx: watch::Receiver<Option<AnalysisProgress>>,
    pub(super) requeue: bool,
    pub(super) entry: usize,
}

/// What woke the owner's one loop.
enum Woke {
    Unheld(usize),
    /// An artifact its own source answered for.
    Load(LoadReply),
    /// The pass in flight published a revision.
    Progress,
    /// The pass in flight is over.
    Finished,
    /// The final checkpoint commit returned.
    Committed(Result<Result<(), AnalysisPersistenceError>, JoinError>),
}

impl Owner {
    pub(super) async fn drive(&mut self) -> bool {
        let woke = self.wake().await;
        let changed = !matches!(&woke, Woke::Progress);
        match woke {
            Woke::Unheld(index) => self.entries[index].release(),
            Woke::Load(reply) => self.take_load(reply),
            Woke::Progress => self.publish(),
            Woke::Finished => {
                self.finish_run();
                self.pump();
            }
            Woke::Committed(result) => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!(%error, "analysis: final checkpoint commit failed"),
                    Err(error) => warn!(%error, "analysis: final checkpoint task failed"),
                }
                self.active = None;
                self.pump();
            }
        }
        self.prune_entries();
        changed
    }

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
            entry.set_stage(Stage::Failed(run.axis));
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

    pub(super) fn open_run(&mut self, index: usize, axis: NonZeroU32) -> Option<Run> {
        self.seed(index, axis);
        let fingerprint = self.runner.fingerprint();
        let entry = &mut self.entries[index];
        let track_id = entry.track_id();
        let held = entry.is_held();
        let seed = entry.value_for(axis);
        if seed
            .as_ref()
            .is_some_and(|progress| entry.prepared().settled_for(progress, fingerprint))
        {
            debug!(
                ?track_id,
                held, "analysis: the cached value settles the track"
            );
            entry.set_stage(Stage::Ended(axis));
            entry.release();
            return None;
        }
        let demand = entry.prepared().demand(fingerprint);
        if demand.is_empty() {
            debug!(
                ?track_id,
                held, "analysis: every artifact is prepared or published; no pass opened"
            );
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
        let rx = match resumed {
            Some(rx) => rx,
            None => match self.runner.analyze(
                config,
                token_for(entry.target().key()),
                axis,
                revision,
                demand,
                deliver(&queue, track_id),
            ) {
                Ok(rx) => rx,
                Err(error) => {
                    warn!(%error, ?track_id, "analysis: pass could not open its ingress");
                    entry.set_stage(Stage::Ended(axis));
                    entry.release();
                    return None;
                }
            },
        };
        debug!(
            ?track_id,
            held,
            fresh,
            beat = demand.beat(),
            waveform = demand.waveform(),
            axis = axis.get(),
            "analysis: pass opened"
        );
        entry.set_stage(Stage::Running);
        Some(Run {
            axis,
            rx,
            entry: index,
            requeue: false,
        })
    }

    pub(super) fn publish(&mut self) {
        let Some(Activity::Running(run)) = &self.active else {
            return;
        };
        let Some(progress) = run.rx.borrow().clone() else {
            return;
        };
        let index = run.entry;
        let entry = &self.entries[index];
        let revision = progress.analysis().revision();
        let complete = progress.analysis().is_complete();
        let target = entry.target().clone();
        let track_id = entry.track_id();
        self.cache.put(target.clone(), progress.clone());
        let queued = self.persistence.try_store(target, progress.clone());
        let entry = &mut self.entries[index];
        let sent = entry.offer(progress);
        debug!(
            ?track_id,
            revision,
            complete,
            held = entry.is_held(),
            sent,
            queued,
            "analysis: revision published"
        );
    }

    /// Wait for a subscriber leaving, an artifact read, or the active pass.
    /// Artifact reads precede pass progress because prepared artifacts remove work.
    async fn wake(&mut self) -> Woke {
        let Self {
            active,
            replies,
            entries,
            ..
        } = self;
        let activity = async {
            match active {
                Some(Activity::Running(run)) => tokio::select! {
                    biased;
                    Some(reply) = replies.recv() => Woke::Load(reply),
                    changed = run.rx.changed() => if changed.is_err() {
                        Woke::Finished
                    } else {
                        Woke::Progress
                    },
                },
                Some(Activity::Committing(task)) => tokio::select! {
                    biased;
                    Some(reply) = replies.recv() => Woke::Load(reply),
                    result = task => Woke::Committed(result),
                },
                None => match replies.recv().await {
                    Some(reply) => Woke::Load(reply),
                    None => std::future::pending().await,
                },
            }
        };
        tokio::select! {
            index = closed_publication(entries) => Woke::Unheld(index),
            woke = activity => woke,
        }
    }
}

async fn closed_publication(entries: &[Entry]) -> usize {
    let mut closed: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.has_terminal_publication())
        .map(|(index, entry)| (index, Box::pin(entry.closed())))
        .collect();
    poll_fn(|cx| {
        for (index, future) in &mut closed {
            if future.as_mut().poll(cx).is_ready() {
                return Poll::Ready(*index);
            }
        }
        Poll::Pending
    })
    .await
}

fn deliver(queue: &AppQueueControl, track_id: TrackId) -> impl FnOnce(AnalysisProducer) {
    let queue = queue.clone();
    move |producer| queue.attach_observer(track_id, producer)
}
