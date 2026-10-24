//! Canonical state/event waits for the integration suite.
//!
//! Every state-wait funnels through [`wait_until`], whose single virtual
//! poll-tick `sleep` is the only timer `sleep` in the test surface. Under the
//! flash virtual clock that tick advances time between predicate checks, so a
//! wait resolves the instant the program reaches the asserted state — never on a
//! wall-clock guess. The `deadline` argument bounds only a non-progress watchdog
//! that returns `Err` (callers `.expect()` it); it is never an early-success
//! path that lets a later assertion pass on timeout.

use kithara::{
    audio::AudioEvent,
    bufpool::HasPool,
    events::{EventReceiver, TrackId},
    platform::{
        thread::active_named_thread_count,
        time::{Duration, Instant, sleep, timeout},
        tokio::sync::broadcast::error::RecvError,
    },
    queue::{QueueControl, QueueEvent, TrackStatus},
};
use kithara_test_utils::{wait::POLL_TICK, wait_until};

use crate::{event::TestEvent, offline::OfflinePlayer};

/// Settle window for [`wait_thread_count_quiesced`]: the named-thread counter
/// must hold steady across this many consecutive virtual ticks before it is
/// treated as quiesced.
const QUIESCE_TICKS: usize = 3;

async fn wait_for_queue_position<S>(
    queue: &QueueControl<S>,
    deadline: Duration,
    label: &str,
    mut matches: impl FnMut(f64) -> bool,
) -> Result<f64, ()>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let mut observed = None;
    wait_until(deadline, label, || {
        observed = queue
            .position_seconds()
            .filter(|&position| matches(position));
        observed.is_some()
    })
    .await
    .map_err(|_| ())?;
    observed.ok_or(())
}

/// Wait until `queue.position_seconds()` reports at least `min_secs`, then
/// return the observed position (seconds).
///
/// Use this when only a `&Queue` is in hand. When an [`EventReceiver<TestEvent>`] is
/// available, prefer [`wait_for_position_event`]: it reads sink-truth from
/// `AudioEvent::PlaybackProgress`, which (unlike the tick-cached
/// `position_seconds()`) does not go stale once an event-driven wait collapses
/// real time under flash. The returned position is the tick-cached value at
/// predicate-pass and is only meant for coarse "advanced past X" comparisons.
pub async fn wait_for_position_at_least<S>(
    queue: &QueueControl<S>,
    min_secs: f64,
    deadline: Duration,
) -> Result<f64, String>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    wait_for_queue_position(queue, deadline, "position_at_least", |position| {
        position >= min_secs
    })
    .await
    .map_err(|()| {
        format!(
            "position stayed below {min_secs:.2}s for {deadline:?} (last: {:?})",
            queue.position_seconds()
        )
    })
}

/// Wait until playback reaches `min_secs`, observed on sink-truth
/// `AudioEvent::PlaybackProgress` (the offline render worker emits one on every
/// committed PCM chunk). `recv()` parks on the virtual clock, so the engine
/// advances and position grows.
///
/// Returns the sink-truth position (seconds) observed at the moment the bound is
/// met. The bus event is the primary sink-truth signal; `position_seconds()` is
/// re-checked on the same virtual poll cadence so a dropped / delayed broadcast
/// receiver cannot turn already-reached playback state into a timeout.
pub async fn wait_for_position_event<S>(
    rx: &mut EventReceiver<TestEvent>,
    queue: &QueueControl<S>,
    min_secs: f64,
    deadline: Duration,
) -> Result<f64, String>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    if let Some(pos) = queue.position_seconds()
        && pos >= min_secs
    {
        return Ok(pos);
    }
    let min_ms = (min_secs * 1000.0) as u64;
    timeout(deadline, async {
        loop {
            if let Some(pos) = queue.position_seconds()
                && pos >= min_secs
            {
                return Ok(pos);
            }
            match timeout(POLL_TICK, rx.recv())
                .await
                .map(|r| r.map(|env| env.event))
            {
                Ok(Ok(TestEvent::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))) => {
                    if position_ms >= min_ms {
                        return Ok(position_ms as f64 / 1000.0);
                    }
                }
                Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) | Err(_) => {}
                Ok(Err(RecvError::Closed)) => return Err("event stream closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| {
        format!(
            "position never reached {min_secs:.2}s in {deadline:?} (last={:?})",
            queue.position_seconds()
        )
    })?
}

