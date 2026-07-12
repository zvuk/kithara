#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! The queue contract under test: **superseding a still-loading selection
//! cancels it so it never plays.** When `select(slow)` is followed by
//! `select(fast)` while `slow` is still constructing, the supersede path marks
//! `slow` [`TrackStatus::Cancelled`]; its loader completion must then skip
//! `replace_item`, and auto-advance must skip the cancelled entry — so `slow`
//! never becomes `current()`, not even after `fast` plays out.
//!
//! Determinism: the supersede premise is gated, not timed. An **init gate**
//! (`EXT-X-MAP` body withhold) parks `slow`'s off-RT blocking construction read
//! (the initial-decoder build inside `Resource::new` reads the container header
//! from the init body), holding `slow`'s loader in `Loading` until the test
//! releases it — so the test drives `select(slow)`/`select(fast)` while `slow`
//! is provably still constructing. There is no reliance on wall-clock
//! construction speed: the older `select(slow)` → `sleep(50ms)` →
//! `select(fast)` window was a lottery, because the slow fixture's per-segment
//! delay only gates media-segment bodies fetched at *playback*, while
//! construction (playlists + size HEADs + the init/header read) is otherwise
//! undelayed — so `slow` routinely reached `Loaded` before the second select,
//! leaving the supersede path untaken.

use kithara::{
    assets::StoreOptions,
    events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep},
        tokio,
        tokio::sync::broadcast::error::RecvError,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, InitGateHandle, TestServerHelper, TestTempDir, kithara,
    offline::OfflineSession, temp_dir,
};
use url::Url;

struct Consts;
impl Consts {
    /// Number of repetitions inside the concurrent-completion stress case.
    /// Pre-`select_apply`-lock this barged in; kept high for catch rate.
    const STRESS_ITERATIONS: u32 = 30;
    /// `fast` fixture: 1 segment × 2 s = 2 s. Short enough that it plays to
    /// its natural end inside the observation window, opening the
    /// auto-advance gate so the test exercises the "after fast plays out"
    /// branch.
    const FAST_SEGMENT_COUNT: usize = 1;
    const FAST_SEGMENT_DURATION_S: f64 = 2.0;
    /// `slow` fixture: several segments so it cannot finish during the
    /// observation window once released.
    const SLOW_SEGMENT_COUNT: usize = 4;
    const SLOW_SEGMENT_DURATION_S: f64 = 4.0;
    /// The single variant of each fixture; its init segment is what the gate
    /// withholds.
    const VARIANT: usize = 0;
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(30);
    /// Deadline for an observable to reach an expected value (init GET
    /// arrival, status transition, current-id change). A real stall trips
    /// this rather than being masked.
    const OBSERVE_DEADLINE: Duration = Duration::from_secs(10);
    /// Hard cap on the post-`fast` current-track event watch — the window in
    /// which an unauthorised flip to `slow` would surface as
    /// `CurrentTrackChanged { id: Some(slow_id) }`. Exceeds
    /// `FAST_SEGMENT_DURATION_S` so `fast`'s natural end (and the auto-advance
    /// that must skip the cancelled `slow`) fires inside it.
    const POST_FAST_OBSERVE: Duration = Duration::from_secs(5);
    /// Polling interval for the init-gate counter observable
    /// (`wait_for_init_requested`).
    const STATUS_POLL: Duration = Duration::from_millis(5);
    /// Gap between `select(slow)` and `select(fast)` in the completion-race
    /// test: long enough for slow's loader completion to fire around the
    /// superseding select (the contended window), short relative to
    /// `RACE_OBSERVE`. A deliberate race-widening pacing delay with no
    /// state-wait equivalent — synchronising on a settled state would dissolve
    /// the contended window (see the test's own note).
    const RACE_GAP: Duration = Duration::from_millis(50);
    /// Window after the selects during which the completion-race test watches
    /// for a barge-in (slow stomping current fast). Ample for the racing
    /// completion's `select_item` to land (~tens of ms when it does).
    const RACE_OBSERVE: Duration = Duration::from_secs(1);
}

