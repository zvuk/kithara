use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kithara::{
    abr::AbrHandle,
    events::{AbrMode, AppEvent, EngineEvent, Event, PlayerEvent, TrackId, VariantInfo},
    play::TimestretchControls,
    prelude::{EngineLoadSnapshot, ResourceConfig},
    stream::AudioCodec,
};
use kithara_platform::{CancellationToken, sync::Mutex};
use kithara_queue::{Queue, QueueEvent, RepeatMode, TrackEntry, TrackSource};
use tokio::sync::{Notify, broadcast::error::RecvError, watch};

use crate::{
    config::AppConfig,
    sources::build_resource_config,
    wave_cache::{AnalysisKey, TrackAnalysisCache, source_key},
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};

/// Analysis resolution for the colored waveform. Changing it invalidates
/// cached track-analysis blobs.
const WAVEFORM_BUCKETS: usize = 1500;

/// Snapshot of player state shared between the queue, the listener task,
/// and the UI thread. The struct is cloned cheaply each frame so the UI
/// can render without holding the lock — the only writers are the
/// listener task and direct setter calls from the UI controller.
#[derive(Debug, Clone)]
pub struct UiState {
    pub crossfade_progress: Option<f32>,
    pub current_track_index: Option<usize>,
    pub selected_variant: Option<usize>,
    pub status_note: Option<String>,
    pub track_name: String,
    pub variant_label: String,
    pub abr_variants: Vec<(usize, String)>,
    pub eq_bands: Vec<f32>,
    pub tracks: Vec<TrackEntry>,
    /// Source analysis of the current track; `None` until analysed.
    pub analysis: Option<TrackAnalysis>,
    pub repeat_mode: RepeatMode,
    pub abr_mode_is_auto: bool,
    pub shuffle_enabled: bool,
    pub is_seeking: bool,
    pub playing: bool,
    pub crossfade: f32,
    pub selected_rate: f32,
    pub volume: f32,
    pub duration: f64,
    pub position: f64,
    pub seek_position: f64,
    pub engine_load: EngineLoadSnapshot,
}

impl UiState {
    fn new(queue: &Queue) -> Self {
        let tracks = queue.tracks();
        let current_track_index = tracks.first().map(|_| 0usize);
        let track_name = tracks.first().map(|e| e.name.clone()).unwrap_or_default();

        Self {
            tracks,
            current_track_index,
            track_name,
            variant_label: String::new(),
            abr_variants: Vec::new(),
            abr_mode_is_auto: true,
            selected_variant: None,
            playing: queue.is_playing(),
            shuffle_enabled: queue.is_shuffle_enabled(),
            repeat_mode: queue.repeat_mode(),
            position: queue.position_seconds().unwrap_or(0.0),
            duration: queue.duration_seconds().unwrap_or(0.0),
            volume: queue.volume(),
            crossfade: queue.crossfade_duration(),
            crossfade_progress: None,
            eq_bands: vec![0.0; queue.eq_band_count()],
            analysis: None,
            status_note: None,
            is_seeking: false,
            seek_position: 0.0,
            selected_rate: 1.0,
            engine_load: EngineLoadSnapshot::default(),
        }
    }
}

/// Owns the canonical [`UiState`] and bridges queue events to it.
///
/// All reads from the UI go through [`StateController::snapshot`] which
/// returns a cheap clone — the lock is released before rendering. The
/// listener task is the only background writer; direct setters from the
/// UI thread cover values that the UI commits optimistically (seek
/// scrub, crossfade slider, etc.) before the engine echoes them back.
///
/// Synchronisation uses [`kithara_platform::sync::Mutex`] (a sync
/// `parking_lot` mutex) instead of `tokio::sync::Mutex`. The previous
/// design called `blocking_lock` from inside the iced runtime, which
/// would panic; this one avoids `await`s while the lock is held.
pub struct StateController {
    queue: Arc<Queue>,
    /// Per-deck time-stretch handle.
    timestretch: Arc<TimestretchControls>,
    state: Arc<Mutex<UiState>>,
    cancel: CancellationToken,
    /// Gate for per-track source analysis. `false` keeps the listener
    /// from decoding whole tracks while the DJ Studio is closed.
    waveform_enabled: Arc<AtomicBool>,
    /// Wakes the listener when [`Self::waveform_enabled`] changes so it
    /// can start (or cancel) analysis without waiting for a queue event.
    waveform_wake: Arc<Notify>,
}

