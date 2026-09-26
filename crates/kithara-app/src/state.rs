use std::{num::NonZeroU32, ops::Range};

use kithara::{
    abr::{AbrHandle, AbrMode, VariantInfo},
    analysis::{BeatGridModel, BeatSnapshot, RawBeatGrid, TrackAnalysis},
    events::{Envelope, EventReceiver, SlotId, TrackId},
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
    play::{BpmInfo, DjEvent, EngineEvent, MediaTime, PlayerEvent, SessionEvent, StretchControls},
    prelude::EngineLoadSnapshot,
    queue::{QueueEvent, TrackEntry},
    stream::AudioCodec,
};
use num_traits::{ToPrimitive, cast::AsPrimitive};
use tracing::warn;

use crate::{
    analysis::{AnalysisEvent, AnalysisHandle, TrackArtifacts},
    pools::AppQueueControl,
};

/// Snapshot of player state shared between the queue, the listener task,
/// and the UI thread. The struct is cloned cheaply each frame so the UI
/// can render without holding the lock — the only writers are the
/// listener task and direct setter calls from the UI controller.
#[derive(Debug, Clone)]
pub struct UiState {
    pub beat_marks: Arc<[f32]>,
    pub downbeat_marks: Arc<[f32]>,
    pub unready_ranges: Arc<[[f32; 2]]>,
    pub engine_load: EngineLoadSnapshot,
    pub current_track_index: Option<usize>,
    pub current_variant: Option<usize>,
    pub abr_mode: Option<AbrMode>,
    pub track_name: String,
    pub abr_variants: Vec<AbrVariant>,
    pub tracks: Vec<TrackEntry>,
    pub playing: bool,
    pub volume: f32,
    pub duration: f64,
    pub position: f64,
    pub(crate) analysis: Option<TrackArtifacts>,
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
            beat_marks,
            downbeat_marks,
            abr_variants: Vec::new(),
            abr_mode: None,
            current_variant: None,
            playing: queue.is_playing(),
            position: queue.position_seconds().unwrap_or(0.0),
            duration: queue.duration_seconds().unwrap_or(0.0),
            volume: queue.volume(),
            analysis: None,
            unready_ranges: Arc::default(),
            engine_load: EngineLoadSnapshot::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        let beat_marks = empty_marks();
        let downbeat_marks = empty_marks();
        Self {
            beat_marks,
            downbeat_marks,
            current_track_index: None,
            current_variant: None,
            abr_mode: None,
            track_name: String::new(),
            abr_variants: Vec::new(),
            tracks: Vec::new(),
            analysis: None,
            unready_ranges: Arc::default(),
            playing: false,
            volume: 1.0,
            duration: 0.0,
            position: 0.0,
            engine_load: EngineLoadSnapshot::default(),
        }
    }

    /// Where the published grid puts its beats, as fractions of the track.
    ///
    /// The grid is read the way it states itself — in media seconds — so a
    /// grid the track was opened with paints exactly like one this build
    /// analysed, and neither is read against the host's output rate. A
    /// publication that carries no grid at all falls back to the pass's own
    /// frame positions, which is all a pass mid-flight has stated yet.
    fn marks(&self, artifacts: &TrackArtifacts) -> Option<Marks> {
        if let Some(grid) = artifacts.grid() {
            let raw = grid.as_raw();
            let seconds = raw.duration.unwrap_or(self.duration);
            return Some(Marks {
                beats: seconds_to_fractions(raw.beats.iter().map(|beat| beat.at), seconds),
                downbeats: seconds_to_fractions(raw.downbeats.iter().map(|beat| beat.at), seconds),
            });
        }
        let analysis = artifacts.analysis()?;
        let frames = analysis.source_frames();
        analysis.beat().filter(|_| frames > 0).map(|grid| Marks {
            beats: frames_to_fractions(grid.artifact().beats(), frames),
            downbeats: frames_to_fractions(grid.artifact().downbeats(), frames),
        })
    }

    pub(crate) fn set_analysis(&mut self, analysis: Option<TrackArtifacts>) {
        let marks = analysis
            .as_ref()
            .and_then(|artifacts| self.marks(artifacts))
            .unwrap_or_default();
        self.beat_marks = marks.beats;
        self.downbeat_marks = marks.downbeats;
        self.unready_ranges = analysis
            .as_ref()
            .and_then(TrackArtifacts::analysis)
            .map_or_else(Arc::default, unready_ranges);
        self.analysis = analysis;
    }
}