/// Build a single-variant HLS fixture and return its [`CreatedHls`] handle so
/// the caller can register an init gate against its token. No per-segment
/// delay: the gate, not wall-clock timing, controls loader readiness.
async fn build_hls(
    helper: &TestServerHelper,
    segment_count: usize,
    segment_duration_s: f64,
) -> CreatedHls {
    helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(segment_count)
                .segment_duration_secs(segment_duration_s)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create local HLS fixture")
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Downloader,
    StoreOptions,
    tokio::task::JoinHandle<()>,
) {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(run_tick_driver(queue_for_tick));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store, tick_handle)
}

#[kithara::flash(true)]
async fn run_tick_driver(queue: Arc<Queue>) {
    loop {
        sleep(Duration::from_millis(50)).await;
        if queue.tick().is_err() {
            break;
        }
    }
}

/// Build a queue WITHOUT the auto-advance tick loop. Auto-advance is driven
/// solely by `Queue::tick` (the player's own auto-advance is disabled in
/// `Queue::new`), so omitting the tick task means a track can only become
/// `current()` via an explicit `select` or a loader-completion's
/// `select_item` — never via end-of-track auto-advance. The completion-race
/// test relies on this to isolate a barge-in (slow stomping the current fast)
/// from the legitimate end-of-`fast` auto-advance to the next queue entry.
fn build_queue_no_tick(temp_dir: &TestTempDir) -> (Arc<Queue>, Downloader, StoreOptions) {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store)
}

/// Await the next [`QueueEvent`] matching `pred` on `rx`, bounded by
/// `deadline` as a hard safety cap. Returns the matched event, or `None` on
/// timeout / bus closure. Lagged is non-fatal (the per-test bus has ample
/// capacity); we re-loop to keep draining, mirroring the sibling-test idiom.
/// The helper is flash-active so deadlines are virtual in flash runs; otherwise
/// every absence window would consume real wall time.
#[kithara::flash(true)]
async fn next_queue_event<F>(
    rx: &mut EventReceiver,
    deadline: Duration,
    mut pred: F,
) -> Option<QueueEvent>
where
    F: FnMut(&QueueEvent) -> bool,
{
    let start = kithara::platform::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return None;
        }
        match kithara::platform::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(Event::Queue(ev))) if pred(&ev) => return Some(ev),
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) | Err(_) => return None,
        }
    }
}

/// Wait until `track_id` reaches a terminal loaded state, driven by
/// [`QueueEvent::TrackStatusChanged`]. Subscribes first, then snapshots the
/// current status (so an already-terminal track returns immediately), then
/// blocks on the event stream — no polling.
async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let mut rx = queue.subscribe();
    if let Some(entry) = queue.track(track_id) {
        match &entry.status {
            TrackStatus::Loaded | TrackStatus::Consumed | TrackStatus::Cancelled => {
                return Ok(());
            }
            TrackStatus::Failed(err) => return Err(format!("track entered Failed: {err}")),
            _ => {}
        }
    }
    let matched = next_queue_event(&mut rx, deadline, |ev| {
        matches!(
            ev,
            QueueEvent::TrackStatusChanged { id, status }
                if *id == track_id
                    && matches!(
                        status,
                        TrackStatus::Loaded
                            | TrackStatus::Consumed
                            | TrackStatus::Cancelled
                            | TrackStatus::Failed(_)
                    )
        )
    })
    .await;
    match matched {
        Some(QueueEvent::TrackStatusChanged {
            status: TrackStatus::Failed(err),
            ..
        }) => Err(format!("track entered Failed: {err}")),
        Some(_) => Ok(()),
        None => Err(format!(
            "timeout after {deadline:?} (last status: {:?})",
            queue.track(track_id).map(|e| e.status)
        )),
    }
}

/// Wait until `expected` becomes `current()`, driven by
/// [`QueueEvent::CurrentTrackChanged`]. Subscribes first, snapshots
/// `current()` (catches an already-current track), then blocks on the event.
async fn wait_for_current_id(
    queue: &Queue,
    expected: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let mut rx = queue.subscribe();
    if queue.current().map(|e| e.id) == Some(expected) {
        return Ok(());
    }
    let matched = next_queue_event(
        &mut rx,
        deadline,
        |ev| matches!(ev, QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == expected),
    )
    .await;
    if matched.is_some() {
        Ok(())
    } else {
        Err(format!(
            "current never became {:?} within {deadline:?} (last={:?})",
            expected,
            queue.current().map(|e| e.id),
        ))
    }
}

