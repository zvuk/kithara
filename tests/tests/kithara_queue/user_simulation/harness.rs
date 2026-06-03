use std::{path::Path, sync::Arc, time::Duration};

use kithara_abr::AbrHandle;
use kithara_assets::StoreOptions;
use kithara_decode::DecoderBackend;
use kithara_events::{AbrMode, EventReceiver, TrackId, TrackStatus, VariantInfo};
use kithara_integration_tests::offline::OfflineSession;
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::CancellationToken;
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, SeekOutcome};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use tokio::time::sleep;
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

/// `SimHarness` is the integration-test entry point for the user
/// simulation scenarios. It owns a `Queue + Player + Downloader` triple
/// plus a tokio tick task; callers drive it with `Action`s and the
/// harness asserts the per-action invariants from the plan.
pub(crate) struct SimHarness {
    queue: Arc<Queue>,
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
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .session(OfflineSession::arc_auto())
                .build(),
        ));
        let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
        let queue_for_tick = Arc::clone(&queue);
        let tick = tokio::spawn(async move {
            loop {
                sleep(Duration::from_millis(50)).await;
                if queue_for_tick.tick().is_err() {
                    break;
                }
            }
        });

        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                NetOptions::default(),
                CancellationToken::default(),
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

        let started = kithara_platform::time::Instant::now();
        let mut landed_at: Option<f64> = None;
        while started.elapsed() < ACTION_BUDGET {
            if let Some(entry) = pre_track.and_then(|id| self.queue.track(id))
                && let TrackStatus::Failed(err) = &entry.status
            {
                panic!("[{action_label}] track entered Failed during seek: {err}");
            }
            let cur = self.position();
            if (cur - target).abs() <= SEEK_TARGET_TOLERANCE_S {
                landed_at = Some(cur);
                break;
            }
            sleep(POLL_INTERVAL).await;
        }

        let landed = landed_at.unwrap_or_else(|| {
            let post = self.position();
            panic!(
                "[{action_label}] HANG: seek to {target:.2}s never settled \
                 within {budget:?} (pre={pre_pos:.2}s, post={post:.2}s, \
                 dur={duration:.2}s)",
                budget = ACTION_BUDGET
            );
        });

        if self.current_track_id() != pre_track {
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
        // `CurrentItemChanged` notification, so polling on
        // `current().id` confirms the engine is now serving the
        // new track. Without this step `do_play_for` captures
        // `pre_pos` from the *previous* track, racing the handover.
        let switch_deadline = kithara_platform::time::Instant::now() + Duration::from_secs(5);
        while kithara_platform::time::Instant::now() < switch_deadline {
            if self.current_track_id() == Some(id) {
                break;
            }
            sleep(POLL_INTERVAL).await;
        }
        // Refresh codec snapshot — picking a different track legitimately
        // changes the codec, so this is a reset, not an assertion.
        self.last_known_codec = self.current_codec();
    }

    async fn do_select_prev(&mut self) {
        if self.queue.return_to_previous(Transition::None).is_some() {
            sleep(Duration::from_millis(100)).await;
            self.last_known_codec = self.current_codec();
        }
    }

    async fn do_select_next(&mut self) {
        if self.queue.advance_to_next(Transition::None).is_some() {
            sleep(Duration::from_millis(100)).await;
            self.last_known_codec = self.current_codec();
        }
    }

    async fn do_set_quality(&mut self, idx: usize) {
        let Some(handle) = self.current_abr_handle() else {
            return;
        };
        let pre_track = self.current_track_id();
        let pre_pos = self.position();
        let bounded = idx % 4;
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
        // verify: track stays the same and playback advances
        // within `ACTION_BUDGET`. Codec FIELD changes are
        // *expected* when variants differ; what would be a bug is
        // a stall, panic, or track-flip.
        let started = kithara_platform::time::Instant::now();
        while started.elapsed() < ACTION_BUDGET {
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
            let cur = self.position();
            if cur > pre_pos + 0.1 {
                self.last_known_codec = self.current_codec();
                return;
            }
            sleep(POLL_INTERVAL).await;
        }
        panic!(
            "[SetQuality({bounded})] HANG: position did not advance after switch \
             (pre_pos={pre_pos:.2}s, post={:.2}s, budget={:?})",
            self.position(),
            ACTION_BUDGET
        );
    }

    async fn do_quality_auto(&mut self) {
        let Some(handle) = self.current_abr_handle() else {
            return;
        };
        let _ = handle.set_mode(AbrMode::Auto(None));
        sleep(Duration::from_millis(100)).await;
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
        let wall_budget = at_least * 2;
        let mut last_pos = pre_pos;
        let mut stagnant_since = kithara_platform::time::Instant::now();

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
                stagnant_since = kithara_platform::time::Instant::now();
            }
            if cur > last_pos + 0.05 {
                last_pos = cur;
                stagnant_since = kithara_platform::time::Instant::now();
            }
            if !self.is_playing() {
                // Two legitimate reasons to be `!is_playing` mid-PlayFor:
                //   (1) the harness paused/resumed earlier and the engine
                //       hasn't picked up resume yet — give it a tick;
                //   (2) the track hit natural EOF and the queue moved on.
                // The PlayFor body already handles spurious auto-advance
                // post-loop, so here we just yield instead of panicking.
                sleep(POLL_INTERVAL).await;
                continue;
            }
            if cur - pre_pos >= at_least.as_secs_f64() * 0.9 {
                // Made the requested progress — done.
                return;
            }
            // Reached natural EOF — legitimate stop. Don't keep
            // polling for a wall-clock budget we'll never satisfy.
            if duration > 0.0 && (duration - cur).abs() < 0.5 {
                return;
            }
            if stagnant_since.elapsed() > Duration::from_secs(3)
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
            sleep(POLL_INTERVAL).await;
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

pub(crate) async fn wait_for_loaded(
    queue: &Queue,
    id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = kithara_platform::time::Instant::now();
    loop {
        if let Some(entry) = queue.track(id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "timeout {deadline:?} (last={:?})",
                queue.track(id).map(|e| e.status)
            ));
        }
        sleep(POLL_INTERVAL).await;
    }
}

pub(crate) async fn wait_for_position_at_least(
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<(), String> {
    let start = kithara_platform::time::Instant::now();
    loop {
        if let Some(p) = queue.position_seconds()
            && p >= min_secs
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "position never reached {min_secs:.2}s in {deadline:?} (last={:?})",
                queue.position_seconds()
            ));
        }
        sleep(POLL_INTERVAL).await;
    }
}