/// Wait until `queue.position_seconds()` is within `tolerance` of `target`, then
/// return the observed position. Queue-poll variant; prefer
/// [`wait_for_position_near_event`] when an [`EventReceiver<TestEvent>`] is available.
pub async fn wait_for_position_near<S>(
    queue: &QueueControl<S>,
    target: f64,
    tolerance: f64,
    deadline: Duration,
) -> Result<f64, String>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    wait_for_queue_position(queue, deadline, "position_near", |position| {
        (position - target).abs() < tolerance
    })
    .await
    .map_err(|()| {
        format!(
            "position never reached {target:.2}s (±{tolerance:.2}) in {deadline:?} (last: {:?})",
            queue.position_seconds()
        )
    })
}

/// TestEvent-driven [`wait_for_position_near`]: resolves the moment sink-truth
/// `PlaybackProgress` or `SeekComplete` lands within `tolerance` of `target`,
/// returning that position.
///
/// Only an event resolves it. `Queue::seek` writes its target into the queue's
/// position cache optimistically, before the render thread drains the command,
/// so consulting that cache after a seek answers with the position that was
/// *asked for* — instantly, having consumed nothing. A caller that then treats
/// the next event as its post-seek anchor gets the block that was already in
/// flight, carrying the pre-seek playhead. That is not a delayed receiver
/// hiding an already-near position; it is the cache being unable to say
/// whether the seek has landed, so there is nothing here for a cache read to
/// fall back to.
pub async fn wait_for_position_near_event<S>(
    rx: &mut EventReceiver<TestEvent>,
    queue: &QueueControl<S>,
    target: f64,
    tolerance: f64,
    deadline: Duration,
) -> Result<f64, String>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    timeout(deadline, async {
        loop {
            match timeout(POLL_TICK, rx.recv())
                .await
                .map(|r| r.map(|env| env.event))
            {
                Ok(Ok(TestEvent::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))) => {
                    let pos = position_ms as f64 / 1000.0;
                    let distance = (pos - target).abs();
                    if distance < tolerance {
                        return Ok(pos);
                    }
                }
                Ok(Ok(TestEvent::Audio(AudioEvent::SeekComplete { position, .. }))) => {
                    let pos = position.as_secs_f64();
                    if (pos - target).abs() < tolerance {
                        return Ok(pos);
                    }
                }
                Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) | Err(_) => {}
                Ok(Err(RecvError::Closed)) => return Err("event stream closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| {
        format!(
            "position never reached {target:.2}s (±{tolerance:.2}) in {deadline:?} (last: {:?})",
            queue.position_seconds()
        )
    })?
}