impl StateController {
    /// Build a controller and start the listener task that mirrors
    /// queue events into [`UiState`].
    ///
    /// `cancel` must be a child of the app master, so the listener task
    /// stops when the app shuts down.
    /// `config` supplies the shared stores for per-track source analysis.
    /// `timestretch` is the per-deck handle shared with the player.
    pub fn new(
        queue: Arc<Queue>,
        timestretch: Arc<TimestretchControls>,
        config: AppConfig,
        cancel: CancellationToken,
    ) -> Self {
        let state = Arc::new(Mutex::new(UiState::new(&queue)));
        let waveform_enabled = Arc::new(AtomicBool::new(false));
        let waveform_wake = Arc::new(Notify::new());

        spawn_listener(
            Arc::clone(&queue),
            Arc::clone(&state),
            config,
            cancel.clone(),
            Arc::clone(&waveform_enabled),
            Arc::clone(&waveform_wake),
        );

        Self {
            queue,
            timestretch,
            state,
            cancel,
            waveform_enabled,
            waveform_wake,
        }
    }

    /// Enable or disable per-track source analysis.
    pub fn set_waveform_enabled(&self, enabled: bool) {
        self.waveform_enabled.store(enabled, Ordering::SeqCst);
        self.waveform_wake.notify_one();
    }

    /// Apply a closure under the lock. Returns the closure's result.
    /// Used for UI-driven optimistic mutations (seek scrub, crossfade,
    /// abr selection) that must outlive the next event echo.
    pub fn mutate<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut UiState) -> R,
    {
        let mut st = self.state.lock_sync();
        f(&mut st)
    }

    #[must_use]
    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    /// The per-deck time-stretch handle.
    #[must_use]
    pub fn deck(&self) -> &Arc<TimestretchControls> {
        &self.timestretch
    }

    /// Pull the continuous values (position, duration, volume, tracks,
    /// active variant) from the queue. Event-driven mirrors keep the
    /// rest in sync.
    pub fn refresh_continuous(&self) {
        let position = self.queue.position_seconds().unwrap_or(0.0);
        let duration = self.queue.duration_seconds().unwrap_or(0.0);
        let abr = self.queue.current_abr_handle();
        let current_variant = abr.as_ref().and_then(AbrHandle::current_variant);
        let variants = abr.as_ref().map(AbrHandle::variants).unwrap_or_default();
        let mode = abr.as_ref().and_then(AbrHandle::mode);
        let mut st = self.state.lock_sync();
        st.playing = self.queue.is_playing();
        st.shuffle_enabled = self.queue.is_shuffle_enabled();
        st.repeat_mode = self.queue.repeat_mode();
        st.volume = self.queue.volume();
        st.position = position;
        st.duration = duration;
        st.engine_load = self.queue.engine_load();
        if let Some(idx) = self.queue.current_index() {
            st.current_track_index = Some(idx);
        }
        st.variant_label = current_variant
            .as_ref()
            .map(variant_display_label_from_info)
            .unwrap_or_default();
        st.abr_variants = variants
            .iter()
            .map(|v| (v.variant_index.get(), variant_short_label(v)))
            .collect();
        st.abr_mode_is_auto = match mode {
            Some(AbrMode::Manual(_)) => false,
            Some(AbrMode::Auto(_)) | None => true,
        };
    }

    /// Cheap clone of the current state — UI consumers call this once
    /// per frame and render off the snapshot.
    #[must_use]
    pub fn snapshot(&self) -> UiState {
        self.state.lock_sync().clone()
    }
}

