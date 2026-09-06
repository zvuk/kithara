use std::num::NonZeroU32;

pub use kithara::analysis::TrackAnalysis;
use kithara::{
    analysis::{
        AnalysisFileError, AnalysisFingerprint, AnalysisPass, AnalysisProducer, AnalysisProgress,
        AnalysisToken, AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder, BeatAnalysisConfig,
    },
    audio::AudioReader,
    platform::{
        CancelToken,
        sync::Arc,
        tokio::{
            sync::watch,
            task::{self, JoinHandle},
        },
    },
    prelude::{PlaybackResamplerBackend, Resource},
    worker::Worker,
};
use tracing::warn;

use crate::pools::{AppPools, AppResourceConfig, Pools};

type AppBeatAnalysisConfig = BeatAnalysisConfig<PlaybackResamplerBackend>;
type AppAnalyzerBuilder = AnalyzerBuilder<PlaybackResamplerBackend, AppPools>;

#[cfg(feature = "analysis-waveform")]
fn with_waveform(builder: AppAnalyzerBuilder, buckets: usize) -> AppAnalyzerBuilder {
    builder.with_waveform(buckets)
}

#[cfg(not(feature = "analysis-waveform"))]
fn with_waveform(builder: AppAnalyzerBuilder, _buckets: usize) -> AppAnalyzerBuilder {
    builder
}

/// App-side handle over the shared [`AnalysisWorker`]: opens the resource
/// off the player runtime, hands the opened reader to the worker thread,
/// and keeps at most one run in flight. Dropping it cancels the run and
/// stops the worker.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct TrackAnalysisRunner {
    fingerprint: AnalysisFingerprint,
    worker: Arc<AnalysisWorker>,
    current: Option<RunHandle>,
    #[field(get = is_active)]
    active: bool,
}

struct RunHandle {
    cancel: CancelToken,
    task: JoinHandle<()>,
}

impl TrackAnalysisRunner {
    /// `master` must be a child of the app master cancel; the worker thread
    /// and every run scope live under it. `buckets` caps the waveform output;
    /// the native window count is the real resolution.
    #[must_use]
    pub fn new(
        master: &CancelToken,
        base_worker: Option<Worker>,
        chunk_seconds: NonZeroU32,
        buckets: usize,
        beat_config: AppBeatAnalysisConfig,
        pools: Pools,
    ) -> Self {
        let builder = AnalyzerBuilder::new(pools).with_beat_config(beat_config);
        let builder = with_waveform(builder, buckets).with_beat();
        let worker = Arc::new(AnalysisWorker::new(
            AnalysisWorkerConfig::for_builder(builder)
                .cancel(master.clone())
                .chunk_seconds(chunk_seconds)
                .maybe_worker(base_worker)
                .build(),
        ));
        let active = worker.is_active();
        let fingerprint = worker.fingerprint().clone();
        Self {
            fingerprint,
            worker,
            active,
            current: None,
        }
    }

    /// Cancel any prior run and queue `config` for analysis on the `rate`
    /// axis: the reader is opened onto it and the pass is measured in it, so
    /// a producer feeding the same pass later shares one axis with it.
    /// `revision` is the one the caller holds for `token`; every publication
    /// outranks it. Staged results arrive on the returned receiver, which
    /// closes when the run ends; nothing arrives on failure/cancel.
    /// `deliver` receives the producer half synchronously, before the fallback
    /// reader is opened. The runner does not know what the handle is for;
    /// attaching it to the track's playback path is the caller's business.
    pub fn analyze<D>(
        &mut self,
        config: AppResourceConfig,
        token: AnalysisToken,
        rate: NonZeroU32,
        revision: u64,
        deliver: D,
    ) -> watch::Receiver<Option<AnalysisProgress>>
    where
        D: FnOnce(AnalysisProducer),
    {
        self.clear();

        let (rx, producer, pass) = self.worker.open(token, rate, revision);
        let run = pass.cancel_token().clone();
        deliver(producer);
        let task = task::spawn(run_analysis(
            Arc::clone(&self.worker),
            config,
            run.clone(),
            rate,
            pass,
        ));
        self.current = Some(RunHandle { task, cancel: run });
        rx
    }

    /// Cancel the in-flight run.
    pub fn clear(&mut self) {
        if let Some(prev) = self.current.take() {
            prev.cancel.cancel();
            prev.task.abort();
        }
    }

    /// What the active configuration produces, per artifact.
    #[must_use]
    pub const fn fingerprint(&self) -> &AnalysisFingerprint {
        &self.fingerprint
    }

    /// Resume a validated checkpoint, preserving the same synchronous
    /// playback-producer handoff as a fresh pass.
    ///
    /// # Errors
    ///
    /// Returns an archive error when the checkpoint no longer matches the
    /// current analyzer configuration.
    pub fn resume<D>(
        &mut self,
        config: AppResourceConfig,
        progress: AnalysisProgress,
        deliver: D,
    ) -> Result<watch::Receiver<Option<AnalysisProgress>>, AnalysisFileError>
    where
        D: FnOnce(AnalysisProducer),
    {
        self.clear();

        let rate = progress.analysis().source_sample_rate();
        let (rx, producer, pass) = self.worker.open_resume(progress)?;
        let run = pass.cancel_token().clone();
        deliver(producer);
        let task = task::spawn(run_analysis(
            Arc::clone(&self.worker),
            config,
            run.clone(),
            rate,
            pass,
        ));
        self.current = Some(RunHandle { task, cancel: run });
        Ok(rx)
    }
}

impl Drop for TrackAnalysisRunner {
    fn drop(&mut self) {
        self.clear();
    }
}

async fn run_analysis(
    worker: Arc<AnalysisWorker>,
    config: AppResourceConfig,
    cancel: CancelToken,
    rate: NonZeroU32,
    pass: AnalysisPass,
) {
    let Some(reader) = open_reader(config, &cancel, rate).await else {
        return;
    };
    worker.start(pass, reader);
}

async fn open_reader(
    mut config: AppResourceConfig,
    cancel: &CancelToken,
    rate: NonZeroU32,
) -> Option<Box<dyn AudioReader>> {
    if cancel.is_cancelled() {
        return None;
    }
    config.set_cancel(cancel.child());
    config.set_host_sample_rate(rate);
    let mut resource = match Resource::new(config).await {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, "analysis: resource open failed");
            return None;
        }
    };
    if let Err(e) = resource.preload().await {
        warn!(?e, "analysis: preload failed");
        return None;
    }
    Some(resource.into())
}
