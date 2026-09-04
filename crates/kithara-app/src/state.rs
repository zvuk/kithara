use std::num::NonZeroU32;

#[cfg(test)]
use kithara::analysis::Coverage;
use kithara::{
    abr::AbrHandle,
    analysis::{AnalysisProgress, FrameRange},
    events::{
        AbrMode, BpmInfo, DjEvent, EngineEvent, Envelope, Event, EventReceiver, MediaTime,
        PlayerEvent, SessionEvent, SlotId, TrackId, VariantInfo,
    },
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        time::Duration,
        tokio::{
            self,
            sync::{broadcast::error::RecvError, watch},
            task,
        },
    },
    play::{StretchControls, effects::eq::GainDb},
    prelude::EngineLoadSnapshot,
    queue::{QueueEvent, TrackEntry},
    stream::AudioCodec,
};
use num_traits::{ToPrimitive, cast::AsPrimitive};
use tracing::warn;

use crate::{analysis::AnalysisHandle, pools::AppQueueControl, waveform::TrackAnalysis};

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
    /// Ranges the analysis has not covered, as track fractions in `[0, 1]`,
    /// derived from `analysis.coverage`.
    pub unready_ranges: Arc<[[f32; 2]]>,
    pub engine_load: EngineLoadSnapshot,
    /// Source analysis of the current track; `None` until analysed.
    pub analysis: Option<TrackAnalysis>,
    pub current_track_index: Option<usize>,
    /// Rung the stream is on right now, whichever mode chose it.
    pub current_variant: Option<usize>,
    pub selected_variant: Option<usize>,
    pub track_name: String,
    pub abr_variants: Vec<AbrVariant>,
    pub eq_bands: Vec<GainDb>,
    pub tracks: Vec<TrackEntry>,
    pub abr_mode_is_auto: bool,
    pub is_seeking: bool,
    pub playing: bool,
    pub volume: f32,
    pub duration: f64,
    pub position: f64,
    pub seek_position: f64,
}

impl UiState {
    pub(crate) fn new(queue: &AppQueueControl) -> Self {
        let tracks = queue.tracks();
        let current_track_index = tracks.first().map(|_| 0usize);
        let track_name = tracks.first().map(|e| e.name.clone()).unwrap_or_default();
        let beat_marks = empty_marks();
        let downbeat_marks = empty_marks();

        Self {
            tracks,
            current_track_index,
            track_name,
            abr_variants: Vec::new(),
            abr_mode_is_auto: true,
            selected_variant: None,
            current_variant: None,
            playing: queue.is_playing(),
            position: queue.position_seconds().unwrap_or(0.0),
            duration: queue.duration_seconds().unwrap_or(0.0),
            volume: queue.volume(),
            eq_bands: vec![GainDb::default(); queue.eq_band_count()],
            analysis: None,
            beat_marks,
            downbeat_marks,
            unready_ranges: Arc::default(),
            is_seeking: false,
            seek_position: 0.0,
            engine_load: EngineLoadSnapshot::default(),
        }
    }

    /// Bare default state for unit tests.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        let beat_marks = empty_marks();
        let downbeat_marks = empty_marks();
        Self {
            current_track_index: None,
            selected_variant: None,
            current_variant: None,
            track_name: String::new(),
            abr_variants: Vec::new(),
            eq_bands: Vec::new(),
            tracks: Vec::new(),
            analysis: None,
            beat_marks,
            downbeat_marks,
            unready_ranges: Arc::default(),
            abr_mode_is_auto: true,
            is_seeking: false,
            playing: false,
            volume: 1.0,
            duration: 0.0,
            position: 0.0,
            seek_position: 0.0,
            engine_load: EngineLoadSnapshot::default(),
        }
    }

    /// Set the analysis and re-derive `beat_marks`/`downbeat_marks` from its
    /// beat grid, keeping them in sync with their single source.
    ///
    pub(crate) fn set_analysis(&mut self, analysis: Option<TrackAnalysis>) {
        let (beats, downbeats) = analysis
            .as_ref()
            .and_then(|a| {
                a.beat().filter(|_| a.source_frames() > 0).map(|grid| {
                    (
                        frames_to_fractions(grid.artifact().beats(), a.source_frames()),
                        frames_to_fractions(grid.artifact().downbeats(), a.source_frames()),
                    )
                })
            })
            .unwrap_or_else(|| (empty_marks(), empty_marks()));
        self.beat_marks = beats;
        self.downbeat_marks = downbeats;
        self.unready_ranges = analysis.as_ref().map_or_else(Arc::default, unready_ranges);
        self.analysis = analysis;
    }
}