impl Drop for StateController {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn spawn_listener(
    queue: Arc<Queue>,
    state: Arc<Mutex<UiState>>,
    config: AppConfig,
    cancel: CancellationToken,
    waveform_enabled: Arc<AtomicBool>,
    waveform_wake: Arc<Notify>,
) {
    let mut rx = queue.subscribe();

    tokio::spawn(async move {
        let dir = config.file_asset_store.root_dir().join("track-analysis");
        let mut analysis = TrackAnalysisRunner::new(&cancel);
        let mut cache = TrackAnalysisCache::new(Some(dir));
        let mut current: Option<Run> = None;
        let mut displayed: Option<AnalysisKey> = None;

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = waveform_wake.notified() => {
                    if waveform_enabled.load(Ordering::SeqCst) {
                        start_analysis(&queue, &state, &config, &mut analysis, &mut current, &mut cache, &mut displayed);
                    } else {
                        analysis.clear();
                        current = None;
                        displayed = None;
                        state.lock_sync().analysis = None;
                    }
                }
                () = wait_result(&mut current) => {
                    commit_analysis(&state, &mut current, &mut cache, &mut displayed);
                }
                event = rx.recv() => match event {
                    Ok(event) => {
                        let track_changed =
                            matches!(event, Event::Queue(QueueEvent::CurrentTrackChanged { .. }));
                        apply_event(event, &queue, &state);
                        if track_changed && waveform_enabled.load(Ordering::SeqCst) {
                            start_analysis(&queue, &state, &config, &mut analysis, &mut current, &mut cache, &mut displayed);
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
            }
        }
    });
}

/// An in-flight analysis: the track it is for (stale-guard), its content cache
/// key (`None` for an unkeyable source), and its result channel.
struct Run {
    track_id: TrackId,
    key: Option<AnalysisKey>,
    rx: watch::Receiver<Option<TrackAnalysis>>,
}

/// Await the current run's result, or pend forever when no run is active so the
/// `select!` branch stays inert.
async fn wait_result(current: &mut Option<Run>) {
    match current {
        Some(run) => {
            let _ = run.rx.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Commit a finished analysis: cache it under its content key (valid for that
/// content regardless of the current track), then publish it if its track is
/// still current. Clears the run either way.
fn commit_analysis(
    state: &Arc<Mutex<UiState>>,
    current: &mut Option<Run>,
    cache: &mut TrackAnalysisCache,
    displayed: &mut Option<AnalysisKey>,
) {
    let Some(run) = current.take() else {
        return;
    };
    let Some(analysis) = run.rx.borrow().clone() else {
        return;
    };

    if let Some(key) = run.key.clone() {
        cache.put(key, analysis.clone());
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
        *displayed = run.key;
    }
}

/// What [`start_analysis`] should do for the current track.
enum Plan {
    /// Already shown or in flight for this content: leave the analysis as is.
    Skip,
    /// Cached (memory or disk): publish it without re-decoding.
    Serve(TrackAnalysis),
    /// Not cached (or an unkeyable source): wipe and analyse.
    Decode,
}

/// Decide the action for a track, guarding against re-decoding content that is
/// already shown, in flight, or cached.
fn plan_analysis(
    key: Option<&AnalysisKey>,
    displayed: Option<&AnalysisKey>,
    in_flight: Option<&AnalysisKey>,
    cache: &mut TrackAnalysisCache,
) -> Plan {
    let Some(key) = key else {
        // No stable key (the reserved non-exhaustive source seam): cannot
        // memoize, so always decode.
        return Plan::Decode;
    };
    if displayed == Some(key) || in_flight == Some(key) {
        return Plan::Skip;
    }
    match cache.get(key) {
        Some(wave) => Plan::Serve(wave),
        None => Plan::Decode,
    }
}

/// Start analysing the current track. Short-circuits when the track is already
/// displayed or in flight, serves cached analysis (memory or disk) without
/// re-decoding, and only wipes + decodes on a genuine cache miss.
fn start_analysis(
    queue: &Arc<Queue>,
    state: &Arc<Mutex<UiState>>,
    config: &AppConfig,
    analysis: &mut TrackAnalysisRunner,
    current: &mut Option<Run>,
    cache: &mut TrackAnalysisCache,
    displayed: &mut Option<AnalysisKey>,
) {
    let track_id = {
        let st = state.lock_sync();
        let Some(idx) = st.current_track_index else {
            return;
        };
        let Some(entry) = st.tracks.get(idx) else {
            return;
        };
        entry.id
    };

    let Some(source) = queue.track_source(track_id) else {
        analysis.clear();
        *current = None;
        return;
    };
    let key = source_key(&source);
    let plan = plan_analysis(
        key.as_ref(),
        displayed.as_ref(),
        current.as_ref().and_then(|run| run.key.as_ref()),
        cache,
    );

    match plan {
        // Already shown or in flight (DJ re-open, or a non-player event): keep.
        Plan::Skip => {}
        // Cached (memory or disk): show it immediately, no wipe, no decode.
        Plan::Serve(analysis_result) => {
            state.lock_sync().analysis = Some(analysis_result);
            *displayed = key;
            *current = None;
            analysis.clear();
        }
        // Genuine miss (or unkeyable source): wipe stale analysis and decode.
        Plan::Decode => {
            let Some(resource_cfg) = resource_config_from_source(source, config) else {
                analysis.clear();
                *current = None;
                return;
            };
            state.lock_sync().analysis = None;
            *displayed = None;
            let rx = analysis.analyze(resource_cfg, WAVEFORM_BUCKETS);
            *current = Some(Run { track_id, key, rx });
        }
    }
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

/// Push the desired EQ gains down to the engine. Calls for bands with no
/// active slot are no-ops; the master EQ persists once a slot accepts them.
fn reapply_eq(queue: &Queue, eq_bands: &[f32]) {
    for (band, &gain) in eq_bands.iter().enumerate() {
        let _ = queue.set_eq_gain(band, gain);
    }
}

fn apply_event(event: Event, queue: &Queue, state: &Mutex<UiState>) {
    match event {
        Event::Queue(QueueEvent::CurrentTrackChanged { .. }) => {
            let current_index = queue.current_index();
            let eq_bands = {
                let mut st = state.lock_sync();
                st.current_track_index = current_index;
                st.track_name = current_index
                    .and_then(|idx| st.tracks.get(idx).map(|t| t.name.clone()))
                    .unwrap_or_default();
                st.selected_variant = None;
                st.is_seeking = false;
                st.eq_bands.clone()
            };
            reapply_eq(queue, &eq_bands);
        }
        Event::Player(PlayerEvent::RateChanged { rate }) => {
            let started = rate > 0.0;
            let mut st = state.lock_sync();
            st.playing = started;
            let eq_bands = started.then(|| st.eq_bands.clone());
            drop(st);
            // Playback just started on an active slot -- push the desired EQ
            // down so gains set before play take effect.
            if let Some(eq_bands) = eq_bands {
                reapply_eq(queue, &eq_bands);
            }
        }
        Event::Player(PlayerEvent::VolumeChanged { volume })
        | Event::Engine(EngineEvent::MasterVolumeChanged { volume }) => {
            let mut st = state.lock_sync();
            st.volume = volume;
        }
        Event::App(AppEvent::Note(note)) => {
            state.lock_sync().status_note = Some(note);
        }
        Event::Queue(
            QueueEvent::TrackAdded { .. }
            | QueueEvent::TrackRemoved { .. }
            | QueueEvent::TrackStatusChanged { .. },
        ) => {
            let tracks = queue.tracks();
            let mut st = state.lock_sync();
            st.tracks = tracks;
            if let Some(idx) = st.current_track_index
                && let Some(track) = st.tracks.get(idx)
            {
                st.track_name = track.name.clone();
            }
        }
        _ => {}
    }
}

fn variant_display_label_from_info(v: &VariantInfo) -> String {
    let bitrate = v.bandwidth_bps.map(|b| format!("{} kbps", b / 1000));
    let codec = v.codecs.as_deref().and_then(codec_label);
    match (bitrate, codec) {
        (Some(b), Some(c)) => format!("{b} \u{00b7} {c}"),
        (Some(b), None) => b,
        (None, Some(c)) => c.to_string(),
        (None, None) => v
            .name
            .clone()
            .unwrap_or_else(|| format!("variant {}", v.variant_index)),
    }
}

/// Human-readable codec family from an HLS `CODECS` attribute value
/// (e.g. `mp4a.40.2` -> `AAC`). Unknown codecs yield `None` so the
/// bitrate is shown without a trailing format tag.
fn codec_label(codecs: &str) -> Option<&'static str> {
    Some(match AudioCodec::parse_hls_codec(codecs)? {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => "AAC",
        AudioCodec::Mp3 => "MP3",
        AudioCodec::Flac => "FLAC",
        AudioCodec::Vorbis => "Vorbis",
        AudioCodec::Opus => "Opus",
        AudioCodec::Alac => "ALAC",
        _ => return None,
    })
}

fn variant_short_label(v: &VariantInfo) -> String {
    v.name.clone().unwrap_or_else(|| {
        v.bandwidth_bps.map_or_else(
            || format!("v{}", v.variant_index),
            |b| format!("{}k", b / 1000),
        )
    })
}

#[cfg(test)]
mod tests {
    use kithara::audio::Waveform;

    use super::{Plan, codec_label, plan_analysis};
    use crate::{
        wave_cache::{AnalysisKey, TrackAnalysisCache},
        waveform::TrackAnalysis,
    };

    #[test]
    fn codec_label_maps_known_hls_codecs() {
        assert_eq!(codec_label("mp4a.40.2"), Some("AAC"));
        assert_eq!(codec_label("mp4a.40.5"), Some("AAC"));
        assert_eq!(codec_label("mp4a.40.34"), Some("MP3"));
        assert_eq!(codec_label("flac"), Some("FLAC"));
        assert_eq!(codec_label("opus"), Some("Opus"));
        assert_eq!(codec_label("av01.0"), None);
    }

    fn one_bucket_wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::from_bytes(&[1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63])
            .expect("hand-built blob is valid")
    }

    fn analysis() -> TrackAnalysis {
        TrackAnalysis {
            waveform: one_bucket_wave(),
        }
    }

    #[test]
    fn plan_skips_shown_or_in_flight_track() {
        let a = AnalysisKey::from_source("file:///a.mp3");
        let mut cache = TrackAnalysisCache::new(None);
        assert!(matches!(
            plan_analysis(Some(&a), Some(&a), None, &mut cache),
            Plan::Skip
        ));
        assert!(matches!(
            plan_analysis(Some(&a), None, Some(&a), &mut cache),
            Plan::Skip
        ));
    }

    #[test]
    fn plan_decodes_a_new_or_unkeyable_track() {
        let a = AnalysisKey::from_source("file:///a.mp3");
        let b = AnalysisKey::from_source("file:///b.mp3");
        let mut cache = TrackAnalysisCache::new(None);
        // A different shown/in-flight track does not block a fresh decode.
        assert!(matches!(
            plan_analysis(Some(&a), Some(&b), Some(&b), &mut cache),
            Plan::Decode
        ));
        // An unkeyable source cannot be memoized.
        assert!(matches!(
            plan_analysis(None, None, None, &mut cache),
            Plan::Decode
        ));
    }

    #[test]
    fn plan_serves_a_cached_track_without_decoding() {
        let a = AnalysisKey::from_source("file:///a.mp3");
        let mut cache = TrackAnalysisCache::new(None);
        cache.put(a.clone(), analysis());
        assert!(matches!(
            plan_analysis(Some(&a), None, None, &mut cache),
            Plan::Serve(_)
        ));
    }
}
