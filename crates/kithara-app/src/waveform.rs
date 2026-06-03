use kithara::{
    audio::{AnalysisParams, ChunkOutcome, Waveform, WaveformAnalyzer},
    prelude::{Resource, ResourceConfig},
};
use kithara_platform::{CancellationToken, thread::sleep, tokio::task::spawn_blocking};
use tokio::{sync::watch, task::JoinHandle};
use tracing::{debug, warn};

mod consts {
    use std::time::Duration;

    /// Backoff while the reader is buffering and has no chunk ready.
    pub(super) const PENDING_BACKOFF: Duration = Duration::from_millis(5);
}
use consts::*;

/// Whole-track source analysis.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TrackAnalysis {
    pub waveform: Waveform,
}

/// Per-deck track analysis. Not a singleton: each deck owns one with its
/// own cancel scope (a child of the app master), so two decks analyse
/// independently. Dropping it cancels any in-flight run.
pub struct TrackAnalysisRunner {
    cancel: CancellationToken,
    current: Option<RunHandle>,
}

/// An in-flight run: its child token and the spawned task. Owned so the task
/// is never detached. Teardown is cooperative - cancelling the token makes the
/// decode loop exit at its next per-chunk check; `spawn_blocking` cannot be
/// force-aborted and `Drop` cannot await, so this is cancellation, not a join.
struct RunHandle {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl TrackAnalysisRunner {
    /// `master` must be a child of the app master cancel; this run scope is a
    /// child of it.
    #[must_use]
    pub fn new(master: &CancellationToken) -> Self {
        Self {
            cancel: master.child_token(),
            current: None,
        }
    }

    /// Cancel any prior run and start analysing `config` into `buckets` off the
    /// player runtime. The latest result (or none on failure/cancel) arrives on
    /// the returned receiver.
    pub fn analyze(
        &mut self,
        config: ResourceConfig,
        buckets: usize,
    ) -> watch::Receiver<Option<TrackAnalysis>> {
        self.clear();

        let run = self.cancel.child_token();
        let (tx, rx) = watch::channel(None);
        let task = tokio::spawn(run_analysis(config, buckets, run.clone(), tx));
        self.current = Some(RunHandle { cancel: run, task });
        rx
    }

    /// Cancel the in-flight run, if any, without starting a new one. Cancels
    /// the run token (the blocking decode exits at its next chunk check) and
    /// aborts the async wrapper so its task and channel drop promptly.
    pub fn clear(&mut self) {
        if let Some(prev) = self.current.take() {
            prev.cancel.cancel();
            prev.task.abort();
        }
    }
}

impl Drop for TrackAnalysisRunner {
    fn drop(&mut self) {
        self.clear();
        self.cancel.cancel();
    }
}

/// Open and decode `config` end to end off the player runtime, sending the
/// finished [`Waveform`] on `tx`. Nothing is sent on failure or cancel.
async fn run_analysis(
    config: ResourceConfig,
    buckets: usize,
    cancel: CancellationToken,
    tx: watch::Sender<Option<TrackAnalysis>>,
) {
    if let Some(analysis) = analyze_track(config, buckets, cancel).await {
        // The receiver may be gone (deck swapped); a failed send is fine.
        let _ = tx.send(Some(analysis));
    }
}

/// Decode `config` end to end off the player runtime and return the finished
/// [`Waveform`]. `None` on a zero bucket count, an up-front cancel, a resource
/// failure, or empty input.
pub async fn analyze(
    config: ResourceConfig,
    buckets: usize,
    cancel: CancellationToken,
) -> Option<Waveform> {
    analyze_track(config, buckets, cancel)
        .await
        .map(|analysis| analysis.waveform)
}

/// Decode `config` end to end off the player runtime and return the finished
/// source analysis. `None` on a zero bucket count, an up-front cancel, a
/// resource failure, or empty input.
pub async fn analyze_track(
    config: ResourceConfig,
    buckets: usize,
    cancel: CancellationToken,
) -> Option<TrackAnalysis> {
    if buckets == 0 || cancel.is_cancelled() {
        return None;
    }

    let mut resource = match Resource::new(config).await {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, "waveform: resource open failed");
            return None;
        }
    };
    if let Err(e) = resource.preload().await {
        warn!(?e, "waveform: preload failed");
        return None;
    }

    spawn_blocking(move || decode_track(resource, buckets, &cancel))
        .await
        .ok()
        .flatten()
}

/// Decode the resource into a [`TrackAnalysis`] of `buckets` columns. The analyzer is built lazily
/// on the first chunk so it can take the source `sample_rate`.
/// Returns `None` on cancel, decode error, or empty input.
fn decode_track(
    mut resource: Resource,
    buckets: usize,
    cancel: &CancellationToken,
) -> Option<TrackAnalysis> {
    let mut analyzer: Option<WaveformAnalyzer> = None;
    loop {
        if cancel.is_cancelled() {
            debug!("waveform: analysis cancelled");
            return None;
        }
        match resource.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let channels = usize::from(chunk.meta.spec.channels);
                let sample_rate = chunk.meta.spec.sample_rate;
                let analyzer = analyzer.get_or_insert_with(|| {
                    WaveformAnalyzer::new(sample_rate, AnalysisParams::default())
                });
                analyzer.push_interleaved(&chunk.pcm[..], channels);
            }
            Ok(ChunkOutcome::Pending { .. }) => sleep(PENDING_BACKOFF),
            Ok(ChunkOutcome::Eof { .. }) => {
                return analyzer.map(|a| TrackAnalysis {
                    waveform: a.finalize(buckets),
                });
            }
            Err(e) => {
                warn!(?e, "waveform: decode error");
                return None;
            }
        }
    }
}
