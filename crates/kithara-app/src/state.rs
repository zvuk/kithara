use std::sync::Arc;

use kithara::{
    abr::AbrHandle,
    events::{AbrMode, AppEvent, EngineEvent, Event, PlayerEvent, VariantInfo},
};
use kithara_platform::sync::Mutex;
use kithara_queue::{Queue, QueueEvent, TrackEntry};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

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
    pub abr_mode_is_auto: bool,
    pub is_seeking: bool,
    pub playing: bool,
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
            position: queue.position_seconds().unwrap_or(0.0),
            duration: queue.duration_seconds().unwrap_or(0.0),
            volume: queue.volume(),
            crossfade: queue.crossfade_duration(),
            crossfade_progress: None,
            eq_bands: vec![0.0; queue.eq_band_count()],
            status_note: None,
            is_seeking: false,
            seek_position: 0.0,
            selected_rate: 1.0,
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
    state: Arc<Mutex<UiState>>,
    cancel: CancellationToken,
}

impl StateController {
    /// Build a controller and start the listener task that mirrors
    /// queue events into [`UiState`].
    ///
    /// `cancel` must be a child of the app master so the listener task
    /// stops when the app shuts down; the controller's `Drop` also
    /// cancels it to stop the listener when the UI tears down first.
    pub fn new(queue: Arc<Queue>, cancel: CancellationToken) -> Self {
        let state = Arc::new(Mutex::new(UiState::new(&queue)));

        spawn_listener(Arc::clone(&queue), Arc::clone(&state), cancel.clone());

        Self {
            queue,
            state,
            cancel,
        }
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
        st.volume = self.queue.volume();
        st.position = position;
        st.duration = duration;
        if let Some(idx) = self.queue.current_index() {
            st.current_track_index = Some(idx);
        }
        st.variant_label = current_variant
            .as_ref()
            .map(variant_display_label_from_info)
            .unwrap_or_default();
        st.abr_variants = variants
            .iter()
            .map(|v| (v.variant_index, variant_short_label(v)))
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

fn spawn_listener(queue: Arc<Queue>, state: Arc<Mutex<UiState>>, cancel: CancellationToken) {
    let mut rx = queue.subscribe();

    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                event = rx.recv() => event,
            };

            match event {
                Ok(event) => apply_event(event, &queue, &state),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}

fn apply_event(event: Event, queue: &Queue, state: &Mutex<UiState>) {
    match event {
        Event::Queue(QueueEvent::CurrentTrackChanged { .. }) => {
            let current_index = queue.current_index();
            let mut st = state.lock_sync();
            st.current_track_index = current_index;
            st.track_name = current_index
                .and_then(|idx| st.tracks.get(idx).map(|t| t.name.clone()))
                .unwrap_or_default();
            st.selected_variant = None;
            st.is_seeking = false;
        }
        Event::Player(PlayerEvent::RateChanged { rate }) => {
            let mut st = state.lock_sync();
            st.playing = rate > 0.0;
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
    v.name.clone().unwrap_or_else(|| {
        v.bandwidth_bps.map_or_else(
            || format!("variant {}", v.variant_index),
            |b| format!("{} kbps", b / 1000),
        )
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
