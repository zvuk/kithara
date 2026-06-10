use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use kithara::{
    events::{Event, EventReceiver, TrackId},
    prelude::ResourceConfig,
};
use kithara_platform::{CancellationToken, sync::Mutex};
use kithara_queue::{Queue, QueueEvent, TrackSource};
use tokio::sync::{broadcast::error::RecvError, watch};

use crate::{
    config::AppConfig,
    sources::build_resource_config,
    state::{UiState, apply_event},
    wave_cache::{AnalysisKey, TrackAnalysisCache, source_key},
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};

/// Analysis resolution for the colored waveform. Changing it invalidates
/// cached track-analysis blobs.
const WAVEFORM_BUCKETS: usize = 1500;

/// Analysis-aware state listener: mirrors queue events into [`UiState`] and
/// drives the background [`AnalysisDriver`]. Starts analysing the already
/// loaded library immediately — independent of which UI is open.
pub(crate) async fn listen(
    queue: Arc<Queue>,
    state: Arc<Mutex<UiState>>,
    config: AppConfig,
    cancel: CancellationToken,
    mut rx: EventReceiver,
) {
    let dir = config.file_asset_store.root_dir().join("track-analysis");
    let mut driver = AnalysisDriver::new(&cancel, Some(dir));

    // Analyse whatever is already loaded; later tracks arrive as events.
    driver.on_tracks_changed(&queue, &state, &config);

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = driver.wait_result() => {
                driver.on_result(&queue, &state, &config);
            }
            event = rx.recv() => match event {
                Ok(event) => {
                    let track_changed =
                        matches!(event, Event::Queue(QueueEvent::CurrentTrackChanged { .. }));
                    let tracks_changed = matches!(
                        event,
                        Event::Queue(QueueEvent::TrackAdded { .. } | QueueEvent::TrackRemoved { .. })
                    );
                    apply_event(event, &queue, &state);
                    if tracks_changed {
                        driver.on_tracks_changed(&queue, &state, &config);
                    }
                    if track_changed {
                        driver.on_track_changed(&queue, &state, &config);
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Background source-analysis driver owned by the state listener task.
///
/// Keeps one analysis in flight at a time and a pending queue of library
/// tracks, so every loaded track gets analysed in the background — the
/// current track first — independent of whether any waveform UI is open.
/// Results land in the two-tier [`TrackAnalysisCache`]; the UI snapshot
/// only receives the current track's analysis.
pub(crate) struct AnalysisDriver {
    runner: TrackAnalysisRunner,
    cache: TrackAnalysisCache,
    current: Option<Run>,
    /// Content key of the analysis currently published to the UI.
    displayed: Option<AnalysisKey>,
    /// Tracks waiting for background analysis, current track first.
    pending: VecDeque<TrackId>,
}

/// An in-flight analysis: the track it is for (stale-guard), its content cache
/// key (`None` for an unkeyable source), and its result channel.
struct Run {
    track_id: TrackId,
    key: Option<AnalysisKey>,
    rx: watch::Receiver<Option<TrackAnalysis>>,
}

impl AnalysisDriver {
    /// `cancel` must be a child of the app master so analysis stops on app
    /// shutdown; `cache_dir` is the on-disk tier of the analysis cache.
    pub(crate) fn new(cancel: &CancellationToken, cache_dir: Option<PathBuf>) -> Self {
        Self {
            runner: TrackAnalysisRunner::new(cancel, WAVEFORM_BUCKETS),
            cache: TrackAnalysisCache::new(cache_dir),
            current: None,
            displayed: None,
            pending: VecDeque::new(),
        }
    }

    /// Await the in-flight run's result, or pend forever when no run is
    /// active so the caller's `select!` branch stays inert.
    pub(crate) async fn wait_result(&mut self) {
        match &mut self.current {
            Some(run) => {
                // `changed` errs when the sender dropped without a result
                // (failed/cancelled run); `on_result` then just clears it.
                let _ = run.rx.changed().await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    /// Commit the finished (or failed) run, then start the next pending one.
    pub(crate) fn on_result(
        &mut self,
        queue: &Arc<Queue>,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        self.commit(state);
        self.pump(queue, state, config);
    }

    /// The current track changed: put it at the front of the queue and
    /// preempt an in-flight background run so the visible deck wins.
    pub(crate) fn on_track_changed(
        &mut self,
        queue: &Arc<Queue>,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        if let Some(id) = current_track_id(state) {
            self.pending.retain(|t| *t != id);
            self.pending.push_front(id);
            if let Some(run) = &self.current
                && run.track_id != id
            {
                let preempted = run.track_id;
                self.runner.clear();
                self.current = None;
                self.pending.retain(|t| *t != preempted);
                self.pending.push_back(preempted);
            }
        }
        self.pump(queue, state, config);
    }

    /// Re-sync the pending queue with the library (current track first) and
    /// keep the background pass going. Cached tracks are skipped cheaply.
    pub(crate) fn on_tracks_changed(
        &mut self,
        queue: &Arc<Queue>,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        {
            let st = state.lock_sync();
            let ids: Vec<TrackId> = st.tracks.iter().map(|entry| entry.id).collect();
            self.pending = pending_order(&ids, st.current_track_index);
        }
        if let Some(run) = &self.current {
            self.pending.retain(|t| *t != run.track_id);
        }
        self.pump(queue, state, config);
    }

    /// Start the next analysis worth running, if none is in flight: serve
    /// the current track from cache, skip background tracks that are cached
    /// or unkeyable, decode the first genuine miss.
    pub(crate) fn pump(&mut self, queue: &Arc<Queue>, state: &Mutex<UiState>, config: &AppConfig) {
        if self.current.is_some() {
            return;
        }
        // No analyzers compiled in: decoding would produce nothing.
        if !self.runner.is_active() {
            self.pending.clear();
            return;
        }
        while let Some(track_id) = self.pending.pop_front() {
            // Track gone from the queue since it was enqueued: skip.
            let Some(source) = queue.track_source(track_id) else {
                continue;
            };
            let key = source_key(&source);
            let is_current = current_track_id(state) == Some(track_id);
            match plan_analysis(key.as_ref(), self.displayed.as_ref(), &mut self.cache) {
                Plan::Skip => {}
                Plan::Serve(analysis) => {
                    if is_current {
                        state.lock_sync().analysis = Some(analysis);
                        self.displayed = key;
                    }
                }
                Plan::Decode => {
                    // An unkeyable source cannot be cached, so a background
                    // decode would be thrown away; decode it only for display.
                    if !is_current && key.is_none() {
                        continue;
                    }
                    let Some(cfg) = resource_config_from_source(source, config) else {
                        continue;
                    };
                    if is_current {
                        state.lock_sync().analysis = None;
                        self.displayed = None;
                    }
                    let rx = self.runner.analyze(cfg);
                    self.current = Some(Run { track_id, key, rx });
                    return;
                }
            }
        }
    }

    /// Commit a finished analysis: cache it under its content key (valid for
    /// that content regardless of the current track), then publish it if its
    /// track is still current. Clears the run either way.
    fn commit(&mut self, state: &Mutex<UiState>) {
        let Some(run) = self.current.take() else {
            return;
        };
        let Some(analysis) = run.rx.borrow().clone() else {
            return;
        };

        if let Some(key) = run.key.clone() {
            self.cache.put(key, analysis.clone());
        }

        let mut st = state.lock_sync();
        let still_current = st
            .current_track_index
            .and_then(|i| st.tracks.get(i))
            .map(|entry| entry.id);
        let is_current = still_current == Some(run.track_id);
        if is_current {
            st.analysis = Some(analysis);
        }
        drop(st);
        if is_current {
            self.displayed = run.key;
        }
    }
}

/// What [`AnalysisDriver::pump`] should do for a track.
enum Plan {
    /// Already shown for this content: leave the analysis as is.
    Skip,
    /// Cached (memory or disk): publish it without re-decoding.
    Serve(TrackAnalysis),
    /// Not cached (or an unkeyable source): analyse.
    Decode,
}

/// Decide the action for a track, guarding against re-decoding content that
/// is already shown or cached. An in-flight run needs no guard here: `pump`
/// returns before planning while one is active.
fn plan_analysis(
    key: Option<&AnalysisKey>,
    displayed: Option<&AnalysisKey>,
    cache: &mut TrackAnalysisCache,
) -> Plan {
    let Some(key) = key else {
        // No stable key (the reserved non-exhaustive source seam): cannot
        // memoize, so always decode.
        return Plan::Decode;
    };
    if displayed == Some(key) {
        return Plan::Skip;
    }
    match cache.get(key) {
        Some(wave) => Plan::Serve(wave),
        None => Plan::Decode,
    }
}

/// Library tracks in background-analysis order: the current track first,
/// then the rest in list order.
fn pending_order(ids: &[TrackId], current: Option<usize>) -> VecDeque<TrackId> {
    let mut order = VecDeque::with_capacity(ids.len());
    if let Some(id) = current.and_then(|i| ids.get(i)) {
        order.push_back(*id);
    }
    for (i, id) in ids.iter().enumerate() {
        if current != Some(i) {
            order.push_back(*id);
        }
    }
    order
}

fn current_track_id(state: &Mutex<UiState>) -> Option<TrackId> {
    let st = state.lock_sync();
    st.current_track_index
        .and_then(|i| st.tracks.get(i))
        .map(|entry| entry.id)
}

/// Build an analysis resource from a track's source, reusing the shared
/// stores so the analysis and the player share one download.
fn resource_config_from_source(source: TrackSource, config: &AppConfig) -> Option<ResourceConfig> {
    match source {
        TrackSource::Config(cfg) => Some(*cfg),
        TrackSource::Uri(url) => build_resource_config(&url, config),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use kithara::audio::Waveform;

    use super::{Plan, pending_order, plan_analysis};
    use crate::{
        wave_cache::{AnalysisKey, TrackAnalysisCache},
        waveform::TrackAnalysis,
    };

    fn one_bucket_wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::from_bytes(&[1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63])
            .expect("hand-built blob is valid")
    }

    fn analysis() -> TrackAnalysis {
        let mut analysis = TrackAnalysis::default();
        analysis.waveform = Some(one_bucket_wave());
        analysis
    }

    #[test]
    fn plan_skips_shown_track() {
        let a = AnalysisKey::from_source("file:///a.mp3");
        let mut cache = TrackAnalysisCache::new(None);
        assert!(matches!(
            plan_analysis(Some(&a), Some(&a), &mut cache),
            Plan::Skip
        ));
    }

    #[test]
    fn plan_decodes_a_new_or_unkeyable_track() {
        let a = AnalysisKey::from_source("file:///a.mp3");
        let b = AnalysisKey::from_source("file:///b.mp3");
        let mut cache = TrackAnalysisCache::new(None);
        // A different shown track does not block a fresh decode.
        assert!(matches!(
            plan_analysis(Some(&a), Some(&b), &mut cache),
            Plan::Decode
        ));
        // An unkeyable source cannot be memoized.
        assert!(matches!(
            plan_analysis(None, None, &mut cache),
            Plan::Decode
        ));
    }

    #[test]
    fn plan_serves_a_cached_track_without_decoding() {
        let a = AnalysisKey::from_source("file:///a.mp3");
        let mut cache = TrackAnalysisCache::new(None);
        cache.put(a.clone(), analysis());
        assert!(matches!(
            plan_analysis(Some(&a), None, &mut cache),
            Plan::Serve(_)
        ));
    }

    #[test]
    fn pending_puts_current_track_first_then_list_order() {
        use kithara::events::TrackId;

        let ids: Vec<TrackId> = [10u64, 11, 12].into_iter().map(TrackId::from).collect();

        let order: Vec<u64> = pending_order(&ids, Some(1))
            .into_iter()
            .map(u64::from)
            .collect();
        assert_eq!(order, vec![11, 10, 12], "current first, then list order");

        let order: Vec<u64> = pending_order(&ids, None)
            .into_iter()
            .map(u64::from)
            .collect();
        assert_eq!(order, vec![10, 11, 12], "no current: plain list order");
    }
}
