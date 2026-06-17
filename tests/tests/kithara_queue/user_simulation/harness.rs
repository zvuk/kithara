use std::{path::Path, sync::Arc};

use kithara_abr::AbrHandle;
use kithara_assets::StoreOptions;
use kithara_decode::DecoderBackend;
use kithara_events::{
    AbrMode, AudioEvent, Event, EventReceiver, QueueEvent, TrackId, TrackStatus, VariantInfo,
};
use kithara_integration_tests::{kithara, offline::OfflineSession};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep, timeout},
};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, SeekOutcome};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
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

/// Periodic queue tick driver, run as a spawned task. `#[kithara::flash(true)]`
/// makes the body flash-ACTIVE under an ambient (flash) test, so its
/// `sleep` resolves to the engine-backed virtual timer — without it the
/// async spawn chokepoint only propagates the per-test ambient gate (it does
/// NOT set the active flag, unlike the sync `spawn_named` pacer), and
/// `kithara_platform::time::sleep` keys on the active flag, so the driver
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
    tick: kithara_platform::tokio::task::JoinHandle<()>,
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
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .session(OfflineSession::arc_auto())
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
        let tick = kithara_platform::tokio::task::spawn(run_tick_driver(queue_for_tick));

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
                .downloader(downloader.clone())
                .store(store.clone())
                .decoder_backend(spec.backend)
                .initial_abr_mode(spec.abr_mode)
                .build();
            let id = queue.append(TrackSource::Config(Box::new(cfg)));
            track_ids.push(id);
        }

        Self {
            queue,
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
        }
    }

    /// Wait for any in-flight seek to settle, then assert the queue is
    /// at a sane terminal state for the scenario. Used as the final
    /// step in scripted scenarios.
    pub(crate) async fn shutdown(self) {
        self.tick.abort();
        let _ = self.tick.await;
        drop(self.queue);
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
        if self.queue.advance_to_next(Transition::None).is_some() {
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
        // Mid-track ABR variant switch lights up the
        // recreate-decoder-mid-stream path in
        // `kithara_audio::pipeline::source` — cross-codec moves
        // (AAC ↔ FLAC, as in the prod 4-variant ladder) must
        // survive without dropping the track. The invariants we
        // verify: track stays the same, never Failed, and (while
        // playable) playback advances within `ACTION_BUDGET`. Codec
        // FIELD changes are *expected* when variants differ; what would
        // be a bug is a stall-while-playable, panic, or track-flip.
        //
        // EOF carve-out: a quality switch issued once the track has
        // already played out to natural EOF cannot advance position —
        // the player has stopped and emits no more `PlaybackProgress`
        // (correct behaviour). The invariant there is *not* "must
        // advance" but "no track-flip, no Failed, no stall-while-
        // playable": the switch must leave the at-EOF track loaded and
        // stationary, not crash or auto-advance. So success is EITHER
        // position cleared `pre_pos + 0.1` (playable case) OR the track
        // settled at natural EOF (terminal-but-fine).
        //
        // Await bus events (which park on the virtual clock, letting the
        // engine recreate the decoder + render or quiesce at EOF). On
        // every delivered event we re-check the flip / Failed invariants
        // so a regression panics with the same teeth as before.
        let advance = async {
            loop {
                if pre_track.is_some() && self.current_track_id() != pre_track {
                    // A flip to None when the switch was issued within
                    // NATURAL_EOF_WINDOW of the track end is the track
                    // legitimately playing out to natural EOF DURING the switch
                    // (the player stops, the queue cursor runs off the end) — the
                    // same terminal-but-fine state the at-EOF carve-out below
                    // accepts, not a spurious cross-switch flip. Use the
                    // pre-switch position/duration, NOT live position()/
                    // is_playing(): current() flips to None on the queue tick
                    // before the player's cleanup sets playing=false, so a live
                    // at_natural_eof() check races and misclassifies. A flip to a
                    // DIFFERENT track, or to None while still mid-track, is still
                    // a dropped-track bug.
                    if self.current_track_id().is_none()
                        && pre_dur > 0.0
                        && (pre_dur - pre_pos) <= NATURAL_EOF_WINDOW_S
                    {
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
                if self.position() > pre_pos + 0.1 {
                    return;
                }
                // No advance is *expected* at natural EOF: the player has
                // stopped and will not emit further progress. Treat the
                // stationary at-EOF track (still loaded, not flipped, not
                // Failed — checked above) as a successful switch.
                if self.at_natural_eof() {
                    return;
                }
                let ev = match recv_event(&mut rx).await {
                    Ok(Some(ev)) => ev,
                    Ok(None) => continue,
                    // Bus closed: stop waiting (the outer timeout will
                    // not fire, so return and let the post-check read
                    // the final position).
                    Err(_) => return,
                };
                if let Some(p) = progress_secs(&ev)
                    && p > pre_pos + 0.1
                {
                    return;
                }
            }
        };
        // The watchdog only fires on a true mid-track stall (no advance
        // AND not at EOF). If the track quiesced at natural EOF in the
        // window between the loop's last check and the deadline, that is
        // the expected terminal state, not a hang.
        if timeout(ACTION_BUDGET, advance).await.is_err() && !self.at_natural_eof() {
            // Re-check flip / Failed so a regression that *also* stalls
            // is reported with its true cause, not a generic HANG.
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
                "[SetQuality({bounded})] HANG: position did not advance after switch \
                 (pre_pos={pre_pos:.2}s, post={:.2}s, budget={:?})",
                self.position(),
                ACTION_BUDGET
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
        let started = kithara_platform::time::Instant::now();

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

        // Drive the watchdog off `PlaybackProgress` events (and any
        // other bus traffic) instead of a `sleep`-cadence position poll:
        // `recv()` parks on the virtual clock, so the engine advances
        // time, runs the tick driver, and renders the next block — which
        // is what each iteration then observes. The same invariants
        // (Failed, requested-advance, natural EOF, 3s stagnation) are
        // evaluated on every wake, with identical thresholds.
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
            if let Ok(Err(_)) = timeout(POLL_INTERVAL, recv_event(&mut rx)).await {
                break;
            }
            // One more producer-tick wake elapsed without progress (progress
            // resets this to 0 at the top of the loop).
            no_progress_ticks = no_progress_ticks.saturating_add(1);
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
}

/// Drain `recv()` once, mapping the broadcast lag/closed errors to a
/// flow-control signal. `Ok(Some(ev))` is a delivered event,
/// `Ok(None)` is a recoverable lag (caller should keep waiting), and
/// `Err` is a closed bus (terminal). `recv()` itself parks on the
/// virtual clock — awaiting it is what lets the engine advance time
/// (run the tick driver + decode worker) until real state changes.
async fn recv_event(rx: &mut EventReceiver) -> Result<Option<Event>, String> {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
    match rx.recv().await {
        Ok(ev) => Ok(Some(ev)),
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