fn fraction(frame: u64, total: f64) -> f32 {
    let frame_f: f64 = frame.as_();
    let frac: f32 = (frame_f / total).clamp(0.0, 1.0).as_();
    frac
}

fn frames_to_fractions(frames: &[u64], total: u64) -> Arc<[f32]> {
    if total == 0 {
        return empty_marks();
    }
    let total_f: f64 = total.as_();
    Arc::from_iter(frames.iter().map(|&frame| fraction(frame, total_f)))
}

/// Where a deck paints its beat and bar lines, as fractions of the track.
#[derive(Default)]
struct Marks {
    beats: Arc<[f32]>,
    downbeats: Arc<[f32]>,
}

/// Media-second positions as fractions of a track that runs `total` seconds.
/// A track of unknown length has no fraction to place anything at.
fn seconds_to_fractions<I: Iterator<Item = f64>>(seconds: I, total: f64) -> Arc<[f32]> {
    if !(total.is_finite() && total > 0.0) {
        return empty_marks();
    }
    Arc::from_iter(seconds.map(|at| {
        let fraction: f32 = (at / total).clamp(0.0, 1.0).as_();
        fraction
    }))
}

fn empty_marks() -> Arc<[f32]> {
    Arc::default()
}

fn ranges_to_fractions(ranges: &[Range<u64>], total: u64) -> Arc<[[f32; 2]]> {
    if ranges.is_empty() || total == 0 {
        return Arc::default();
    }
    let total_f: f64 = total.as_();
    Arc::from_iter(
        ranges
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| [fraction(range.start, total_f), fraction(range.end, total_f)]),
    )
}

#[cfg(test)]
use kithara::analysis::RangeSet;

mod consts {
    /// Timescale the deck stamps a beat position with: the grid states seconds,
    /// and the DJ surface carries them as a rational media time.
    pub(super) const MEDIA_TIMESCALE: i32 = 600;
}

#[cfg(test)]
pub(crate) fn covered(runs: &[(u64, u64)], extent: Option<u64>) -> TrackAnalysis {
    let mut coverage = RangeSet::new();
    for &(start, end) in runs {
        coverage.insert(start..end);
    }
    TrackAnalysis::builder()
        .token("track".into())
        .revision(1)
        .source_sample_rate(NonZeroU32::new(44_100).expect("a positive rate"))
        .maybe_extent(extent)
        .coverage(coverage)
        .build()
}

fn unready_ranges(analysis: &TrackAnalysis) -> Arc<[[f32; 2]]> {
    if analysis.extent().is_none() || analysis.is_complete() {
        return Arc::default();
    }
    ranges_to_fractions(&analysis.missing(), analysis.source_frames())
}

/// Owns the canonical [`UiState`] and bridges queue events to it. The
/// listener task is the only background writer; the UI thread commits
/// optimistic values through [`StateController::mutate`].
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct StateController {
    #[field(get, deref = false)]
    queue: AppQueueControl,
    state: Arc<Mutex<UiState>>,
    #[field(get = stretch, deref = false)]
    timestretch: Arc<StretchControls>,
    cancel: CancelToken,
    beat_clock: Mutex<BeatClockState>,
}

impl StateController {
    /// `cancel` must be a child of the deck master: the listener stops with its
    /// deck or with the app.
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
    pub fn mutate<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut UiState) -> R,
    {
        let mut st = self.state.lock();
        f(&mut st)
    }

    /// Reads the state under the lock.
    pub fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&UiState) -> R,
    {
        f(&self.state.lock())
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
        st.abr_mode = mode;
        let snapshot = st.clone();
        drop(st);
        self.publish_dj_events(&snapshot);
    }
}

#[cfg(test)]
pub(crate) mod test_fixture {
    use super::*;

