use kithara::{
    abr::AbrHandle,
    events::{
        AbrMode, AppEvent, DjEvent, EngineEvent, Event, MediaTime, PlayerEvent, SlotId, VariantInfo,
    },
    play::StretchControls,
    prelude::EngineLoadSnapshot,
    stream::AudioCodec,
};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    tokio::task,
};
use kithara_queue::{Queue, QueueEvent, RepeatMode, TrackEntry};
use num_traits::{ToPrimitive, cast::AsPrimitive};

use crate::{config::AppConfig, waveform::TrackAnalysis};

/// Snapshot of player state shared between the queue, the listener task,
/// and the UI thread. The struct is cloned cheaply each frame so the UI
/// can render without holding the lock — the only writers are the
/// listener task and direct setter calls from the UI controller.
#[derive(Debug, Clone)]
pub struct UiState {
    /// Beat positions as track fractions in `[0, 1]`, derived from `analysis.beat`.
    pub beat_marks: Arc<[f32]>,
    /// Downbeat positions as track fractions in `[0, 1]`, derived from `analysis.beat`.
    pub downbeat_marks: Arc<[f32]>,
    pub engine_load: EngineLoadSnapshot,
    /// Source analysis of the current track; `None` until analysed.
    pub analysis: Option<TrackAnalysis>,
    pub crossfade_progress: Option<f32>,
    pub current_track_index: Option<usize>,
    pub selected_variant: Option<usize>,
    pub status_note: Option<String>,
    pub repeat_mode: RepeatMode,
    pub track_name: String,
    pub variant_label: String,
    pub abr_variants: Vec<(usize, String)>,
    pub eq_bands: Vec<f32>,
    pub tracks: Vec<TrackEntry>,
    pub abr_mode_is_auto: bool,
    pub is_seeking: bool,
    pub playing: bool,
    pub shuffle_enabled: bool,
    pub crossfade: f32,
    pub selected_rate: f32,
    pub volume: f32,
    pub duration: f64,
    pub position: f64,
    pub seek_position: f64,
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
            beat_marks: Arc::from(Vec::new()),
            downbeat_marks: Arc::from(Vec::new()),
            status_note: None,
            is_seeking: false,
            seek_position: 0.0,
            selected_rate: 1.0,
            engine_load: EngineLoadSnapshot::default(),
        }
    }

    /// Bare default state for unit tests.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            crossfade_progress: None,
            current_track_index: None,
            selected_variant: None,
            status_note: None,
            track_name: String::new(),
            variant_label: String::new(),
            abr_variants: Vec::new(),
            eq_bands: Vec::new(),
            tracks: Vec::new(),
            analysis: None,
            beat_marks: Arc::from(Vec::new()),
            downbeat_marks: Arc::from(Vec::new()),
            repeat_mode: RepeatMode::default(),
            abr_mode_is_auto: true,
            shuffle_enabled: false,
            is_seeking: false,
            playing: false,
            crossfade: 0.0,
            selected_rate: 1.0,
            volume: 1.0,
            duration: 0.0,
            position: 0.0,
            seek_position: 0.0,
            engine_load: EngineLoadSnapshot::default(),
        }
    }

    /// Set the analysis and re-derive `beat_marks`/`downbeat_marks` from its
    /// beat grid, keeping them in sync with their single source.
    pub(crate) fn set_analysis(&mut self, analysis: Option<TrackAnalysis>) {
        let (beats, downbeats) = analysis
            .as_ref()
            .and_then(|a| {
                a.beat.as_ref().filter(|_| a.source_frames > 0).map(|grid| {
                    (
                        frames_to_fractions(&grid.beats, a.source_frames),
                        frames_to_fractions(&grid.downbeats, a.source_frames),
                    )
                })
            })
            .unwrap_or_else(|| (Arc::from(Vec::new()), Arc::from(Vec::new())));
        self.beat_marks = beats;
        self.downbeat_marks = downbeats;
        self.analysis = analysis;
    }
}

