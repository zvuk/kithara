use std::path::Path;

use kithara::{
    abr::AbrHandle,
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{
        AbrMode, AdvanceReason, AudioEvent, Event, EventReceiver, QueueEvent, SeekLifecycleStage,
        TrackId, TrackStatus, VariantInfo,
    },
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant, sleep, timeout},
        tokio,
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig, SeekOutcome, SessionDispatcher},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{kithara, offline::OfflineSession};
use url::Url;

use super::actions::Action;

/// Worst-case wall-clock budget for any single action. Anything longer
/// is treated as a hang — matches the "no hang" invariant from the
/// player robustness plan.
pub(crate) const ACTION_BUDGET: Duration = Duration::from_secs(8);
/// Tolerance window around the seek target. The reader snaps to
/// keyframe / segment boundaries, so an exact match is not realistic.
pub(crate) const SEEK_TARGET_TOLERANCE_S: f64 = 1.5;
/// If a track auto-advances within this window of `duration`, treat
/// the transition as a legitimate natural EOF — not the false-EOF
/// auto-advance the harness is trying to catch. Larger than seek
/// tolerance so the post-seek decode can chew through any residual
/// audio before EOF without the harness misclassifying it as a bug.
pub(crate) const NATURAL_EOF_WINDOW_S: f64 = 3.0;
/// Tick interval driving the loop polling helpers.
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const RENDER_BLOCK_FRAMES: usize = 512;
const RENDER_BATCH_BLOCKS: usize = 16;
const RENDER_STALL_BUDGET: Duration = Duration::from_secs(3);

/// Periodic queue tick driver, run as a spawned task. `#[kithara::flash(true)]`
/// makes the body flash-ACTIVE under an ambient (flash) test, so its
/// `sleep` resolves to the engine-backed virtual timer — without it the
/// async spawn chokepoint only propagates the per-test ambient gate (it does
/// NOT set the active flag, unlike the sync `spawn_named` pacer), and
/// `kithara::platform::time::sleep` keys on the active flag, so the driver
/// would run on a REAL timer. A real-paced driver keeps up with the virtual
/// clock only while ongoing real I/O paces it (HLS); for a fully-buffered
/// source (file / local mp3) the virtual clock collapses ahead of it, the
/// cached playhead freezes relative to virtual time, and the action
/// watchdog false-fires. Off the `flash` feature the macro is a no-op, so
/// the driver is a plain real-time tick exactly as before.
#[kithara::flash(true)]
async fn run_tick_driver(queue: Arc<Queue>) {
    loop {
        sleep(Duration::from_millis(50)).await;
        if queue.tick().is_err() {
            break;
        }
    }
}

/// `SimHarness` is the integration-test entry point for the user
/// simulation scenarios. It owns a `Queue + Player + Downloader` triple
/// plus a tokio tick task; callers drive it with `Action`s and the
/// harness asserts the per-action invariants from the plan.
pub(crate) struct SimHarness {
    queue: Arc<Queue>,
    session: Arc<OfflineSession>,
    tick: tokio::task::JoinHandle<()>,
    _downloader: Downloader,
    _store: StoreOptions,
    track_ids: Vec<TrackId>,
    /// Captured codec of the currently-playing variant. Updated by
    /// `enter_track` and on each successful quality switch; the
    /// `SetQuality` action compares the codec before and after the
    /// switch to catch cross-codec recreate regressions.
    last_known_codec: Option<String>,
}

/// Single owned URL+backend descriptor used to populate the queue.
///
/// `abr_mode` defaults to `Auto(None)` — matching the prod
/// `kithara-app` config — but tests parametrise over `Manual(idx)` as
/// well so the ABR-on and ABR-off code paths both get exercised.
#[derive(Clone, Debug)]
pub(crate) struct TrackSpec {
    pub url: Url,
    pub backend: DecoderBackend,
    pub abr_mode: AbrMode,
}

impl TrackSpec {
    pub(crate) fn new(url: Url, backend: DecoderBackend) -> Self {
        Self {
            url,
            backend,
            abr_mode: AbrMode::Auto(None),
        }
    }

    pub(crate) fn with_abr_mode(mut self, mode: AbrMode) -> Self {
        self.abr_mode = mode;
        self
    }

    pub(crate) fn with_backend(mut self, backend: DecoderBackend) -> Self {
        self.backend = backend;
        self
    }
}