/// Poll the gate's in-process `requested()` counter until the withheld init
/// GET has reached the server, under a hard deadline. This is the empirical
/// proof that the loader is parked on the gate (not merely slow), and
/// synchronizes on an observable rather than on wall-clock timing.
async fn wait_for_init_requested(gate: &InitGateHandle, deadline: Duration) -> Result<(), String> {
    let start = kithara::platform::time::Instant::now();
    loop {
        if gate.requested() >= 1 {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "init GET never reached the gate within {deadline:?} (requested={})",
                gate.requested()
            ));
        }
        sleep(Consts::STATUS_POLL).await;
    }
}

/// Wait until `track_id` reaches `expected`, driven by
/// [`QueueEvent::TrackStatusChanged`]. Subscribes first, snapshots the
/// current status (catches an already-applied transition), then blocks on the
/// event stream under `deadline` as a hard safety cap.
async fn wait_for_status(
    queue: &Queue,
    track_id: TrackId,
    expected: TrackStatus,
    deadline: Duration,
) -> Result<(), String> {
    let mut rx = queue.subscribe();
    if queue.track(track_id).map(|e| e.status) == Some(expected.clone()) {
        return Ok(());
    }
    let matched = next_queue_event(&mut rx, deadline, |ev| {
        matches!(
            ev,
            QueueEvent::TrackStatusChanged { id, status }
                if *id == track_id && *status == expected
        )
    })
    .await;
    if matched.is_some() {
        Ok(())
    } else {
        Err(format!(
            "status never became {expected:?} within {deadline:?} (last={:?})",
            queue.track(track_id).map(|e| e.status)
        ))
    }
}

/// Watch the queue's current-track event stream, asserting `current()` never
/// flips to `slow_id`. The success terminal (fast plays out, auto-advance
/// skips the cancelled slow, queue ends) is [`QueueEvent::QueueEnded`] or
/// [`QueueEvent::CurrentTrackChanged`] with `id: None`; the forbidden event is
/// `CurrentTrackChanged { id: Some(slow_id) }`. Bounded by
/// `Consts::POST_FAST_OBSERVE` as a hard cap — exhausting it without a
/// barge-in is itself the absence-over-the-window success. Caller must have
/// already confirmed `fast` is current, so the only future current-change is
/// the end-of-`fast` auto-advance this races.
async fn assert_no_barge_in(queue: &Queue, slow_id: TrackId) -> Result<(), String> {
    let mut rx = queue.subscribe();
    let terminal = next_queue_event(&mut rx, Consts::POST_FAST_OBSERVE, |ev| match ev {
        QueueEvent::QueueEnded | QueueEvent::CurrentTrackChanged { id: None } => true,
        QueueEvent::CurrentTrackChanged { id: Some(id) } => *id == slow_id,
        _ => false,
    })
    .await;
    match terminal {
        Some(QueueEvent::CurrentTrackChanged { id: Some(id) }) if id == slow_id => Err(format!(
            "slow_id barged in (CurrentTrackChanged {{ id: Some({slow_id:?}) }})"
        )),
        // QueueEnded / CurrentTrackChanged{None} = success; a None timeout
        // (no current-change at all within the window) is also no barge-in.
        _ => Ok(()),
    }
}

fn mk_cfg(url: &Url, downloader: &Downloader, store: &StoreOptions) -> ResourceConfig {
    ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .downloader(downloader.clone())
        .store(store.clone())
        .initial_abr_mode(AbrMode::Auto(None))
        .build()
}