/// Map source-frame positions to track fractions in `[0, 1]`, clamping
/// out-of-range frames to `1.0`. Empty input or `total == 0` yields empty.
fn frames_to_fractions(frames: &[u64], total: u64) -> Arc<[f32]> {
    if frames.is_empty() || total == 0 {
        return Arc::from(Vec::new());
    }
    let total_f: f64 = total.as_();
    let out: Vec<f32> = frames
        .iter()
        .map(|&frame| {
            let frame_f: f64 = frame.as_();
            let frac: f32 = (frame_f / total_f).clamp(0.0, 1.0).as_();
            frac
        })
        .collect();
    Arc::from(out)
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
    beat_clock: Arc<Mutex<BeatClockState>>,
    queue: Arc<Queue>,
    state: Arc<Mutex<UiState>>,
    /// Per-deck time-stretch handle.
    timestretch: Arc<StretchControls>,
    cancel: CancelToken,
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
        timestretch: Arc<StretchControls>,
        config: AppConfig,
        cancel: CancelToken,
    ) -> Self {
        let state = Arc::new(Mutex::new(UiState::new(&queue)));

        spawn_listener(
            Arc::clone(&queue),
            Arc::clone(&state),
            config,
            cancel.clone(),
        );

        Self {
            beat_clock: Arc::new(Mutex::new(BeatClockState::default())),
            queue,
            state,
            timestretch,
            cancel,
        }
    }

    /// The per-deck time-stretch handle.
    #[must_use]
    pub fn deck(&self) -> &Arc<StretchControls> {
        &self.timestretch
    }

    /// Apply a closure under the lock. Returns the closure's result.
    /// Used for UI-driven optimistic mutations (seek scrub, crossfade,
    /// abr selection) that must outlive the next event echo.
    pub fn mutate<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut UiState) -> R,
    {
        let mut st = self.state.lock();
        f(&mut st)
    }

    #[must_use]
    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    /// Pull the continuous values (position, duration, volume, tracks,
    /// active variant) from the queue. Event-driven mirrors keep the
    /// rest in sync.
    pub fn refresh_continuous(&self) {
        let position = self.queue.position_seconds().unwrap_or(0.0);
        let duration = self.queue.duration_seconds().unwrap_or(0.0);
        let queue = &self.queue;
        let abr = queue.current_abr_handle();
        let current_variant = abr.as_ref().and_then(AbrHandle::current_variant);
        let variants = abr.as_ref().map(AbrHandle::variants).unwrap_or_default();
        let mode = abr.as_ref().and_then(AbrHandle::mode);
        let mut st = self.state.lock();
        st.playing = queue.is_playing();
        st.shuffle_enabled = queue.is_shuffle_enabled();
        st.repeat_mode = queue.repeat_mode();
        st.volume = queue.volume();
        st.position = position;
        st.duration = duration;
        st.engine_load = queue.engine_load();
        if let Some(idx) = queue.current_index() {
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
        let snapshot = st.clone();
        drop(st);
        self.publish_dj_events(&snapshot);
    }

    /// Cheap clone of the current state — UI consumers call this once
    /// per frame and render off the snapshot.
    #[must_use]
    pub fn snapshot(&self) -> UiState {
        self.state.lock().clone()
    }
}

#[derive(Debug, Default)]
struct BeatClockState {
    last_beat_number: Option<u64>,
    published_track: Option<usize>,
}

impl StateController {
    fn publish_dj_events(&self, state: &UiState) {
        let Some(current_index) = state.current_track_index else {
            self.beat_clock.lock().last_beat_number = None;
            return;
        };
        let Some(analysis) = state.analysis.as_ref() else {
            return;
        };
        let Some(grid) = analysis.beat.as_ref() else {
            return;
        };
        let slot = SlotId::new(1);
        let mut beat_clock = self.beat_clock.lock();
        if beat_clock.published_track != Some(current_index)
            && let Some(info) = bpm_info_from_state(grid, analysis.source_frames, state.duration)
        {
            self.queue
                .bus()
                .publish(DjEvent::BpmDetected { slot, info });
            beat_clock.published_track = Some(current_index);
            beat_clock.last_beat_number = None;
        }

        let beats = &grid.beats;
        if beats.is_empty() || analysis.source_frames == 0 || state.duration <= 0.0 {
            return;
        }

        let max_frame = u64_to_f64(analysis.source_frames);
        let current_frame = (((state.position / state.duration) * max_frame).clamp(0.0, max_frame))
            .to_u64()
            .unwrap_or(analysis.source_frames);
        let crossed = beats.partition_point(|beat| *beat <= current_frame);
        let latest = crossed.checked_sub(1).and_then(|idx| idx.to_u64());
        let start = beat_clock.last_beat_number.map_or(0, |prev| prev + 1);
        if let Some(latest) = latest {
            for beat_number in start..=latest {
                let beat_idx = usize::try_from(beat_number).unwrap_or(usize::MAX);
                let timestamp =
                    media_time_for_frame(beats[beat_idx], analysis.source_frames, state.duration);
                self.queue.bus().publish(DjEvent::BeatTick {
                    slot,
                    beat_number,
                    timestamp,
                });
            }
            beat_clock.last_beat_number = Some(latest);
        }
    }
}