/// Map one source frame to a track fraction in `[0, 1]`, clamping past the end.
fn fraction(frame: u64, total: f64) -> f32 {
    let frame_f: f64 = frame.as_();
    let frac: f32 = (frame_f / total).clamp(0.0, 1.0).as_();
    frac
}

/// Map source-frame positions to track fractions in `[0, 1]`, clamping
/// out-of-range frames to `1.0`. Empty input or `total == 0` yields empty.
fn frames_to_fractions(frames: &[u64], total: u64) -> Arc<[f32]> {
    if total == 0 {
        return empty_marks();
    }
    let total_f: f64 = total.as_();
    Arc::from_iter(frames.iter().map(|&frame| fraction(frame, total_f)))
}

fn empty_marks() -> Arc<[f32]> {
    Arc::default()
}

/// Map source-frame ranges to track fractions in `[0, 1]`. Empty input or
/// `total == 0` yields empty, and an empty range is dropped.
fn ranges_to_fractions(ranges: &[FrameRange], total: u64) -> Arc<[[f32; 2]]> {
    if ranges.is_empty() || total == 0 {
        return Arc::default();
    }
    let total_f: f64 = total.as_();
    Arc::from_iter(
        ranges
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| {
                [
                    fraction(range.start(), total_f),
                    fraction(range.end(), total_f),
                ]
            }),
    )
}

/// A snapshot covering `runs` of a track `extent` frames long, or of unknown
/// length when `extent` is `None`.
#[cfg(test)]
pub(crate) fn covered(runs: &[(u64, u64)], extent: Option<u64>) -> TrackAnalysis {
    let mut coverage = Coverage::default();
    for &(start, end) in runs {
        coverage.insert(FrameRange::new(start, end - start));
    }
    TrackAnalysis::builder()
        .token("track".into())
        .revision(1)
        .source_sample_rate(NonZeroU32::new(44_100).expect("a positive rate"))
        .maybe_extent(extent)
        .coverage(coverage)
        .build()
}

/// The ranges a snapshot has not covered, as track fractions.
///
/// Empty once the coverage holds the whole extent, and empty while the extent
/// is unknown: with no known length there is no rest of the track to be
/// missing, so a live source claims nothing rather than guessing.
fn unready_ranges(analysis: &TrackAnalysis) -> Arc<[[f32; 2]]> {
    if analysis.extent().is_none() || analysis.is_complete() {
        return Arc::default();
    }
    ranges_to_fractions(&analysis.missing(), analysis.source_frames())
}

/// Owns the canonical [`UiState`] and bridges queue events to it.
///
/// All reads from the UI go through [`StateController::snapshot`] which
/// returns a cheap clone — the lock is released before rendering. The
/// listener task is the only background writer; direct setters from the
/// UI thread cover values that the UI commits optimistically (seek
/// scrub, crossfade slider, etc.) before the engine echoes them back.
///
/// Synchronisation uses [`kithara::platform::sync::Mutex`] (a sync
/// `parking_lot` mutex) instead of `tokio::sync::Mutex`. The previous
/// design called `blocking_lock` from inside the iced runtime, which
/// would panic; this one avoids `await`s while the lock is held.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct StateController {
    beat_clock: Mutex<BeatClockState>,
    #[field(get, deref = false)]
    queue: AppQueueControl,
    state: Arc<Mutex<UiState>>,
    /// Per-deck time-stretch handle.
    #[field(get = stretch, deref = false)]
    timestretch: Arc<StretchControls>,
    cancel: CancelToken,
}