/// Deterministic supersede-while-loading: gate `slow`'s init so it is provably
/// `Loading`, `select(slow)` then `select(fast)` (now deterministically taking
/// the supersede path → `slow` marked `Cancelled`), release `slow`'s init so
/// its loader completes and skips on the cancelled status, then assert `slow`
/// never becomes `current()` — including after `fast` plays out.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn supersede_while_loading_cancels_slow_track() {
    let helper = TestServerHelper::new().await;
    let fast = build_hls(
        &helper,
        Consts::FAST_SEGMENT_COUNT,
        Consts::FAST_SEGMENT_DURATION_S,
    )
    .await;
    let slow = build_hls(
        &helper,
        Consts::SLOW_SEGMENT_COUNT,
        Consts::SLOW_SEGMENT_DURATION_S,
    )
    .await;
    // Withhold slow's active-variant init body: holds its loader in `Loading`.
    let slow_init = helper.register_init_gate(slow.token(), Consts::VARIANT);
    let fast_url = fast.master_url();
    let slow_url = slow.master_url();

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let fast_id = queue.append(TrackSource::Config(Box::new(mk_cfg(
        &fast_url,
        &downloader,
        &store,
    ))));
    let slow_id = queue.append(TrackSource::Config(Box::new(mk_cfg(
        &slow_url,
        &downloader,
        &store,
    ))));

    // fast is undelayed and ungated → it reaches a terminal loaded state.
    wait_for_loader_done(&queue, fast_id, Consts::LOAD_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("fast load: {e}"));

    // EMPIRICAL PROOF the gate holds slow in a loading state: the init GET
    // reached the gate, and slow is NOT `Loaded` (it is parked, not merely
    // slow). Without this, the supersede path below would not be taken.
    wait_for_init_requested(&slow_init, Consts::OBSERVE_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("slow init gate: {e}"));
    let slow_status = queue.track(slow_id).map(|e| e.status);
    assert!(
        matches!(
            slow_status,
            Some(TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow)
        ),
        "init gate must hold slow in a loading state, got {slow_status:?}"
    );

    // select(slow): slow is loading → stashed as the pending selection.
    queue
        .select(slow_id, Transition::None)
        .expect("select slow");
    // select(fast): supersedes the pending slow selection → slow Cancelled.
    queue
        .select(fast_id, Transition::None)
        .expect("select fast");

    // Deterministic: the supersede marked slow Cancelled synchronously.
    wait_for_status(
        &queue,
        slow_id,
        TrackStatus::Cancelled,
        Consts::OBSERVE_DEADLINE,
    )
    .await
    .unwrap_or_else(|e| panic!("supersede must cancel slow: {e}"));

    // Release the init so slow's loader completes. Its `spawn_apply_after_load`
    // completion must observe the Cancelled status and skip `replace_item`, so
    // slow stays Cancelled (never Loaded → never plantable for handover).
    slow_init.release();
    wait_for_loader_done(&queue, slow_id, Consts::LOAD_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("slow load after release: {e}"));
    assert_eq!(
        queue.track(slow_id).map(|e| e.status),
        Some(TrackStatus::Cancelled),
        "cancelled-then-completed slow load must remain Cancelled, not flip to Loaded",
    );

    wait_for_current_id(&queue, fast_id, Consts::OBSERVE_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("fast never became current: {e}"));

    // Across the whole window — including after fast plays out and
    // auto-advance runs — slow must never become current.
    assert_no_barge_in(&queue, slow_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    tick_handle.abort();
}