fn bpm_info_from_state(
    grid: &kithara::audio::BeatGrid,
    source_frames: u64,
    duration_secs: f64,
) -> Option<kithara::events::BpmInfo> {
    let first_beat = *grid.beats.first()?;
    if source_frames == 0 || duration_secs <= 0.0 {
        return None;
    }
    Some(kithara::events::BpmInfo::new(
        grid.bpm,
        None,
        kithara_platform::time::Duration::from_secs_f64(
            (u64_to_f64(first_beat) / u64_to_f64(source_frames)) * duration_secs,
        ),
    ))
}

fn media_time_for_frame(frame: u64, total_frames: u64, duration_secs: f64) -> MediaTime {
    if total_frames == 0 || duration_secs <= 0.0 {
        return MediaTime::ZERO;
    }
    MediaTime::with_seconds(
        (u64_to_f64(frame) / u64_to_f64(total_frames)) * duration_secs,
        600,
    )
}

/// Saturating conversion for frame counts used in beat-grid ratio math.
fn u64_to_f64(value: u64) -> f64 {
    value.to_f64().unwrap_or(f64::MAX)
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
    cancel: CancelToken,
) {
    let rx = queue.subscribe();
    task::spawn(crate::analysis::listen(queue, state, config, cancel, rx));
}

/// Push the desired EQ gains down to the engine. Calls for bands with no
/// active slot are no-ops; the master EQ persists once a slot accepts them.
fn reapply_eq(queue: &Queue, eq_bands: &[f32]) {
    for (band, &gain) in eq_bands.iter().enumerate() {
        let _ = queue.set_eq_gain(band, gain);
    }
}

pub(crate) fn apply_event(event: Event, queue: &Queue, state: &Mutex<UiState>) {
    match event {
        Event::Queue(QueueEvent::CurrentTrackChanged { .. }) => {
            let current_index = queue.current_index();
            let eq_bands = {
                let mut st = state.lock();
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
            let mut st = state.lock();
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
            let mut st = state.lock();
            st.volume = volume;
        }
        Event::App(AppEvent::Note(note)) => {
            state.lock().status_note = Some(note);
        }
        Event::Queue(
            QueueEvent::TrackAdded { .. }
            | QueueEvent::TrackRemoved { .. }
            | QueueEvent::TrackStatusChanged { .. },
        ) => {
            let tracks = queue.tracks();
            let mut st = state.lock();
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
    use super::{codec_label, frames_to_fractions};

    #[test]
    fn frames_to_fractions_maps_and_clamps() {
        assert!(frames_to_fractions(&[], 100).is_empty(), "empty input");
        assert!(
            frames_to_fractions(&[0, 50, 100], 0).is_empty(),
            "zero total yields empty"
        );

        let got = frames_to_fractions(&[0, 5_000, 10_000], 10_000);
        assert_eq!(got.len(), 3);
        assert!((got[0] - 0.0).abs() < 1e-6, "start at 0.0: {got:?}");
        assert!((got[1] - 0.5).abs() < 1e-6, "midpoint 0.5: {got:?}");
        assert!((got[2] - 1.0).abs() < 1e-6, "end at 1.0: {got:?}");

        // An out-of-range frame clamps to 1.0 and order is preserved.
        let clamped = frames_to_fractions(&[2_000, 50_000], 10_000);
        assert!((clamped[0] - 0.2).abs() < 1e-6, "{clamped:?}");
        assert!(
            (clamped[1] - 1.0).abs() < 1e-6,
            "over-range clamps: {clamped:?}"
        );
        assert!(clamped[0] < clamped[1], "ascending preserved");
    }

    #[test]
    fn codec_label_maps_known_hls_codecs() {
        assert_eq!(codec_label("mp4a.40.2"), Some("AAC"));
        assert_eq!(codec_label("mp4a.40.5"), Some("AAC"));
        assert_eq!(codec_label("mp4a.40.34"), Some("MP3"));
        assert_eq!(codec_label("flac"), Some("FLAC"));
        assert_eq!(codec_label("opus"), Some("Opus"));
        assert_eq!(codec_label("av01.0"), None);
    }
}