impl SimHarness {
    /// Build a fresh `Queue + Player` against `cache_path` and append
    /// every track in `specs`. Does **not** call `select` — that's left
    /// to the scenario via `enter_track`.
    pub(crate) async fn new(cache_path: &Path, specs: &[TrackSpec]) -> Self {
        let session = Arc::new(OfflineSession::new());
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .byte_pool(kithara::bufpool::BytePool::default())
                .pcm_pool(kithara::bufpool::PcmPool::default())
                .session(Arc::clone(&session) as Arc<dyn SessionDispatcher>)
                .build(),
        ));
        let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
        let queue_for_tick = Arc::clone(&queue);
        // Spawn through the platform chokepoint, NOT raw `tokio::spawn`: under
        // flash this installs the quiescence poll-wrapper + ambient gate so the
        // driver participates in the virtual clock. The active flag that makes
        // its `sleep` engine-virtual comes from `#[kithara::flash(true)]` on
        // `run_tick_driver` (the async chokepoint propagates ambient only) — see
        // that fn's doc for why a real-paced driver false-HANGs buffered sources.
        let tick = tokio::task::spawn(run_tick_driver(queue_for_tick));

        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                NetOptions::default(),
                CancelToken::never(),
            ))
            .build(),
        );
        let store = StoreOptions::new(cache_path);

        let mut track_ids = Vec::with_capacity(specs.len());
        for spec in specs {
            let cfg = ResourceConfig::for_src(spec.url.as_str())
                .expect("valid track URL")
                .byte_pool(kithara::bufpool::BytePool::default())
                .pcm_pool(kithara::bufpool::PcmPool::default())
                .downloader(downloader.clone())
                .store(store.clone())
                .decoder(
                    kithara::audio::AudioDecoderConfig::builder()
                        .backend(spec.backend)
                        .build(),
                )
                .initial_abr_mode(spec.abr_mode)
                .build();
            let id = queue.append(TrackSource::Config(Box::new(cfg)));
            track_ids.push(id);
        }

        Self {
            queue,
            session,
            tick,
            _downloader: downloader,
            _store: store,
            track_ids,
            last_known_codec: None,
        }
    }

    pub(crate) fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    pub(crate) fn track_id(&self, idx: usize) -> TrackId {
        self.track_ids[idx]
    }

    pub(crate) fn subscribe(&self) -> EventReceiver {
        self.queue.subscribe()
    }

    /// Start playback of the queue track at `idx`: select it, wait for
    /// Loaded, play for `warmup` so the audio worker is past initial
    /// pre-roll, then snapshot the codec so `SetQuality` has a baseline.
    pub(crate) async fn enter_track(&mut self, idx: usize, warmup: Duration) {
        let id = self.track_ids[idx];
        self.queue
            .select(id, Transition::None)
            .unwrap_or_else(|e| panic!("select track {idx}: {e}"));

        wait_for_loaded(&self.queue, id, Duration::from_secs(30))
            .await
            .unwrap_or_else(|e| panic!("enter_track[{idx}] load: {e}"));

        wait_for_position_at_least(&self.queue, warmup.as_secs_f64(), Duration::from_secs(15))
            .await
            .unwrap_or_else(|e| panic!("enter_track[{idx}] warmup: {e}"));

        self.last_known_codec = self.current_codec();
    }

    /// Apply one user-simulation action and assert the four invariants
    /// from the plan. Panics on hang, on out-of-tolerance seek landing,
    /// on spurious auto-advance, and on quality-switch cross-codec
    /// recreate — that's what gives the bug repros their teeth.
    pub(crate) async fn apply(&mut self, action: Action) {
        match action {
            Action::SeekRatio(r) => self.do_seek(r, false).await,
            Action::SeekNearEnd(r) => self.do_seek(r, true).await,
            Action::SelectAt(i) => self.do_select_at(i).await,
            Action::SelectPrev => self.do_select_prev().await,
            Action::SelectNext => self.do_select_next().await,
            Action::SetQuality(idx) => self.do_set_quality(idx).await,
            Action::QualityAuto => self.do_quality_auto().await,
            Action::Pause => self.do_pause(),
            Action::Resume => self.do_resume(),
            Action::PlayFor(d) => self.do_play_for(d).await,
            Action::RenderFor(d) => self.do_render_for(d).await,
        }
    }

    /// Wait for any in-flight seek to settle, then assert the queue is
    /// at a sane terminal state for the scenario. Used as the final
    /// step in scripted scenarios.
    pub(crate) async fn shutdown(self) {
        self.tick.abort();
        let _ = self.tick.await;
        drop(self.queue);
        drop(self.session);
        drop(self._downloader);
    }

    fn current_codec(&self) -> Option<String> {
        let variant = self.queue.current_variant()?;
        variant.codecs.or(variant.container)
    }

    fn current_abr_handle(&self) -> Option<AbrHandle> {
        self.queue.current_abr_handle()
    }

    fn current_variant(&self) -> Option<VariantInfo> {
        self.queue.current_variant()
    }

    fn position(&self) -> f64 {
        self.queue.position_seconds().unwrap_or(0.0)
    }

    fn duration(&self) -> f64 {
        self.queue.duration_seconds().unwrap_or(0.0)
    }

    fn is_playing(&self) -> bool {
        self.queue.is_playing()
    }

    fn current_track_id(&self) -> Option<TrackId> {
        self.queue.current().map(|e| e.id)
    }

    /// True when the current track has reached natural EOF and the
    /// player has stopped — there is no more audio to advance through.
    ///
    /// After natural EOF the player emits no further `PlaybackProgress`
    /// (position cannot advance — correct behaviour, not a bug), so any
    /// action that would otherwise wait for "position advanced" must
    /// treat this state as terminal-but-fine rather than a hang. The two
    /// signals together rule out a mid-track false positive: position
    /// within `NATURAL_EOF_WINDOW_S` of `duration` AND the engine no
    /// longer playing (the EOF path stops the slot once the last frame
    /// is served). A track that is genuinely playable near the tail
    /// still reports `is_playing() == true`, so the must-advance path
    /// stays in force for it.
    fn at_natural_eof(&self) -> bool {
        let duration = self.duration();
        duration > 0.0 && (duration - self.position()) <= NATURAL_EOF_WINDOW_S && !self.is_playing()
    }

    async fn do_seek(&mut self, ratio: f64, near_end: bool) {
        let action_label = if near_end {
            format!("SeekNearEnd({ratio:.3})")
        } else {
            format!("SeekRatio({ratio:.3})")
        };
        let duration = self.duration();
        assert!(
            duration > 0.0,
            "[{action_label}] duration unknown — track did not load before seek"
        );
        let target = (duration * ratio).clamp(0.0, duration);
        let pre_track = self.current_track_id();
        let pre_pos = self.position();

        let outcome = self
            .queue
            .seek(target)
            .unwrap_or_else(|e| panic!("[{action_label}] queue.seek returned Err: {e}"));

        match outcome {
            SeekOutcome::Landed { .. } => {}
            SeekOutcome::PastEof { duration, .. } => {
                // PastEof is only acceptable for SeekNearEnd very close
                // to 1.0; everything else means we computed `target` wrong.
                assert!(
                    near_end && ratio >= 0.95,
                    "[{action_label}] unexpected PastEof (dur={duration:?})"
                );
                return;
            }
        }

        // Settle on the seek by awaiting the player's own
        // `PlaybackProgress` events (production-truth sink commits)
        // until the reported position lands within tolerance of the
        // target. `recv()` parks on the virtual clock, so the engine
        // advances time + renders the post-seek blocks while we wait;
        // `ACTION_BUDGET` is a virtual deadline that still fails a real
        // hang loudly. Per-event we re-check the Failed + track-flip
        // invariants so a false-EOF auto-advance is surfaced as the
        // SPURIOUS AUTO-ADVANCE panic below rather than as a misleading
        // settle timeout.
        let mut rx = self.queue.subscribe();
        // Highest position observed while still on the pre-seek track.
        // On a flip this is the seek's landing point on the *old*
        // track — for a legitimate near-end EOF that is ≈ `duration`,
        // so the carve-out below classifies it correctly instead of
        // mis-reading the new track's reset-to-0 position.
        let mut last_old_pos = pre_pos;
        let settle = async {
            loop {
                if let Some(entry) = pre_track.and_then(|id| self.queue.track(id))
                    && let TrackStatus::Failed(err) = &entry.status
                {
                    panic!("[{action_label}] track entered Failed during seek: {err}");
                }
                // A flip OFF a live pre-seek track stops the settle and is
                // handed to the SPURIOUS AUTO-ADVANCE logic below. But when the
                // queue had already ended before this seek (`pre_track == None`,
                // last track played to natural EOF), the seek is a
                // Superpowered-style revive: the cursor flips None→the revived
                // track by design. Keep waiting for the seek target instead, so
                // the revive is validated by actually reaching it.
                if pre_track.is_some() && self.current_track_id() != pre_track {
                    return None;
                }
                let cur = self.position();
                last_old_pos = cur;
                if (cur - target).abs() <= SEEK_TARGET_TOLERANCE_S {
                    return Some(cur);
                }
                let ev = match recv_event(&mut rx).await {
                    Ok(Some(ev)) => ev,
                    // Recoverable lag — keep waiting.
                    Ok(None) => continue,
                    // Bus closed (queue/player dropped): nothing more to
                    // wait on. Surface as unsettled rather than spin.
                    Err(_) => return None,
                };
                if let Some(p) = progress_secs(&ev) {
                    if self.current_track_id() == pre_track {
                        last_old_pos = p;
                    }
                    if (p - target).abs() <= SEEK_TARGET_TOLERANCE_S {
                        return Some(p);
                    }
                }
            }
        };
        let landed = match timeout(ACTION_BUDGET, settle).await {
            Ok(Some(cur)) => cur,
            Ok(None) => last_old_pos,
            Err(_) => {
                if let Some(entry) = pre_track.and_then(|id| self.queue.track(id))
                    && let TrackStatus::Failed(err) = &entry.status
                {
                    panic!("[{action_label}] track entered Failed during seek: {err}");
                }
                let post = self.position();
                panic!(
                    "[{action_label}] HANG: seek to {target:.2}s never settled \
                     within {budget:?} (pre={pre_pos:.2}s, post={post:.2}s, \
                     dur={duration:.2}s)",
                    budget = ACTION_BUDGET
                );
            }
        };

        // Only a flip OFF a live pre-seek track can be a spurious auto-advance
        // (Bug #5). A revive from an already-ended queue (`pre_track == None`,
        // last track at natural EOF) legitimately flips None→the revived track
        // and is validated by the settle above reaching the seek target, so it
        // is not treated as an auto-advance.
        if pre_track.is_some() && self.current_track_id() != pre_track {
            // Two cases:
            //   (a) `near_end == true` and the seek landed inside the
            //       NATURAL_EOF_WINDOW — legitimate natural EOF after a
            //       deliberately-tail-targeted seek. NOT a bug, return.
            //   (b) anything else — false-EOF auto-advance (Bug #5).
            //
            // The legitimacy carve-out is only for `SeekNearEnd`; a
            // regular `SeekRatio` should never legitimately land close
            // enough to natural EOF to auto-advance, because seek
            // tolerance is well below NATURAL_EOF_WINDOW. Folding both
            // into the same branch was a mistake — it masked the bug.
            if near_end && (duration - landed).abs() <= NATURAL_EOF_WINDOW_S {
                return;
            }
            panic!(
                "[{action_label}] SPURIOUS AUTO-ADVANCE: track flipped \
                 from {pre_track:?} to {:?} during seek (target={target:.2}s, \
                 landed={landed:.2}s, dur={duration:.2}s)",
                self.current_track_id()
            );
        }
    }

    async fn do_select_at(&mut self, idx: usize) {
        let bounded = idx % self.track_ids.len();
        let id = self.track_ids[bounded];
        self.queue
            .select(id, Transition::None)
            .unwrap_or_else(|e| panic!("[SelectAt({bounded})] select failed: {e}"));
        wait_for_loaded(&self.queue, id, Duration::from_secs(20))
            .await
            .unwrap_or_else(|e| panic!("[SelectAt({bounded})] load: {e}"));
        // Wait for the player to actually become this track — the
        // engine handover happens after `Loaded` via the
        // `CurrentItemChanged` notification, surfaced on the bus as
        // `QueueEvent::CurrentTrackChanged`. Awaiting that event (which
        // parks on the virtual clock) confirms the engine is now serving
        // the new track. Without this step `do_play_for` captures
        // `pre_pos` from the *previous* track, racing the handover.
        let mut rx = self.queue.subscribe();
        let switch_wait = async {
            loop {
                if self.current_track_id() == Some(id) {
                    return;
                }
                match recv_event(&mut rx).await {
                    Ok(Some(Event::Queue(QueueEvent::CurrentTrackChanged { id: Some(cur) })))
                        if cur == id =>
                    {
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        };
        let _ = timeout(Duration::from_secs(5), switch_wait).await;
        // Refresh codec snapshot — picking a different track legitimately
        // changes the codec, so this is a reset, not an assertion.
        self.last_known_codec = self.current_codec();
    }

    async fn do_select_prev(&mut self) {
        let mut rx = self.queue.subscribe();
        if self.queue.return_to_previous(Transition::None).is_some() {
            self.await_current_changed(&mut rx).await;
            self.last_known_codec = self.current_codec();
        }
    }

    async fn do_select_next(&mut self) {
        let mut rx = self.queue.subscribe();
        if self
            .queue
            .advance_to_next(Transition::None, AdvanceReason::UserNext)
            .is_some()
        {
            self.await_current_changed(&mut rx).await;
            self.last_known_codec = self.current_codec();
        }
    }

    /// Park (on the virtual clock) until the queue publishes a
    /// `CurrentTrackChanged`, or a short virtual deadline elapses. Lets
    /// the engine run the handover instead of guessing with a fixed
    /// sleep that may fire before the tick driver applies the change.
    async fn await_current_changed(&self, rx: &mut EventReceiver) {
        let wait = async {
            loop {
                match recv_event(rx).await {
                    Ok(Some(Event::Queue(QueueEvent::CurrentTrackChanged { .. }))) => return,
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        };
        let _ = timeout(Duration::from_secs(5), wait).await;
    }

    async fn do_set_quality(&mut self, idx: usize) {
        let Some(handle) = self.current_abr_handle() else {
            return;
        };
        let pre_track = self.current_track_id();
        let pre_pos = self.position();
        // Captured while the track is still current (duration known) so the
        // natural-EOF carve-out below does not depend on the live
        // `position()`/`is_playing()`, which race the queue cursor at EOF.
        let pre_dur = self.duration();
        let bounded = idx % 4;
        // Subscribe BEFORE the mode change so no post-switch
        // `PlaybackProgress` event can slip through between the switch
        // and the first `recv()`.
        let mut rx = self.queue.subscribe();
        if let Err(e) = handle.set_mode(AbrMode::manual(bounded)) {
            // Out-of-bounds is acceptable — fixtures with fewer
            // variants reject the request; this is expected user
            // input on a track that doesn't expose that quality.
            tracing::debug!(?e, idx = bounded, "set_mode out of range (expected)");
            return;
        }
        let mut baseline_pos = pre_pos;
        let mut last_pos = pre_pos;
        let mut no_progress_ticks: u32 = 0;
        let mut switch_ticks: u32 = 0;
        let mut seek_rebase_allowed = false;
        const STAGNATION_TICKS: u32 = 75;
        const SWITCH_PROGRESS_TICKS: u32 = 200;

        while switch_ticks < SWITCH_PROGRESS_TICKS {
            if pre_track.is_some() && self.current_track_id() != pre_track {
                if self.current_track_id().is_none()
                    && pre_dur > 0.0
                    && (pre_dur - pre_pos) <= NATURAL_EOF_WINDOW_S
                {
                    self.last_known_codec = self.current_codec();
                    return;
                }
                panic!(
                    "[SetQuality({bounded})] TRACK FLIPPED across quality switch \
                     (pre={pre_track:?}, post={:?}, pre_pos={pre_pos:.2}s)",
                    self.current_track_id()
                );
            }
            if let Some(id) = pre_track
                && let Some(entry) = self.queue.track(id)
                && let TrackStatus::Failed(err) = &entry.status
            {
                panic!("[SetQuality({bounded})] track Failed mid-switch: {err}");
            }

            let cur = self.position();
            if seek_rebase_allowed && cur + 1.0 < baseline_pos {
                baseline_pos = cur;
            }
            if cur + 1.0 < last_pos || cur > last_pos + 0.05 {
                last_pos = cur;
                no_progress_ticks = 0;
            }
            if cur > baseline_pos + 0.1 || self.at_natural_eof() {
                self.last_known_codec = self.current_codec();
                return;
            }
            if no_progress_ticks >= STAGNATION_TICKS {
                panic!(
                    "[SetQuality({bounded})] HANG: position stuck after switch \
                     (pre_pos={pre_pos:.2}s, baseline={baseline_pos:.2}s, \
                      last={last_pos:.2}s, post={cur:.2}s, \
                      observed_ticks={switch_ticks})"
                );
            }

            let mut counts_as_playback_tick = false;
            let mut progressed = false;
            match timeout(POLL_INTERVAL, recv_event(&mut rx)).await {
                Ok(Ok(Some(ev))) => {
                    match &ev {
                        Event::Audio(AudioEvent::SeekLifecycle {
                            stage: SeekLifecycleStage::SeekApplied,
                            ..
                        }) => {
                            seek_rebase_allowed = true;
                        }
                        Event::Audio(AudioEvent::SeekComplete { position, .. }) => {
                            seek_rebase_allowed = true;
                            baseline_pos = position.as_secs_f64();
                            last_pos = baseline_pos;
                            progressed = true;
                        }
                        _ => {}
                    }
                    if let Some(p) = progress_secs(&ev) {
                        counts_as_playback_tick = true;
                        if seek_rebase_allowed && p + 1.0 < baseline_pos {
                            baseline_pos = p;
                        }
                        if p + 1.0 < last_pos || p > last_pos + 0.05 {
                            last_pos = p;
                            progressed = true;
                        }
                        if p > baseline_pos + 0.1 {
                            self.last_known_codec = self.current_codec();
                            return;
                        }
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(_)) => break,
                Err(_) => {
                    counts_as_playback_tick = true;
                }
            }
            if counts_as_playback_tick {
                switch_ticks = switch_ticks.saturating_add(1);
                if progressed {
                    no_progress_ticks = 0;
                } else {
                    no_progress_ticks = no_progress_ticks.saturating_add(1);
                }
            }
        }

        if !self.at_natural_eof() {
            if pre_track.is_some() && self.current_track_id() != pre_track {
                panic!(
                    "[SetQuality({bounded})] TRACK FLIPPED across quality switch \
                     (pre={pre_track:?}, post={:?}, pre_pos={pre_pos:.2}s)",
                    self.current_track_id()
                );
            }
            if let Some(id) = pre_track
                && let Some(entry) = self.queue.track(id)
                && let TrackStatus::Failed(err) = &entry.status
            {
                panic!("[SetQuality({bounded})] track Failed mid-switch: {err}");
            }
            panic!(
                "[SetQuality({bounded})] HANG: position did not clear pre-switch point \
                 (pre_pos={pre_pos:.2}s, baseline={baseline_pos:.2}s, post={:.2}s, \
                  last={last_pos:.2}s, ticks={switch_ticks})",
                self.position()
            );
        }
        self.last_known_codec = self.current_codec();
    }

    async fn do_quality_auto(&mut self) {
        let Some(handle) = self.current_abr_handle() else {
            return;
        };
        let mut rx = self.queue.subscribe();
        let _ = handle.set_mode(AbrMode::Auto(None));
        // Park briefly (virtual clock) for the mode to apply rather
        // than a fixed sleep that may fire before the engine reacts.
        let settle = async {
            let _ = recv_event(&mut rx).await;
        };
        let _ = timeout(Duration::from_millis(100), settle).await;
    }

    fn do_pause(&mut self) {
        self.queue.pause();
    }

    fn do_resume(&mut self) {
        self.queue.play();
    }

    async fn do_play_for(&mut self, at_least: Duration) {
        let mut pre_pos = self.position();
        let pre_track = self.current_track_id();
        let duration = self.duration();
        let started = Instant::now();

        // Allow up to 2x the requested wall clock so we accept some
        // jitter, but anything beyond that with no progress is a hang.
        // Under flash these are virtual deadlines.
        let wall_budget = at_least * 2;
        let mut last_pos = pre_pos;
        // No-progress is counted in producer-tick *wakes*, not virtual wall
        // time: under flash a quiescent virtual clock can jump past a 3s
        // window in a single tick while the producer simply has not been
        // re-ticked yet, tripping a false SILENT HANG. Each loop wake (a
        // delivered bus event OR a `POLL_INTERVAL` poll tick) is one real
        // producer opportunity; counting consecutive no-progress wakes keys
        // the detector on observed producer state instead of a clock the
        // virtual engine fast-forwards past.
        let mut no_progress_ticks: u32 = 0;

        /// Consecutive no-progress producer-tick wakes before a stuck
        /// playhead is a SILENT HANG. At one wake per `POLL_INTERVAL`
        /// (40ms) this is the ~3s window the panic message names.
        const STAGNATION_TICKS: u32 = 75;

        // Drive the watchdog off current playback opportunities instead of a
        // blind `sleep` cadence: `PlaybackProgress` proves the sink committed a
        // block, and a `POLL_INTERVAL` timeout proves one queue tick elapsed
        // with no relevant bus event. Unrelated preload/download events from a
        // next track are deliberately ignored; they are not producer chances
        // for the current playhead.
        let mut rx = self.queue.subscribe();
        while started.elapsed() < wall_budget {
            if let Some(id) = pre_track
                && let Some(entry) = self.queue.track(id)
                && let TrackStatus::Failed(err) = &entry.status
            {
                panic!(
                    "[PlayFor({}ms)] track Failed mid-play: {err}",
                    at_least.as_millis()
                );
            }
            let cur = self.position();
            // Track switch detection: position can drop after SelectAt
            // (new track reports from 0 once it starts producing). The
            // PlayFor watchdog is measuring progress on whichever track
            // is *now* playing, so rebase `pre_pos` and `last_pos` to
            // the post-switch baseline rather than mistakenly comparing
            // against the previous track's high-water mark.
            if cur + 1.0 < pre_pos {
                pre_pos = cur;
                last_pos = cur;
                no_progress_ticks = 0;
            }
            if cur > last_pos + 0.05 {
                last_pos = cur;
                no_progress_ticks = 0;
            }
            if self.is_playing() {
                if cur - pre_pos >= at_least.as_secs_f64() * 0.9 {
                    // Made the requested progress — done.
                    return;
                }
                // Reached natural EOF — legitimate stop. Don't keep
                // waiting for a budget we'll never satisfy.
                if duration > 0.0 && (duration - cur).abs() < 0.5 {
                    return;
                }
                if no_progress_ticks >= STAGNATION_TICKS
                    && self.position() < pre_pos + 0.1
                    && (duration <= 0.0 || (duration - cur).abs() >= NATURAL_EOF_WINDOW_S)
                {
                    panic!(
                        "[PlayFor({}ms)] SILENT HANG: position stuck at {cur:.3}s for 3s+ \
                         (pre={pre_pos:.3}s, target_advance={target:.3}s, dur={duration:.3}s)",
                        at_least.as_millis(),
                        target = at_least.as_secs_f64()
                    );
                }
            }
            // Park for the next bus event, but never longer than one
            // `POLL_INTERVAL` — so a track that emits no further progress
            // events still wakes once per producer tick, and each such wake is
            // one no-progress observation the SILENT HANG above counts. The
            // outer `while started.elapsed() < wall_budget` bounds the total.
            // Both are virtual under flash.
            // A closed bus delivers `Err` instantly; without breaking we
            // would busy-spin and (under flash) freeze the virtual clock.
            // Stop watching and fall through to the post-loop checks.
            let counts_as_playback_tick = match timeout(POLL_INTERVAL, recv_event(&mut rx)).await {
                Ok(Ok(Some(Event::Audio(AudioEvent::PlaybackProgress { .. })))) => true,
                Ok(Ok(Some(_)) | Ok(None)) => false,
                Ok(Err(_)) => break,
                Err(_) => true,
            };
            if counts_as_playback_tick {
                // One more current-playback opportunity elapsed without
                // progress (progress resets this to 0 at the top of the loop).
                no_progress_ticks = no_progress_ticks.saturating_add(1);
            }
        }

        let post = self.position();
        let advance = post - pre_pos;
        let target = at_least.as_secs_f64();
        // Treat reaching natural EOF as a legitimate completion of a
        // shorter-than-requested PlayFor. Only panic on PARTIAL HANG
        // when post position is well below duration.
        let reached_eof = duration > 0.0 && (duration - post).abs() < 0.5;
        if !reached_eof && advance < target * 0.5 {
            panic!(
                "[PlayFor({}ms)] PARTIAL HANG: only advanced {advance:.3}s in {wall:?} \
                 (target={target:.3}s, pre={pre_pos:.3}s, post={post:.3}s, dur={duration:.3}s)",
                at_least.as_millis(),
                wall = wall_budget
            );
        }
        // Verify no spurious auto-advance during the window: if the
        // current_track flipped without crossing the natural EOF, the
        // queue auto-advanced too early.
        if let Some(pre_id) = pre_track
            && self.current_track_id() != Some(pre_id)
        {
            let pre_entry = self.queue.track(pre_id);
            // The track that flipped away may already have been popped
            // off the navigation cursor, so `self.duration()` now reports
            // the *new* current's duration (or 0). Read the duration we
            // captured before the action ran instead — pre_entry holds a
            // snapshot of the lost track's loader state but not its
            // duration; fall back to `post` + 0.5s as a permissive guard
            // (anything beyond `NATURAL_EOF_WINDOW_S` past `pre_pos +
            // requested_play` is bug territory).
            let expected_advance = at_least.as_secs_f64();
            let advanced = post - pre_pos;
            if advanced < expected_advance - NATURAL_EOF_WINDOW_S {
                panic!(
                    "[PlayFor({}ms)] SPURIOUS AUTO-ADVANCE: track flipped from \
                     {pre_id:?} to {:?} at position {post:.2}s after only \
                     {advanced:.2}s of playback (requested {expected_advance:.2}s). \
                     pre_status={:?}",
                    at_least.as_millis(),
                    self.current_track_id(),
                    pre_entry.map(|e| e.status)
                );
            }
        }
    }

    async fn do_render_for(&mut self, at_least: Duration) {
        let started = Instant::now();
        let mut pre_pos = self.position();
        let pre_track = self.current_track_id();
        let duration = self.duration();
        let target = at_least.as_secs_f64();
        let mut last_pos = pre_pos;
        let mut last_progress_at = started;
        let mut last_progress_block: usize = 0;
        let mut progress_events: usize = 0;
        let mut rendered_blocks: usize = 0;
        let mut rx = self.queue.subscribe();

        while started.elapsed() < ACTION_BUDGET {
            if let Some(id) = pre_track
                && let Some(entry) = self.queue.track(id)
                && let TrackStatus::Failed(err) = &entry.status
            {
                panic!(
                    "[RenderFor({}ms)] track Failed mid-render: {err}",
                    at_least.as_millis()
                );
            }

            let cur = self.position();
            if cur + 1.0 < pre_pos {
                pre_pos = cur;
                last_pos = cur;
                last_progress_at = Instant::now();
            }
            if cur > last_pos + 0.05 {
                last_pos = cur;
                last_progress_at = Instant::now();
                last_progress_block = rendered_blocks;
            }
            if self.is_playing() {
                if cur - pre_pos >= target * 0.9 {
                    return;
                }
                if duration > 0.0 && (duration - cur).abs() < 0.5 {
                    return;
                }
                if last_progress_at.elapsed() >= RENDER_STALL_BUDGET
                    && (duration <= 0.0 || (duration - cur).abs() >= NATURAL_EOF_WINDOW_S)
                {
                    panic!(
                        "[RenderFor({}ms)] SILENT HANG: position stuck at {cur:.3}s for \
                         {stall:?} \
                         (pre={pre_pos:.3}s, target_advance={target:.3}s, dur={duration:.3}s, \
                         wall={wall:?}, rendered_blocks={rendered_blocks}, \
                         last_progress_block={last_progress_block}, progress_events={progress_events}, \
                         playing={playing}, player_status={player_status:?}, track_status={track_status:?}, \
                         engine_load={engine_load:?})",
                        at_least.as_millis(),
                        stall = RENDER_STALL_BUDGET,
                        wall = started.elapsed(),
                        playing = self.is_playing(),
                        player_status = self.queue.status(),
                        track_status = pre_track
                            .and_then(|id| self.queue.track(id))
                            .map(|entry| entry.status),
                        engine_load = self.queue.engine_load(),
                    );
                }
            }

            for _ in 0..RENDER_BATCH_BLOCKS {
                let _ = self.session.render(RENDER_BLOCK_FRAMES);
            }
            rendered_blocks = rendered_blocks.saturating_add(RENDER_BATCH_BLOCKS);
            let _ = self.queue.tick();
            let post_render = self.position();
            let saw_progress = drain_playback_progress(&mut rx);
            if saw_progress || post_render > last_pos + 0.05 {
                last_pos = last_pos.max(post_render);
                last_progress_at = Instant::now();
                last_progress_block = rendered_blocks;
                progress_events = progress_events.saturating_add(1);
                tokio::task::yield_now().await;
            } else {
                sleep(POLL_INTERVAL).await;
            }
        }

        let post = self.position();
        let advance = post - pre_pos;
        let reached_eof = duration > 0.0 && (duration - post).abs() < 0.5;
        if !reached_eof && advance < target * 0.5 {
            panic!(
                "[RenderFor({}ms)] PARTIAL HANG: only advanced {advance:.3}s in {wall_budget:?} \
                 (target={target:.3}s, pre={pre_pos:.3}s, post={post:.3}s, dur={duration:.3}s, \
                 wall={wall:?}, last_progress={last_pos:.3}s@block#{last_progress_block}, \
                 blocks_since_progress={blocks_since_progress}, progress_events={progress_events}, \
                 playing={playing}, player_status={player_status:?}, track_status={track_status:?}, \
                 engine_load={engine_load:?})",
                at_least.as_millis(),
                wall_budget = ACTION_BUDGET,
                wall = started.elapsed(),
                blocks_since_progress = rendered_blocks.saturating_sub(last_progress_block),
                playing = self.is_playing(),
                player_status = self.queue.status(),
                track_status = pre_track
                    .and_then(|id| self.queue.track(id))
                    .map(|entry| entry.status),
                engine_load = self.queue.engine_load(),
            );
        }

        if let Some(pre_id) = pre_track
            && self.current_track_id() != Some(pre_id)
        {
            let pre_entry = self.queue.track(pre_id);
            let expected_advance = at_least.as_secs_f64();
            let advanced = post - pre_pos;
            if advanced < expected_advance - NATURAL_EOF_WINDOW_S {
                panic!(
                    "[RenderFor({}ms)] SPURIOUS AUTO-ADVANCE: track flipped from \
                     {pre_id:?} to {:?} at position {post:.2}s after only \
                     {advanced:.2}s of playback (requested {expected_advance:.2}s). \
                     pre_status={:?}",
                    at_least.as_millis(),
                    self.current_track_id(),
                    pre_entry.map(|e| e.status)
                );
            }
        }
    }
}

fn drain_playback_progress(rx: &mut EventReceiver) -> bool {
    let mut saw_progress = false;
    loop {
        match rx.try_recv().map(|env| env.event) {
            Ok(Event::Audio(AudioEvent::PlaybackProgress { .. })) => saw_progress = true,
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return saw_progress,
        }
    }
}

/// Drain `recv()` once, mapping the broadcast lag/closed errors to a
/// flow-control signal. `Ok(Some(ev))` is a delivered event,
/// `Ok(None)` is a recoverable lag (caller should keep waiting), and
/// `Err` is a closed bus (terminal). `recv()` itself parks on the
/// virtual clock — awaiting it is what lets the engine advance time
/// (run the tick driver + decode worker) until real state changes.
async fn recv_event(rx: &mut EventReceiver) -> Result<Option<Event>, String> {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    match rx.recv().await {
        Ok(env) => Ok(Some(env.event)),
        Err(RecvError::Lagged(_)) => Ok(None),
        Err(RecvError::Closed) => Err("event bus closed".to_string()),
    }
}

/// Convert an `AudioEvent::PlaybackProgress` position into seconds.
/// `position_ms as f64` mirrors the established sibling tests
/// (`local_track_plays`, `hls_seek_near_end_stress`): playback
/// positions stay far below `2^53` ms, so the cast is exact.
fn progress_secs(ev: &Event) -> Option<f64> {
    if let Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = ev {
        Some(*position_ms as f64 / 1000.0)
    } else {
        None
    }
}

/// Await until the player publishes a `PlaybackProgress` whose reported
/// position satisfies `done`, OR `deadline` (a virtual deadline under
/// flash) elapses. Returns the satisfying position on success.
///
/// Driving the wait off `PlaybackProgress` (production-truth sink
/// commits) instead of a `sleep`-cadence poll of `position_seconds()`
/// is what makes the harness flash-correct: `recv()` parks on the
/// virtual clock, so the engine is free to advance time, run the tick
/// driver, and let the decode worker produce the next block — exactly
/// the events the wait then resolves on.
async fn await_progress(
    rx: &mut EventReceiver,
    queue: &Queue,
    mut done: impl FnMut(f64) -> bool,
    deadline: Duration,
) -> Result<f64, String> {
    // The current cached position may already satisfy the predicate
    // (the tick driver updated it before we subscribed) — check before
    // blocking so a no-op wait returns immediately.
    if let Some(p) = queue.position_seconds()
        && done(p)
    {
        return Ok(p);
    }
    let wait = async {
        loop {
            let Some(ev) = recv_event(rx).await? else {
                continue;
            };
            if let Some(p) = progress_secs(&ev)
                && done(p)
            {
                return Ok(p);
            }
        }
    };
    match timeout(deadline, wait).await {
        Ok(res) => res,
        Err(_) => Err(format!(
            "no PlaybackProgress satisfied predicate within {deadline:?} \
             (last cached={:?})",
            queue.position_seconds()
        )),
    }
}

pub(crate) async fn wait_for_loaded(
    queue: &Queue,
    id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    if let Some(entry) = queue.track(id) {
        match &entry.status {
            TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
            TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
            _ => {}
        }
    }
    let mut rx = queue.subscribe();
    let wait = async {
        loop {
            // Re-check on every event: status may have flipped between
            // the pre-subscribe snapshot and the first delivered event.
            if let Some(entry) = queue.track(id) {
                match &entry.status {
                    TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                    TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
                    _ => {}
                }
            }
            let Some(ev) = recv_event(&mut rx).await? else {
                continue;
            };
            if let Event::Queue(QueueEvent::TrackStatusChanged { id: tid, status }) = &ev
                && *tid == id
            {
                match status {
                    TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                    TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
                    _ => {}
                }
            }
        }
    };
    match timeout(deadline, wait).await {
        Ok(res) => res,
        Err(_) => Err(format!(
            "timeout {deadline:?} (last={:?})",
            queue.track(id).map(|e| e.status)
        )),
    }
}

pub(crate) async fn wait_for_position_at_least(
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<(), String> {
    let mut rx = queue.subscribe();
    await_progress(&mut rx, queue, |p| p >= min_secs, deadline)
        .await
        .map(|_| ())
        .map_err(|_| {
            format!(
                "position never reached {min_secs:.2}s in {deadline:?} (last={:?})",
                queue.position_seconds()
            )
        })
}