/// Drain any backlog already buffered on `rx` so the completion-race watch
/// observes only events that follow the selects under test.
fn drain_event_backlog(rx: &mut EventReceiver) {
    use kithara::platform::tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

/// Concurrent completion-vs-select race (the `select_apply` regression gate).
/// Both fixtures load near-simultaneously (construction is undelayed), so a
/// short window after `select(slow)` lands `slow`'s loader completion right
/// around `select(fast)`. The completion reads+consumes `pending_select` then
/// runs `select_item`; the superseding `select(fast)` runs concurrently. The
/// auto-advance tick is intentionally OMITTED so a track can become `current`
/// only via `select` or a loader completion — never via end-of-`fast`
/// auto-advance — which isolates a genuine barge-in (slow stomping the
/// already-current fast) from the legitimate next-in-queue auto-advance.
///
/// A barge-in is `slow` becoming current *after* `fast` already became current.
/// `slow` becoming current *before* `fast` is the legitimate superseded-then-
/// switched ordering and is not a barge-in.
///
/// Pre-`select_apply`-lock this barged in (~5/8 runs measured: slow's
/// `select_item` landed after `select(fast)` committed fast, stomping it at
/// ~tens of ms). With the lock the apply and the select are mutually
/// exclusive, so it must not.
// The absence watch (slow must not stomp the already-current fast) follows
// `CurrentTrackChanged` on the bus rather than polling a virtual clock, so each
// step blocks on a real current-track change; `RACE_OBSERVE` only bounds it as
// a safety cap. The one remaining timer is `RACE_GAP` between `select(slow)`
// and `select(fast)`: it is a deliberate race-WIDENING delay (it must land
// `select(fast)` mid-flight, around slow's loader-completion `select_item`), not
// a wait for a settled state — synchronising on slow reaching `Loaded` would
// order the completion before the select and dissolve the contended window this
// gate exists to probe. It therefore has no state-wait equivalent and is left
// as a timer on purpose.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn concurrent_completion_race_does_not_barge_in() {
    let helper = TestServerHelper::new().await;
    let fast = build_hls(
        &helper,
        Consts::FAST_SEGMENT_COUNT,
        Consts::FAST_SEGMENT_DURATION_S,
    )
    .await;
    let slow = build_hls(
        &helper,
        Consts::SLOW_SEGMENT_COUNT,
        Consts::SLOW_SEGMENT_DURATION_S,
    )
    .await;
    let fast_url = fast.master_url();
    let slow_url = slow.master_url();

    let mut barge_ins: Vec<String> = Vec::new();

    for iter in 0..Consts::STRESS_ITERATIONS {
        let temp = temp_dir();
        // No tick: auto-advance is disabled, so `slow` can only become current
        // via the loader-completion race we are probing.
        let (queue, downloader, store) = build_queue_no_tick(&temp);

        let fast_id = queue.append(TrackSource::Config(Box::new(mk_cfg(
            &fast_url,
            &downloader,
            &store,
        ))));
        let slow_id = queue.append(TrackSource::Config(Box::new(mk_cfg(
            &slow_url,
            &downloader,
            &store,
        ))));

        let mut rx = queue.subscribe();
        drain_event_backlog(&mut rx);

        // select(slow), then a short delay so slow's loader completion fires
        // around select(fast) — the contended window.
        queue
            .select(slow_id, Transition::None)
            .unwrap_or_else(|e| panic!("[iter {iter}] select slow: {e}"));
        time::sleep(Consts::RACE_GAP).await;
        queue
            .select(fast_id, Transition::None)
            .unwrap_or_else(|e| panic!("[iter {iter}] select fast: {e}"));

        // Watch the bounded window by following `CurrentTrackChanged` on the
        // bus instead of polling `current()`: once fast becomes current it must
        // stay current (no auto-advance exists to move off it), so slow
        // appearing *after* fast is the barge-in. `RACE_OBSERVE` bounds the
        // watch as a hard safety cap, not as a pacing wait — every step blocks
        // on the next real current-track change. The initial `current()`
        // snapshot catches a change that landed before we start consuming.
        let watch = kithara::platform::time::Instant::now();
        let mut history: Vec<Option<TrackId>> = vec![queue.current().map(|e| e.id)];
        let mut saw_fast = history.last().copied().flatten() == Some(fast_id);
        loop {
            let remaining = Consts::RACE_OBSERVE.saturating_sub(watch.elapsed());
            if remaining.is_zero() {
                break;
            }
            // Block until the next current-track change (or the safety cap).
            let Some(QueueEvent::CurrentTrackChanged { id: cur }) =
                next_queue_event(&mut rx, remaining, |ev| {
                    matches!(ev, QueueEvent::CurrentTrackChanged { .. })
                })
                .await
            else {
                break;
            };
            if cur != history.last().copied().flatten() {
                history.push(cur);
            }
            if cur == Some(fast_id) {
                saw_fast = true;
            }
            if saw_fast && cur == Some(slow_id) {
                barge_ins.push(format!(
                    "[iter {iter}] slow stomped current fast after {} ms (history={history:?})",
                    watch.elapsed().as_millis(),
                ));
                break;
            }
        }
    }

    assert!(
        barge_ins.is_empty(),
        "concurrent completion race: {n}/{iters} iteration(s) saw slow barge in over current fast:\n{}",
        barge_ins.join("\n"),
        n = barge_ins.len(),
        iters = Consts::STRESS_ITERATIONS,
    );
}