impl StateController {
    /// Build a controller and start the listener task that mirrors
    /// queue events into [`UiState`].
    ///
    /// `cancel` must be a child of the deck master, so the listener task
    /// stops with its deck or the whole app.
    /// `analysis` is the app-wide analysis owner the deck observes.
    /// `timestretch` is the per-deck handle shared with the player.
    pub(crate) fn new(
        queue: AppQueueControl,
        timestretch: Arc<StretchControls>,
        cancel: CancelToken,
        analysis: AnalysisHandle,
    ) -> Self {
        let state = Arc::new(Mutex::new(UiState::new(&queue)));

        let rx = queue.subscribe();
        task::spawn(listen(
            queue.clone(),
            Arc::clone(&state),
            cancel.clone(),
            rx,
            analysis,
        ));

        Self {
            queue,
            state,
            timestretch,
            cancel,
            beat_clock: Mutex::new(BeatClockState::default()),
        }
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
        st.volume = queue.volume();
        st.position = position;
        st.duration = duration;
        st.engine_load = queue.engine_load();
        if let Some(idx) = queue.current_index() {
            st.current_track_index = Some(idx);
        }
        st.current_variant = current_variant
            .as_ref()
            .map(|info| info.variant_index.get());
        st.abr_variants = variants.iter().map(AbrVariant::from).collect();
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
        let Some(beat) = analysis.beat() else {
            return;
        };
        let grid = beat.artifact();
        let source_frames = analysis.source_frames();
        let slot = SlotId::new(1);
        let mut beat_clock = self.beat_clock.lock();
        if beat_clock.published_track != Some(current_index)
            && let Some(info) = bpm_info_from_state(beat, source_frames, state.duration)
        {
            self.queue
                .bus()
                .publish(DjEvent::BpmDetected { slot, info });
            beat_clock.published_track = Some(current_index);
            beat_clock.last_beat_number = None;
        }

        let beats = grid.beats();
        if beats.is_empty() || source_frames == 0 || state.duration <= 0.0 {
            return;
        }

        let max_frame = u64_to_f64(source_frames);
        let current_frame = (((state.position / state.duration) * max_frame).clamp(0.0, max_frame))
            .to_u64()
            .unwrap_or(source_frames);
        let crossed = beats.partition_point(|beat| *beat <= current_frame);
        let latest = crossed.checked_sub(1).and_then(|idx| idx.to_u64());
        let start = beat_clock.last_beat_number.map_or(0, |prev| prev + 1);
        if let Some(latest) = latest {
            for beat_number in start..=latest {
                let beat_idx = usize::try_from(beat_number).unwrap_or(usize::MAX);
                let timestamp =
                    media_time_for_frame(beats[beat_idx], source_frames, state.duration);
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

/// Takes the whole beat artifact, not just its grid: tempo and how sure the
/// detector was are two answers about the same markers, and passing them
/// separately is how they come to disagree.
fn bpm_info_from_state(
    beat: &kithara::analysis::BeatSnapshot,
    source_frames: u64,
    duration_secs: f64,
) -> Option<BpmInfo> {
    let grid = beat.artifact();
    let first_beat = *grid.beats().first()?;
    if source_frames == 0 || duration_secs <= 0.0 {
        return None;
    }
    Some(BpmInfo::new(
        grid.bpm(),
        beat.confidence(),
        Duration::from_secs_f64(
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

/// Mirror queue events into [`UiState`] and the analysis of the track the
/// queue holds into `UiState::analysis`.
pub(crate) async fn listen(
    queue: AppQueueControl,
    state: Arc<Mutex<UiState>>,
    cancel: CancelToken,
    mut rx: EventReceiver,
    analysis: AnalysisHandle,
) {
    let mut held = HeldAnalysis {
        queue: queue.clone(),
        analysis,
        rx: None,
    };
    held.follow(&state).await;
    held.warm(&state).await;

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            open = held.changed() => held.mirror(&state, open),
            event = rx.recv() => match event {
                Ok(Envelope { event, .. }) => {
                    apply_event(&event, &queue, &state);
                    match event {
                        Event::Queue(QueueEvent::CurrentTrackChanged { .. })
                        | Event::Engine(EngineEvent::Started)
                        | Event::Session(SessionEvent::RouteChanged { .. }) => {
                            held.follow(&state).await;
                        }
                        Event::Queue(QueueEvent::TrackAdded { .. } | QueueEvent::TrackRemoved { .. }) => {
                            held.follow(&state).await;
                            held.warm(&state).await;
                        }
                        _ => {}
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    warn!(missed, "queue events lagged; the deck resyncs from its queue");
                    apply_list(&queue, &state);
                    held.follow(&state).await;
                    held.warm(&state).await;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// The deck's view of the analysis owner: a receiver for the track its
/// queue holds.
struct HeldAnalysis {
    queue: AppQueueControl,
    analysis: AnalysisHandle,
    rx: Option<watch::Receiver<Option<AnalysisProgress>>>,
}

impl HeldAnalysis {
    /// Observe the track the deck shows now and mirror what it has.
    async fn follow(&mut self, state: &Mutex<UiState>) {
        let held = {
            let st = state.lock();
            st.current_track_index
                .and_then(|index| st.tracks.get(index).map(|track| track.id))
        };
        let track = held.and_then(|id| self.queue.track_source(id).map(|source| (id, source)));
        self.rx = None;
        self.rx = match (track, self.axis()) {
            (Some((id, source)), Some(axis)) => {
                self.analysis
                    .subscribe(self.queue.clone(), id, source, axis)
                    .await
            }
            _ => None,
        };
        self.mirror(state, true);
    }

    async fn warm(&self, state: &Mutex<UiState>) {
        let ids: Vec<TrackId> = state.lock().tracks.iter().map(|track| track.id).collect();
        if let Some(axis) = self.axis() {
            self.analysis.warm(self.queue.clone(), ids, axis).await;
        }
    }

    fn axis(&self) -> Option<NonZeroU32> {
        let axis = NonZeroU32::new(self.queue.sample_rate());
        if axis.is_none() {
            warn!("analysis: the engine reports no sample rate; the deck observes nothing");
        }
        axis
    }

    /// Resolves on the next revision; `false` once the owner is gone.
    async fn changed(&mut self) -> bool {
        match &mut self.rx {
            Some(rx) => rx.changed().await.is_ok(),
            None => std::future::pending().await,
        }
    }

    /// Show the held revision when it differs from the one on screen. A
    /// receiver whose owner is gone is shown one last time and dropped.
    fn mirror(&mut self, state: &Mutex<UiState>, open: bool) {
        let next = self
            .rx
            .as_ref()
            .and_then(|rx| rx.borrow().as_ref().map(|p| p.analysis().clone()));
        if !open {
            self.rx = None;
        }
        let mut st = state.lock();
        if !same_revision(st.analysis.as_ref(), next.as_ref()) {
            st.set_analysis(next);
        }
    }
}

fn same_revision(shown: Option<&TrackAnalysis>, next: Option<&TrackAnalysis>) -> bool {
    match (shown, next) {
        (None, None) => true,
        (Some(shown), Some(next)) => {
            shown.token() == next.token() && shown.revision() == next.revision()
        }
        _ => false,
    }
}

/// Push the desired EQ gains down to the engine. Calls for bands with no
/// active slot are no-ops; the master EQ persists once a slot accepts them.
fn reapply_eq(queue: &AppQueueControl, eq_bands: &[GainDb]) {
    for (band, &gain) in eq_bands.iter().enumerate() {
        let _ = queue.set_eq_gain(band, f32::from(gain));
    }
}

pub(crate) fn apply_event(event: &Event, queue: &AppQueueControl, state: &Mutex<UiState>) {
    match *event {
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
        // Session-mix gain deliberately has no event mapping here: `st.volume`
        // is content volume, owned by the player's volume path alone.
        Event::Player(PlayerEvent::VolumeChanged { volume }) => {
            let mut st = state.lock();
            st.volume = volume;
        }
        Event::Queue(
            QueueEvent::TrackAdded { .. }
            | QueueEvent::TrackRemoved { .. }
            | QueueEvent::TrackStatusChanged { .. },
        ) => apply_list(queue, state),
        _ => {}
    }
}

/// Mirror the queue's list and keep the shown index naming a track.
fn apply_list(queue: &AppQueueControl, state: &Mutex<UiState>) {
    let tracks = queue.tracks();
    let current = queue.current_index();
    let mut st = state.lock();
    st.tracks = tracks;
    st.current_track_index = shown_index(current, st.current_track_index, st.tracks.len());
    st.track_name = st
        .current_track_index
        .and_then(|idx| st.tracks.get(idx).map(|track| track.name.clone()))
        .unwrap_or_default();
}

/// The track a deck shows: the player's current item; without one, the
/// track it showed while the list still has it, else the first.
fn shown_index(current: Option<usize>, shown: Option<usize>, len: usize) -> Option<usize> {
    current
        .or(shown)
        .filter(|&index| index < len)
        .or_else(|| (len > 0).then_some(0))
}

/// One rung of the ABR ladder as the UI names it: the short label a control
/// shows and the fuller one it explains the rung with.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AbrVariant {
    pub detail: String,
    pub label: String,
    pub index: usize,
}

impl From<&VariantInfo> for AbrVariant {
    fn from(info: &VariantInfo) -> Self {
        Self {
            index: info.variant_index.get(),
            label: variant_short_label(info),
            detail: variant_display_label_from_info(info),
        }
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
    use ::kithara::{
        analysis::{AnalysisProgress, BeatArtifact, BeatSnapshot, BeatState},
        events::PlayerEvent,
        platform::{
            CancelToken,
            sync::{Arc, Mutex},
            time::{self, Duration},
            tokio::{sync::mpsc, task},
        },
        queue::QueueEvent,
    };
    use kithara_test_utils::kithara;

    use super::{
        UiState, bpm_info_from_state, codec_label, covered, frames_to_fractions, listen,
        unready_ranges,
    };
    use crate::{
        analysis::{
            AnalysisHandle, Request,
            fixtures::{answer_subscribe, next_subscribe, queue, track, wait_for_revision},
        },
        pools::AppQueueControl,
        waveform::TrackAnalysis,
    };

    fn progress(revision: u64) -> AnalysisProgress {
        let mut analysis = covered(&[(0, 1_000)], Some(1_000));
        analysis = TrackAnalysis::builder()
            .token(analysis.token().clone())
            .revision(revision)
            .source_sample_rate(analysis.source_sample_rate())
            .maybe_extent(analysis.extent())
            .settled(true)
            .coverage(analysis.coverage().clone())
            .build();
        AnalysisProgress::try_from(analysis).expect("settled fixture is valid progress")
    }

    /// A deck listening to `queue`, with the test playing the analysis owner.
    fn deck(
        queue: &AppQueueControl,
    ) -> (Arc<Mutex<UiState>>, mpsc::Receiver<Request>, CancelToken) {
        let state = Arc::new(Mutex::new(UiState::new(queue)));
        let (analysis, requests) = AnalysisHandle::channel();
        let cancel = CancelToken::root();
        task::spawn(listen(
            queue.clone(),
            Arc::clone(&state),
            cancel.clone(),
            queue.subscribe(),
            analysis,
        ));
        (state, requests, cancel)
    }

    /// A deck built on an empty queue shows the first track that arrives and
    /// observes it before anything plays.
    #[kithara::test(native, tokio, flash(false))]
    async fn a_deck_observes_a_track_added_to_its_empty_queue() {
        let (_host, queue) = queue();
        let (state, mut requests, cancel) = deck(&queue);
        assert_eq!(state.lock().current_track_index, None);

        let (track_id, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let tx = time::timeout(
            Duration::from_secs(2),
            answer_subscribe(&mut requests, track_id),
        )
        .await
        .expect("the deck subscribes for the track its queue gained");

        assert_eq!(state.lock().current_track_index, Some(0));
        assert!(!queue.is_playing());
        tx.send_replace(Some(progress(1)));
        wait_for_revision(&state, 1).await;
        cancel.cancel();
    }

    /// The index a deck shows keeps naming a track across removals, and names
    /// none once the list is empty; the deck lets go of what it observed.
    #[kithara::test(native, tokio, flash(false))]
    async fn a_deck_lets_go_of_a_removed_track() {
        let (_host, queue) = queue();
        let (track_id, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let (state, mut requests, cancel) = deck(&queue);
        let tx = answer_subscribe(&mut requests, track_id).await;
        tx.send_replace(Some(progress(1)));
        wait_for_revision(&state, 1).await;

        queue.remove(track_id).expect("remove test track");

        time::timeout(Duration::from_secs(2), tx.closed())
            .await
            .expect("the deck drops the receiver of a track its queue lost");
        for _ in 0..2_000 {
            if state.lock().analysis.is_none() {
                break;
            }
            task::yield_now().await;
        }
        let st = state.lock();
        assert_eq!(st.current_track_index, None);
        assert!(st.analysis.is_none(), "nothing is shown for no track");
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn a_current_track_change_resubscribes_the_deck_and_mirrors_the_revisions() {
        let (_host, queue) = queue();
        let (track_id, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let (state, mut requests, cancel) = deck(&queue);

        let first = answer_subscribe(&mut requests, track_id).await;
        let Some(Request::Warm { track_ids, .. }) = requests.recv().await else {
            panic!("the deck warms its library");
        };
        assert_eq!(track_ids, vec![track_id]);
        first.send_replace(Some(progress(1)));
        wait_for_revision(&state, 1).await;

        queue
            .bus()
            .publish(QueueEvent::CurrentTrackChanged { id: Some(track_id) });
        let second = answer_subscribe(&mut requests, track_id).await;
        second.send_replace(Some(progress(2)));
        wait_for_revision(&state, 2).await;

        drop(first);
        cancel.cancel();
    }

    /// While the deck asks for its next track it holds no receiver, so the
    /// owner sees the track it left as unheld and can preempt for the new one.
    #[kithara::test(native, tokio, flash(false))]
    async fn a_deck_lets_go_of_its_track_before_asking_for_the_next() {
        let (_host, queue) = queue();
        let (first_id, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let (second_id, _) = track(&queue, 2, "file:///tmp/track-2.mp3");
        let (_state, mut requests, cancel) = deck(&queue);
        let first = answer_subscribe(&mut requests, first_id).await;

        queue.remove(first_id).expect("remove test track");
        let (asked, reply) = time::timeout(Duration::from_secs(2), next_subscribe(&mut requests))
            .await
            .expect("the deck asks for the track it moved to");
        assert_eq!(asked, second_id, "the deck asks for the track it moved to");
        assert!(
            first.is_closed(),
            "and holds no receiver for the one it left while it waits"
        );
        drop(reply);
        cancel.cancel();
    }

    /// A lagged bus is a resync: the deck reads the queue's list and current
    /// track again and observes the track it shows.
    #[kithara::test(native, tokio, flash(false))]
    async fn a_lagged_deck_resyncs_from_its_queue() {
        let (_host, queue) = queue();
        let (first_id, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let (state, mut requests, cancel) = deck(&queue);
        let (_, reply) = next_subscribe(&mut requests).await;
        let (_second_id, _) = track(&queue, 2, "file:///tmp/track-2.mp3");
        for _ in 0..=::kithara::events::DEFAULT_EVENT_BUS_CAPACITY {
            queue
                .bus()
                .publish(PlayerEvent::VolumeChanged { volume: 0.5 });
        }
        let (first, first_rx) = ::kithara::platform::tokio::sync::watch::channel(None);
        assert!(reply.send(first_rx).is_ok(), "the deck waits for the reply");

        let again = time::timeout(
            Duration::from_secs(2),
            answer_subscribe(&mut requests, first_id),
        )
        .await
        .expect("the deck observes its track again after the lag");
        let st = state.lock();
        assert_eq!(st.tracks.len(), 2, "the list is read from the queue");
        assert_eq!(st.current_track_index, Some(0));
        drop(st);
        drop(again);
        drop(first);
        cancel.cancel();
    }

    fn beat(beats: Vec<(u64, Option<f32>)>) -> BeatSnapshot {
        BeatSnapshot::new(
            BeatArtifact::new(120.0, beats, Vec::new()),
            BeatState::Provisional,
            Vec::new(),
        )
    }

    /// The event carries a confidence slot; leaving it empty when the grid
    /// knows the answer is the gap this closes.
    #[kithara::test(native, flash(false))]
    fn a_published_tempo_carries_the_confidence_its_grid_reports() {
        let detected = beat(vec![(0, Some(0.4)), (22_050, Some(0.8))]);
        let info = bpm_info_from_state(&detected, 44_100, 1.0).expect("a grid names a tempo");

        assert!((info.bpm - 120.0).abs() < f64::EPSILON);
        let confidence = info.confidence.expect("detected markers name a confidence");
        assert!(
            (confidence - 0.6).abs() < 1e-6,
            "the published confidence is the grid's own: {confidence}"
        );
    }

    /// A grid built entirely by extrapolation has a tempo but nothing to
    /// stand behind it, and must say so rather than report a zero.
    #[kithara::test(native, flash(false))]
    fn a_tempo_with_nothing_detected_publishes_no_confidence() {
        let guessed = beat(vec![(0, None), (22_050, None)]);
        let info = bpm_info_from_state(&guessed, 44_100, 1.0).expect("a grid names a tempo");

        assert_eq!(info.confidence, None);
    }

    #[kithara::test(native, flash(false))]
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

    /// A snapshot that holds the whole track has nothing left to mark, and
    /// must say so before any range is derived.
    #[kithara::test(native, flash(false))]
    fn a_fully_covered_track_has_no_unready_ranges() {
        let full = covered(&[(0, 1_000)], Some(1_000));

        assert!(unready_ranges(&full).is_empty());
    }

    /// Coverage spread over the track leaves holes between the runs and after
    /// the last one, each reported on the track's own fraction axis.
    #[kithara::test(native, flash(false))]
    fn a_partly_covered_track_names_the_holes_it_left() {
        let partial = covered(&[(0, 200), (400, 600), (800, 900)], Some(1_000));

        let ranges = unready_ranges(&partial);

        assert_eq!(ranges.len(), 3, "{ranges:?}");
        assert_eq!(ranges[0], [0.2, 0.4], "{ranges:?}");
        assert_eq!(ranges[1], [0.6, 0.8], "{ranges:?}");
        assert_eq!(ranges[2], [0.9, 1.0], "{ranges:?}");
    }

    /// Without an extent there is no rest of the track to be missing, so a
    /// live source claims nothing rather than guessing a length.
    #[kithara::test(native, flash(false))]
    fn a_track_of_unknown_length_claims_nothing_unready() {
        let live = covered(&[(0, 200), (400, 600)], None);

        assert!(unready_ranges(&live).is_empty());
    }

    /// A revision carries the whole coverage, so a growing analysis only ever
    /// takes ranges out of the unready set; one coming back would mean a
    /// revision had contradicted an earlier one.
    #[kithara::test(native, flash(false))]
    fn growing_coverage_only_shrinks_the_unready_set() {
        let revisions = [
            &[(0, 200)][..],
            &[(0, 200), (600, 800)][..],
            &[(0, 400), (600, 800)][..],
            &[(0, 1_000)][..],
        ];
        let mut ui = UiState::empty();
        let mut previous: Option<Vec<[f32; 2]>> = None;

        for runs in revisions {
            ui.set_analysis(Some(covered(runs, Some(1_000))));
            let unready = ui.unready_ranges.to_vec();
            if let Some(previous) = previous {
                for range in &unready {
                    assert!(
                        previous
                            .iter()
                            .any(|was| was[0] <= range[0] && range[1] <= was[1]),
                        "{range:?} was ready in {previous:?}"
                    );
                }
            }
            previous = Some(unready);
        }

        assert!(ui.unready_ranges.is_empty(), "the last revision covers all");
    }

    #[kithara::test(native, flash(false))]
    fn codec_label_maps_known_hls_codecs() {
        assert_eq!(codec_label("mp4a.40.2"), Some("AAC"));
        assert_eq!(codec_label("mp4a.40.5"), Some("AAC"));
        assert_eq!(codec_label("mp4a.40.34"), Some("MP3"));
        assert_eq!(codec_label("flac"), Some("FLAC"));
        assert_eq!(codec_label("opus"), Some("Opus"));
        assert_eq!(codec_label("av01.0"), None);
    }
}