/// Wait for a matching event within `deadline`; lag is tolerated and a closed
/// bus returns an error.
#[kithara::flash(true)]
pub async fn wait_for_event(
    rx: &mut EventReceiver<TestEvent>,
    what: &str,
    mut pred: impl FnMut(&TestEvent) -> bool,
    deadline: Duration,
) -> Result<(), String> {
    timeout(deadline, async {
        loop {
            let received = rx.recv().await;
            match received {
                Ok(ev) if pred(&ev.event) => {
                    return Ok(());
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err(format!("event bus closed before {what}")),
            }
        }
    })
    .await
    .map_err(|_| format!("{what} never observed within {deadline:?}"))?
}

/// Classify a `TrackStatus` as loader-terminal.
fn loader_outcome(status: &TrackStatus) -> Option<Result<(), String>> {
    match status {
        TrackStatus::Loaded | TrackStatus::Consumed => Some(Ok(())),
        TrackStatus::Failed(err) => Some(Err(format!("track entered Failed: {err}"))),
        TrackStatus::Pending
        | TrackStatus::Loading
        | TrackStatus::Slow
        | TrackStatus::Cancelled => None,
    }
}

/// Wait until `track_id`'s loader finishes (`Loaded`/`Consumed`) by polling the
/// queue's track status. `Failed` resolves to `Err`. Use the event variant
/// [`wait_for_loader_done_event`] when a receiver is available.
pub async fn wait_for_loader_done<S>(
    queue: &QueueControl<S>,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let mut outcome: Result<(), String> = Ok(());
    let reached = wait_until(deadline, "loader_done", || {
        if let Some(entry) = queue.track(track_id)
            && let Some(res) = loader_outcome(&entry.status)
        {
            outcome = res;
            return true;
        }
        false
    })
    .await;
    match reached {
        Ok(()) => outcome,
        Err(_) => Err(format!(
            "loader timeout after {deadline:?} (last: {:?})",
            queue.track(track_id).map(|e| e.status)
        )),
    }
}

/// Wait for the requested loader's terminal status within `deadline`.
/// Loading includes real I/O and need not publish intermediate status changes.
#[kithara::flash(io)]
pub async fn wait_for_loader_done_event<S>(
    rx: &mut EventReceiver<TestEvent>,
    queue: &QueueControl<S>,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    if let Some(entry) = queue.track(track_id)
        && let Some(res) = loader_outcome(&entry.status)
    {
        return res;
    }
    timeout(deadline, async {
        loop {
            if let Some(entry) = queue.track(track_id)
                && let Some(res) = loader_outcome(&entry.status)
            {
                return res;
            }
            let Ok(received) = timeout(POLL_TICK, rx.recv()).await else {
                continue;
            };
            match received.map(|env| env.event) {
                Ok(TestEvent::Queue(QueueEvent::TrackStatusChanged { id, status }))
                    if id == track_id =>
                {
                    if let Some(res) = loader_outcome(&status) {
                        return res;
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| {
        format!(
            "loader timeout after {deadline:?} (last status: {:?})",
            queue.track(track_id).map(|e| e.status)
        )
    })?
}

/// Wait until the named-thread counter goes quiescent — i.e.
/// `active_named_thread_count()` holds steady across [`QUIESCE_TICKS`]
/// consecutive polls — then return the settled value.
///
/// This waits on the real teardown state (the same counter every leak assertion
/// reads): the count is decremented when each spawned thread's closure RETURNS
/// after shutdown/cancel, which is observable on the wall clock. A leak
/// keeps the count high — it still stabilizes high, so the assertion still
/// catches the growth. The watchdog PANICS on genuine non-quiescence rather than
/// returning a mid-teardown reading that an assertion could pass against.
#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
pub async fn wait_thread_count_quiesced(deadline: Duration) -> usize {
    let watchdog = Instant::now() + deadline;
    let mut last = active_named_thread_count();
    let mut stable = 0usize;
    loop {
        hang_tick!(format!(
            "waiting for named threads to quiesce: last={last}, stable={stable}"
        ));
        sleep(POLL_TICK).await;
        let now = active_named_thread_count();
        if now == last {
            stable += 1;
            hang_reset!();
            if stable >= QUIESCE_TICKS {
                return now;
            }
        } else {
            stable = 0;
            last = now;
        }
        assert!(
            Instant::now() < watchdog,
            "named-thread count never quiesced: last={now} \
             (active_named_thread_count kept changing for {deadline:?})"
        );
    }
}

/// Render `player` in batches until `player.position()` reaches
/// `until_position` AND at least `max_blocks` blocks have been rendered, then
/// return.
///
/// State-driven: the success gate is the produced position, not elapsed time.
/// `min_wall_ms` only sizes a non-progress watchdog that PANICS — it is never an
/// early-return path that lets a later assert pass on timeout. Callers pass
/// `until_position = pre_pos + target_advance` for warmup and
/// `until_position = seek_target + min_advance` for post-seek to discriminate a
/// seek jump from render-driven progress.
#[kithara::flash(true)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
pub async fn render_until_position(
    player: &mut OfflinePlayer,
    max_blocks: u32,
    until_position: f64,
    block_frames: usize,
    min_wall_ms: u64,
) {
    const BATCH: u32 = 16;
    const TICK_MS: u64 = 25;
    // Safety net only: bound consecutive no-progress ticks so a genuinely wedged
    // pipeline panics with a clear message instead of spinning to the outer test
    // timeout. A healthy run reaches `until_position` and returns long before
    // this trips. Derived from the caller's intended budget so it scales with
    // the per-segment fetch window, never shorter than a floor.
    let max_stall_ticks = (min_wall_ms / TICK_MS).max(40) + 40;
    let mut rendered = 0u32;
    let mut last_position = player.position();
    let mut stall_ticks: u64 = 0;
    loop {
        hang_tick!(format!(
            "rendered {rendered}/{max_blocks} blocks; position={last_position}, target={until_position}"
        ));
        let this = max_blocks.saturating_sub(rendered).clamp(1, BATCH);
        for _ in 0..this {
            hang_tick!(format!("rendering a block toward {until_position}"));
            drop(player.render(block_frames).await);
        }
        rendered = rendered.saturating_add(this);
        let position = player.position();
        if position >= until_position && rendered >= max_blocks {
            return;
        }
        if position > last_position {
            hang_reset!();
            last_position = position;
            stall_ticks = 0;
        } else {
            stall_ticks = stall_ticks.saturating_add(1);
            assert!(
                stall_ticks <= max_stall_ticks,
                "render_until_position stalled: position {position:.3}s did not \
                 reach {until_position:.3}s after {max_stall_ticks} no-progress \
                 ticks (rendered {rendered}/{max_blocks} blocks)"
            );
        }
        sleep(Duration::from_millis(TICK_MS)).await;
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara::{events::EventBus, platform::time::WallInstant};

    use super::*;
    use crate::{HlsFixtureBuilder, TestServerHelper, offline::OfflinePlayerOptions};

    #[cfg(feature = "flash")]
    #[kithara::test(tokio, multi_thread, flash(true))]
    async fn modeled_render_progress_does_not_turn_fixture_delay_into_wall_time() {
        use std::num::{NonZeroU32, NonZeroUsize};

        use kithara::{
            assets::{AssetStore, StorageBackend},
            host::{HostConfig, HostSettings},
            platform::time::Instant,
            play::{PlayWorker, PlayWorkerConfig, ResourceConfig, ResourceSrc},
        };
        use kithara_test_utils::bufpool::pools;

        use crate::fixture_protocol::DelayRule;

        const RATE: u32 = 44_100;
        const BLOCK: usize = 512;
        let server = TestServerHelper::new().await;
        let fixture = server
            .create_hls(
                HlsFixtureBuilder::new()
                    .variant_count(1)
                    .segments_per_variant(4)
                    .segment_duration_secs(1.0)
                    .packaged_audio_aac_lc(RATE, 2)
                    .push_delay_rule(DelayRule {
                        variant: Some(0),
                        segment_eq: Some(1),
                        delay_ms: 2_000,
                        ..DelayRule::default()
                    }),
            )
            .await
            .expect("modeled render fixture");
        let gate = server.register_segment_gate(fixture.token(), 0, 1);
        let region = pools();
        let resource = ResourceConfig::for_src(ResourceSrc::Url(fixture.master_url()))
            .store(
                AssetStore::builder(region.clone())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .downloader(server.modeled_downloader())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(region.clone()).build(),
            ))
            .preload_chunks(NonZeroUsize::MIN)
            .build();
        let mut player = OfflinePlayer::with_options(
            OfflinePlayerOptions::builder()
                .block_on_underrun(true)
                .build(),
            HostConfig::offline(region)
                .settings(
                    HostSettings::builder()
                        .sample_rate(NonZeroU32::new(RATE).expect("fixture rate"))
                        .build(),
                )
                .build(),
        )
        .await;
        player.load_config(resource).await;
        let start = Instant::now();
        let wall = WallInstant::now();
        {
            let mut waiting =
                std::pin::pin!(render_until_position(&mut player, 20, 1.25, BLOCK, 5_000));
            assert!(futures::poll!(waiting.as_mut()).is_pending());
            gate.release();
            waiting.await;
        }
        assert!(gate.requested() > 0);
        assert!(player.position() >= 1.25);
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert!(wall.elapsed() < Duration::from_secs(1));
        player.close().await;
    }

    #[cfg(feature = "flash")]
    #[kithara::test(tokio, multi_thread, flash(true))]
    async fn modeled_event_wait_does_not_turn_fixture_delay_into_wall_time() {
        use kithara::{
            net::NetOptions,
            platform::{CancelToken, time::Instant, tokio::task::spawn},
        };

        use crate::fixture_protocol::DelayRule;

        let helper = TestServerHelper::new().await;
        let fixture = helper
            .create_hls(
                HlsFixtureBuilder::new()
                    .segment_size(2_048)
                    .push_delay_rule(DelayRule {
                        delay_ms: 2_000,
                        ..DelayRule::default()
                    }),
            )
            .await
            .expect("modeled event fixture");
        let client = helper.modeled_client(NetOptions::default(), CancelToken::never());
        let url = fixture.segment_url(0, 0);
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let id = TrackId::allocate();
        let start = Instant::now();
        let wall = WallInstant::now();
        let request = spawn(async move {
            let bytes = client
                .get_bytes(url, None)
                .await
                .expect("modeled segment request");
            assert_eq!(bytes.len(), 2_048);
            bus.publish(QueueEvent::TrackStatusChanged {
                id,
                status: TrackStatus::Loaded,
            });
        });
        wait_for_event(&mut events, "modeled request completion", |event| matches!(event,
            TestEvent::Queue(QueueEvent::TrackStatusChanged { id: found, status: TrackStatus::Loaded }) if *found == id),
            Duration::from_secs(5)).await.expect("modeled completion event");
        request.await.expect("modeled request task");
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert!(wall.elapsed() < Duration::from_secs(1));
    }

    #[kithara::test(tokio)]
    async fn loader_completion_wins_when_its_notification_is_missing() {
        let helper = TestServerHelper::new().await;
        let fixture = helper
            .create_hls(
                HlsFixtureBuilder::new()
                    .variant_count(1)
                    .segments_per_variant(4)
                    .segment_duration_secs(1.0)
                    .packaged_audio_aac_lc(44_100, 2),
            )
            .await
            .expect("HLS fixture");
        let init = helper.register_init_gate(fixture.token(), 0);
        let harness =
            OfflinePlayer::with_sample_rate(OfflinePlayerOptions::builder().build(), 44_100).await;
        let url = fixture.master_url().to_string();
        let id = harness
            .with_queue(move |queue| {
                let id = queue.append(url).expect("append gated track");
                queue
                    .select(id, kithara::queue::Transition::None)
                    .expect("select gated track");
                id
            })
            .await;
        assert!(matches!(
            harness.player().track(id).expect("queued track").status,
            TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow
        ));

        {
            let unrelated_bus = EventBus::new(4);
            let mut receiver = unrelated_bus.subscribe();
            let mut waiting = std::pin::pin!(wait_for_loader_done_event(
                &mut receiver,
                harness.player(),
                id,
                Duration::from_secs(2),
            ));
            assert!(futures::poll!(waiting.as_mut()).is_pending());
            init.release();
            harness
                .render_until_current(id, 512, WallInstant::now() + Duration::from_secs(1))
                .await;
            wait_for_loader_done(harness.player(), id, Duration::from_secs(1))
                .await
                .expect("the authoritative loader state completes");
            waiting
                .await
                .expect("a completed load needs no notification");
        }
        harness.close().await;
    }
}