    /// A controller over a state someone else writes into - the listener task,
    /// as the running deck has it.
    pub(crate) fn controller_on(
        queue: AppQueueControl,
        timestretch: Arc<StretchControls>,
        cancel: CancelToken,
        state: Arc<Mutex<UiState>>,
    ) -> StateController {
        StateController {
            queue,
            state,
            timestretch,
            cancel,
            beat_clock: Mutex::new(BeatClockState::default()),
        }
    }
}

#[derive(Debug, Default)]
struct BeatClockState {
    /// The ordinal of the last beat announced, as the grid names it.
    last_beat_number: Option<i64>,
    published_track: Option<usize>,
}

impl StateController {
    /// The grid numbers each beat by its ordinal, so a marker the pass could not place leaves a gap
    /// in the numbering rather than renumbering its neighbours.
    fn publish_dj_events(&self, state: &UiState) {
        let Some(current_index) = state.current_track_index else {
            self.beat_clock.lock().last_beat_number = None;
            return;
        };
        let Some(analysis) = state.analysis.as_ref() else {
            return;
        };
        let Some(grid) = analysis.grid().map(BeatGridModel::as_raw) else {
            return;
        };
        let slot = SlotId::new(1);
        let mut beat_clock = self.beat_clock.lock();
        if beat_clock.published_track != Some(current_index)
            && let Some(info) = bpm_info_from_grid(
                grid,
                analysis
                    .analysis()
                    .and_then(TrackAnalysis::beat)
                    .and_then(BeatSnapshot::confidence),
            )
        {
            self.queue
                .bus()
                .publish(DjEvent::BpmDetected { slot, info });
            beat_clock.published_track = Some(current_index);
            beat_clock.last_beat_number = None;
        }

        if grid.beats.is_empty() {
            return;
        }

        let crossed = grid.beats.partition_point(|beat| beat.at <= state.position);
        let last = beat_clock.last_beat_number;
        for beat in grid.beats[..crossed]
            .iter()
            .filter(|beat| last.is_none_or(|prev| beat.ordinal > prev))
        {
            let Some(beat_number) = beat.ordinal.to_u64() else {
                continue;
            };
            self.queue.bus().publish(DjEvent::BeatTick {
                slot,
                beat_number,
                timestamp: MediaTime::with_seconds(beat.at, consts::MEDIA_TIMESCALE),
            });
            beat_clock.last_beat_number = Some(beat.ordinal);
        }
    }
}

/// The tempo the deck announces: read from the published grid, whose times are
/// already media seconds on the source timeline, so the announcement does not
/// depend on what the engine currently reports as the track length.
fn bpm_info_from_grid(grid: &RawBeatGrid, confidence: Option<f32>) -> Option<BpmInfo> {
    let first_beat = grid.beats.first()?;
    Some(BpmInfo::new(
        grid.bpm,
        confidence,
        Duration::from_secs_f64(first_beat.at),
    ))
}

impl Drop for StateController {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) async fn listen(
    queue: AppQueueControl,
    state: Arc<Mutex<UiState>>,
    cancel: CancelToken,
    mut rx: EventReceiver<AnalysisEvent>,
    analysis: AnalysisHandle,
) {
    let mut held = HeldAnalysis {
        analysis,
        queue: queue.clone(),
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
                        AnalysisEvent::Queue(QueueEvent::CurrentTrackChanged { .. })
                        | AnalysisEvent::Engine(EngineEvent::Started)
                        | AnalysisEvent::Session(SessionEvent::RouteChanged { .. }) => {
                            held.follow(&state).await;
                        }
                        AnalysisEvent::Queue(QueueEvent::TrackAdded { .. } | QueueEvent::TrackRemoved { .. }) => {
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

struct HeldAnalysis {
    analysis: AnalysisHandle,
    queue: AppQueueControl,
    rx: Option<watch::Receiver<Option<TrackArtifacts>>>,
}

impl HeldAnalysis {
    fn axis(&self) -> Option<NonZeroU32> {
        let axis = NonZeroU32::new(self.queue.sample_rate());
        if axis.is_none() {
            warn!("analysis: the engine reports no sample rate; the deck observes nothing");
        }
        axis
    }

    async fn changed(&mut self) -> bool {
        match &mut self.rx {
            Some(rx) => rx.changed().await.is_ok(),
            None => std::future::pending().await,
        }
    }

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

    fn mirror(&mut self, state: &Mutex<UiState>, open: bool) {
        let next = self.rx.as_ref().and_then(|rx| rx.borrow().clone());
        if !open {
            self.rx = None;
        }
        let mut st = state.lock();
        if !same_revision(st.analysis.as_ref(), next.as_ref()) {
            st.set_analysis(next);
        }
    }

    async fn warm(&self, state: &Mutex<UiState>) {
        let ids: Vec<TrackId> = state.lock().tracks.iter().map(|track| track.id).collect();
        if let Some(axis) = self.axis() {
            self.analysis.warm(self.queue.clone(), ids, axis).await;
        }
    }
}

fn same_revision(shown: Option<&TrackArtifacts>, next: Option<&TrackArtifacts>) -> bool {
    match (shown, next) {
        (None, None) => true,
        (Some(shown), Some(next)) => {
            let identity = |artifacts: &TrackArtifacts| {
                artifacts
                    .analysis()
                    .map(|analysis| (analysis.token().clone(), analysis.revision()))
            };
            identity(shown) == identity(next)
        }
        _ => false,
    }
}

/// Session-mix gain deliberately has no event mapping here: `st.volume` is content volume, owned
/// solely by the player's volume path.
pub(crate) fn apply_event(event: &AnalysisEvent, queue: &AppQueueControl, state: &Mutex<UiState>) {
    match *event {
        AnalysisEvent::Queue(QueueEvent::CurrentTrackChanged { .. }) => {
            let current_index = queue.current_index();
            let mut st = state.lock();
            st.current_track_index = current_index;
            st.track_name = current_index
                .and_then(|idx| st.tracks.get(idx).map(|t| t.name.clone()))
                .unwrap_or_default();
        }
        AnalysisEvent::Player(PlayerEvent::RateChanged { rate }) => {
            state.lock().playing = rate > 0.0;
        }
        AnalysisEvent::Player(PlayerEvent::VolumeChanged { volume }) => {
            let mut st = state.lock();
            st.volume = volume;
        }
        AnalysisEvent::Queue(
            QueueEvent::TrackAdded { .. }
            | QueueEvent::TrackRemoved { .. }
            | QueueEvent::TrackStatusChanged { .. },
        ) => apply_list(queue, state),
        _ => {}
    }
}

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

fn shown_index(current: Option<usize>, shown: Option<usize>, len: usize) -> Option<usize> {
    current
        .or(shown)
        .filter(|&index| index < len)
        .or_else(|| (len > 0).then_some(0))
}

/// One rung of the ABR ladder as the UI names it: the short label a control
/// shows and the fuller one it explains the rung with.
#[derive(Debug, Clone, PartialEq)]
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
        abr::AbrMode,
        analysis::{BeatArtifact, BeatGridState, BeatSnapshot, BeatState},
        platform::{
            CancelToken,
            sync::{Arc, Mutex},
            time::{self, Duration},
            tokio::{sync::mpsc, task},
        },
        play::{DjEvent, PlayerEvent},
        queue::QueueEvent,
    };
    use kithara_test_utils::kithara;

    use super::{
        AnalysisEvent, BpmInfo, EngineEvent, Envelope, EventReceiver, MediaTime, NonZeroU32,
        RangeSet, StretchControls, UiState, bpm_info_from_grid, codec_label,
        consts::MEDIA_TIMESCALE, covered, frames_to_fractions, listen, unready_ranges,
    };
    use crate::{
        analysis::{
            AnalysisHandle, Request, TrackArtifacts,
            fixtures::{
                answer_subscribe, next_subscribe, queue_off, tone_mp3, track, wait_for_revision,
            },
        },
        pools::AppQueueControl,
        state::test_fixture::controller_on,
        waveform::TrackAnalysis,
    };

    fn progress(revision: u64) -> TrackArtifacts {
        let mut analysis = covered(&[(0, 1_000)], Some(1_000));
        analysis = TrackAnalysis::builder()
            .token(analysis.token().clone())
            .revision(revision)
            .source_sample_rate(analysis.source_sample_rate())
            .maybe_extent(analysis.extent())
            .settled(true)
            .coverage(analysis.coverage().clone())
            .build();
        analysis.into()
    }

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

    #[kithara::test(native, tokio, flash(false))]
    async fn a_deck_observes_a_track_added_to_its_empty_queue() {
        let (host, queue) = queue_off().await;
        let (state, mut requests, cancel) = deck(&queue);
        assert_eq!(state.lock().current_track_index, None);

        let (track_id, _) = track(&host, 1, "file:///tmp/track-1.mp3").await;
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
        host.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_deck_lets_go_of_a_removed_track() {
        let (host, queue) = queue_off().await;
        let (track_id, _) = track(&host, 1, "file:///tmp/track-1.mp3").await;
        let (state, mut requests, cancel) = deck(&queue);
        let tx = answer_subscribe(&mut requests, track_id).await;
        tx.send_replace(Some(progress(1)));
        wait_for_revision(&state, 1).await;

        queue.remove(track_id).expect("remove test track");

        for _ in 0..2_000 {
            if tx.receiver_count() == 0 && state.lock().analysis.is_none() {
                break;
            }
            task::yield_now().await;
        }
        assert_eq!(
            tx.receiver_count(),
            0,
            "the deck drops the receiver of a track its queue lost"
        );
        let st = state.lock();
        assert_eq!(st.current_track_index, None);
        assert!(st.analysis.is_none(), "nothing is shown for no track");
        drop(st);
        cancel.cancel();
        host.close().await;
    }

    #[kithara::test(native, tokio)]
    async fn a_current_track_change_resubscribes_the_deck_and_mirrors_the_revisions() {
        let (host, queue) = queue_off().await;
        let (track_id, _) = track(&host, 1, "file:///tmp/track-1.mp3").await;
        let (state, mut requests, cancel) = deck(&queue);

        let first = answer_subscribe(&mut requests, track_id).await;
        let Some(Request::Warm { track_ids, .. }) = requests.recv().await else {
            panic!("the deck warms its library");
        };
        assert_eq!(track_ids, vec![track_id]);
        first.send_replace(Some(progress(1)));
        wait_for_revision(&state, 1).await;

        host.call(|(_, queue)| queue.set_eq_gain(0, -6.0).expect("set the deck EQ"))
            .await;
        state.lock().abr_mode = Some(AbrMode::manual(1));
        queue.bus().publish(PlayerEvent::RateChanged { rate: 1.0 });
        queue
            .bus()
            .publish(QueueEvent::CurrentTrackChanged { id: Some(track_id) });
        let second = answer_subscribe(&mut requests, track_id).await;
        second.send_replace(Some(progress(2)));
        wait_for_revision(&state, 2).await;
        assert_eq!(
            queue.eq_gain(0),
            Some(-6.0),
            "event mirrors preserve the deck EQ"
        );
        assert!(state.lock().playing, "the rate event reaches the UI");

        controller_on(
            queue.clone(),
            StretchControls::new(1.0),
            cancel.child(),
            Arc::clone(&state),
        )
        .refresh_continuous();
        assert_eq!(
            state.lock().abr_mode,
            None,
            "a new track shows the mode of its own ladder"
        );
        drop(first);
        cancel.cancel();
        host.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_deck_lets_go_of_its_track_before_asking_for_the_next() {
        let (host, queue) = queue_off().await;
        let (first_id, _) = track(&host, 1, "file:///tmp/track-1.mp3").await;
        let (second_id, _) = track(&host, 2, "file:///tmp/track-2.mp3").await;
        let (_state, mut requests, cancel) = deck(&queue);
        let first = answer_subscribe(&mut requests, first_id).await;

        queue.remove(first_id).expect("remove test track");
        let (asked, reply) = time::timeout(Duration::from_secs(2), next_subscribe(&mut requests))
            .await
            .expect("the deck asks for the track it moved to");
        assert_eq!(asked, second_id, "the deck asks for the track it moved to");
        assert_eq!(
            first.receiver_count(),
            0,
            "and holds no receiver for the one it left while it waits"
        );
        drop(reply);
        cancel.cancel();
        host.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_lagged_deck_resyncs_from_its_queue() {
        let (host, queue) = queue_off().await;
        let (first_id, _) = track(&host, 1, "file:///tmp/track-1.mp3").await;
        let (state, mut requests, cancel) = deck(&queue);
        let (_, reply) = next_subscribe(&mut requests).await;
        let (_second_id, _) = track(&host, 2, "file:///tmp/track-2.mp3").await;
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
        host.close().await;
    }

    fn analysed(beats: Vec<(u64, Option<f32>)>) -> TrackAnalysis {
        TrackAnalysis::builder()
            .token("track".into())
            .revision(1)
            .source_sample_rate(NonZeroU32::new(44_100).expect("a positive rate"))
            .extent(44_100)
            .beat(BeatSnapshot::new(
                BeatArtifact::new(120.0, beats, Vec::new()),
                BeatState::Provisional,
                Vec::new(),
            ))
            .build()
    }

    fn published(analysis: &TrackAnalysis) -> BpmInfo {
        let grid = analysis.grid().expect("the publication states a grid");
        bpm_info_from_grid(
            grid.as_raw(),
            analysis.beat().and_then(BeatSnapshot::confidence),
        )
        .expect("a grid names a tempo")
    }

    #[kithara::test(native, flash(false))]
    fn a_published_tempo_carries_the_confidence_its_grid_reports() {
        let info = published(&analysed(vec![(0, Some(0.4)), (22_050, Some(0.8))]));

        assert!((info.bpm - 120.0).abs() < f64::EPSILON);
        let confidence = info.confidence.expect("detected markers name a confidence");
        assert!(
            (confidence - 0.6).abs() < 1e-6,
            "the published confidence is the grid's own: {confidence}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_tempo_with_nothing_detected_publishes_no_confidence() {
        let info = published(&analysed(vec![(0, None), (22_050, None)]));

        assert_eq!(
            info.confidence, None,
            "an extrapolated grid names no confidence rather than a zero"
        );
    }

    /// The deck announces the first beat where the grid puts it on the media
    /// timeline, not where a proportion of whatever length the engine reports
    /// would land.
    #[kithara::test(native, flash(false))]
    fn a_published_tempo_names_the_first_beat_in_media_seconds() {
        let info = published(&analysed(vec![(22_050, Some(0.9)), (44_100, Some(0.9))]));

        assert!(
            (info.first_beat_offset.as_secs_f64() - 0.5).abs() < 1e-9,
            "the first beat sits half a second in: {:?}",
            info.first_beat_offset
        );
    }

    fn publication(revision: u64, state: BeatState, beats: &[u64]) -> TrackAnalysis {
        let mut coverage = RangeSet::new();
        coverage.insert(0..220_500);
        TrackAnalysis::builder()
            .token("deck-track".into())
            .revision(revision)
            .source_sample_rate(NonZeroU32::new(44_100).expect("a positive rate"))
            .extent(220_500)
            // Settled as a publication either way: what a revision changes for
            // the deck is the state of the grid the pass states, not whether
            // the pass has more source to read.
            .settled(true)
            .coverage(coverage)
            .beat(BeatSnapshot::new(
                BeatArtifact::new(
                    120.0,
                    beats.iter().map(|&frame| (frame, Some(0.9))).collect(),
                    Vec::new(),
                ),
                state,
                Vec::new(),
            ))
            .build()
    }

    fn ticks(events: &mut EventReceiver<DjEvent>) -> Vec<MediaTime> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|envelope| match envelope.event {
                DjEvent::BeatTick { timestamp, .. } => Some(timestamp),
                _ => None,
            })
            .collect()
    }

    fn stamps(beats: &[u64]) -> Vec<MediaTime> {
        beats
            .iter()
            .map(|&frame| {
                MediaTime::with_seconds(
                    f64::from(u32::try_from(frame).unwrap_or(0)) / 44_100.0,
                    MEDIA_TIMESCALE,
                )
            })
            .collect()
    }

    /// End to end over the deck's own plumbing: what a pass publishes reaches
    /// the listener, the listener puts it in the deck's state, and the deck
    /// announces the tempo and the beats from the grid that publication
    /// states - including the revision that follows the first.
    #[kithara::test(native, tokio, flash(false))]
    async fn the_deck_follows_the_grid_each_publication_states(tone_mp3: String) {
        let (host, queue) = queue_off().await;
        let mut loading: EventReceiver<AnalysisEvent> = queue.subscribe();
        let (track_id, _source) = track(&host, 1, &tone_mp3).await;
        // The tone loads for real and starts the engine, which the deck follows
        // with a fresh subscription; the deck starts on the loaded track so the
        // pass it subscribes to is the one publishing both revisions.
        loop {
            match loading.recv().await {
                Ok(Envelope {
                    event: AnalysisEvent::Engine(EngineEvent::Started),
                    ..
                }) => break,
                Ok(_) => {}
                Err(error) => panic!("the queue loads the tone: {error}"),
            }
        }
        let (state, mut requests, cancel) = deck(&queue);
        let tx = answer_subscribe(&mut requests, track_id).await;
        let controller = controller_on(
            queue.clone(),
            StretchControls::new(1.0),
            cancel.child(),
            Arc::clone(&state),
        );
        let mut events = queue.bus().subscribe::<DjEvent>();

        let first = [0, 22_050, 44_100];
        tx.send(Some(publication(1, BeatState::Provisional, &first).into()))
            .expect("the pass publishes");
        wait_for_revision(&state, 1).await;
        controller.mutate(|st| st.position = 1.0);
        controller.publish_dj_events(&controller.read(UiState::clone));

        let announced = match events.try_recv().expect("the deck announces a tempo").event {
            DjEvent::BpmDetected { info, .. } => info,
            other => panic!("the deck announces the tempo first, not {other:?}"),
        };
        let grid_bpm = state
            .lock()
            .analysis
            .as_ref()
            .and_then(TrackArtifacts::grid)
            .expect("the publication states a grid")
            .as_raw()
            .bpm;
        assert!(
            (announced.bpm - grid_bpm).abs() < f64::EPSILON,
            "the announced tempo is the published grid's own: {announced:?}"
        );
        assert_eq!(
            ticks(&mut events),
            stamps(&first),
            "every tick stands where the grid puts its beat, in media seconds"
        );

        let then = [0, 22_050, 44_100, 66_150, 88_200];
        tx.send(Some(publication(2, BeatState::Final, &then).into()))
            .expect("the pass publishes again");
        wait_for_revision(&state, 2).await;
        controller.mutate(|st| st.position = 2.0);
        controller.publish_dj_events(&controller.read(UiState::clone));

        assert_eq!(
            ticks(&mut events),
            stamps(&then[3..]),
            "the deck follows the later revision's grid without repeating itself"
        );
        assert_eq!(
            state
                .lock()
                .analysis
                .as_ref()
                .and_then(TrackArtifacts::grid)
                .expect("the later publication states a grid")
                .as_raw()
                .state,
            BeatGridState::Final,
            "the settled publication states a grid nothing will revise"
        );
        cancel.cancel();
        host.close().await;
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

    #[kithara::test(native, flash(false))]
    fn a_fully_covered_track_has_no_unready_ranges() {
        let full = covered(&[(0, 1_000)], Some(1_000));

        assert!(unready_ranges(&full).is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn a_partly_covered_track_names_the_holes_it_left() {
        let partial = covered(&[(0, 200), (400, 600), (800, 900)], Some(1_000));

        let ranges = unready_ranges(&partial);

        assert_eq!(
            ranges.len(),
            3,
            "a hole sits between each pair of runs and after the last one: {ranges:?}"
        );
        assert_eq!(ranges[0], [0.2, 0.4], "{ranges:?}");
        assert_eq!(ranges[1], [0.6, 0.8], "{ranges:?}");
        assert_eq!(ranges[2], [0.9, 1.0], "{ranges:?}");
    }

    #[kithara::test(native, flash(false))]
    fn a_track_of_unknown_length_claims_nothing_unready() {
        let live = covered(&[(0, 200), (400, 600)], None);

        assert!(
            unready_ranges(&live).is_empty(),
            "with no extent there is no rest of the track to be missing"
        );
    }

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
            ui.set_analysis(Some(covered(runs, Some(1_000)).into()));
            let unready = ui.unready_ranges.to_vec();
            if let Some(previous) = previous {
                for range in &unready {
                    assert!(
                        previous
                            .iter()
                            .any(|was| was[0] <= range[0] && range[1] <= was[1]),
                        "a revision only takes ranges out: {range:?} was ready in {previous:?}"
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
