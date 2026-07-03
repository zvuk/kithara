//! Canonical state/event waits for the integration suite.
//!
//! Every state-wait funnels through [`wait_until`], whose single virtual
//! poll-tick `sleep` is the only timer `sleep` in the test surface. Under the
//! flash virtual clock that tick advances time between predicate checks, so a
//! wait resolves the instant the program reaches the asserted state — never on a
//! wall-clock guess. The `deadline` argument bounds only a non-progress watchdog
//! that returns `Err` (callers `.expect()` it); it is never an early-success
//! path that lets a later assertion pass on timeout.

#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    events::{AudioEvent, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    platform::{
        thread::active_named_thread_count,
        time::{Duration, Instant, sleep, timeout},
        tokio::sync::broadcast::error::RecvError,
    },
    queue::Queue,
};

use crate::offline::OfflinePlayer;

/// Poll cadence for [`wait_until`] and the queue-polling waits. This is the only
/// timer `sleep` in the suite: a virtual tick that advances the flash clock so
/// the engine runs between predicate checks.
const POLL_TICK: Duration = Duration::from_millis(20);

/// Settle window for [`wait_thread_count_quiesced`]: the named-thread counter
/// must hold steady across this many consecutive virtual ticks before it is
/// treated as quiesced.
const QUIESCE_TICKS: usize = 3;

/// The one poll primitive. Re-checks `cond` after every virtual tick until it is
/// true, or returns `Err` when `deadline` (virtual time) elapses without
/// progress. Callers `.expect()` the result, so a genuinely wedged pipeline
/// panics with `label` instead of silently letting a later assertion pass.
pub async fn wait_until<F>(deadline: Duration, label: &str, mut cond: F) -> Result<(), String>
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    loop {
        if cond() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "wait_until({label}) exceeded {deadline:?} without reaching the target state"
            ));
        }
        sleep(POLL_TICK).await;
    }
}

/// Wait until `queue.position_seconds()` reports at least `min_secs`, then
/// return the observed position (seconds).
///
/// Use this when only a `&Queue` is in hand. When an [`EventReceiver`] is
/// available, prefer [`wait_for_position_event`]: it reads sink-truth from
/// `AudioEvent::PlaybackProgress`, which (unlike the tick-cached
/// `position_seconds()`) does not go stale once an event-driven wait collapses
/// real time under flash. The returned position is the tick-cached value at
/// predicate-pass and is only meant for coarse "advanced past X" comparisons.
pub async fn wait_for_position_at_least(
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<f64, String> {
    let mut observed = min_secs;
    wait_until(deadline, "position_at_least", || {
        match queue.position_seconds() {
            Some(p) if p >= min_secs => {
                observed = p;
                true
            }
            _ => false,
        }
    })
    .await
    .map_err(|_| {
        format!(
            "position stayed below {min_secs:.2}s for {deadline:?} (last: {:?})",
            queue.position_seconds()
        )
    })?;
    Ok(observed)
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
pub async fn wait_for_position_event(
    rx: &mut EventReceiver,
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<f64, String> {
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
            match timeout(POLL_TICK, rx.recv()).await {
                Ok(Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))) => {
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
/// [`wait_for_position_near_event`] when an [`EventReceiver`] is available.
pub async fn wait_for_position_near(
    queue: &Queue,
    target: f64,
    tolerance: f64,
    deadline: Duration,
) -> Result<f64, String> {
    let mut observed = target;
    wait_until(deadline, "position_near", || {
        match queue.position_seconds() {
            Some(p) if (p - target).abs() < tolerance => {
                observed = p;
                true
            }
            _ => false,
        }
    })
    .await
    .map_err(|_| {
        format!(
            "position never reached {target:.2}s (±{tolerance:.2}) in {deadline:?} (last: {:?})",
            queue.position_seconds()
        )
    })?;
    Ok(observed)
}

/// Event-driven [`wait_for_position_near`]: resolves the moment sink-truth
/// `PlaybackProgress` or `SeekComplete` lands within `tolerance` of `target`,
/// returning that position. The queue state is re-checked on the same virtual
/// poll cadence so a dropped / delayed broadcast receiver cannot hide an
/// already-near position.
pub async fn wait_for_position_near_event(
    rx: &mut EventReceiver,
    queue: &Queue,
    target: f64,
    tolerance: f64,
    deadline: Duration,
) -> Result<f64, String> {
    if let Some(pos) = queue.position_seconds()
        && (pos - target).abs() < tolerance
    {
        return Ok(pos);
    }
    timeout(deadline, async {
        loop {
            if let Some(pos) = queue.position_seconds()
                && (pos - target).abs() < tolerance
            {
                return Ok(pos);
            }
            match timeout(POLL_TICK, rx.recv()).await {
                Ok(Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))) => {
                    let pos = position_ms as f64 / 1000.0;
                    if (pos - target).abs() < tolerance {
                        return Ok(pos);
                    }
                }
                Ok(Ok(Event::Audio(AudioEvent::SeekComplete { position, .. }))) => {
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

/// Wait for the first bus event matching `pred`, recv-driven.
///
/// Parks on the broadcast `recv` so the engine advances the virtual clock; the
/// platform `timeout(deadline, ...)` is the backstop — under flash it fires when
/// the system quiesces with the event still unseen, on native it is a real
/// bound. `Lagged` is tolerated (mirrors other multi-subscriber tests); `Closed`
/// is an error. This is the generic primitive behind the typed
/// [`wait_for_position_event`] / [`wait_for_loader_done_event`].
pub async fn wait_for_event(
    rx: &mut EventReceiver,
    what: &str,
    mut pred: impl FnMut(&Event) -> bool,
    deadline: Duration,
) -> Result<(), String> {
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(ev) if pred(&ev) => return Ok(()),
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
        _ => None,
    }
}

/// Wait until `track_id`'s loader finishes (`Loaded`/`Consumed`) by polling the
/// queue's track status. `Failed` resolves to `Err`. Use the event variant
/// [`wait_for_loader_done_event`] when a receiver is available.
pub async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
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

/// Wait until `track_id`'s loader finishes, driven by
/// `QueueEvent::TrackStatusChanged` (parks on the virtual clock). A fast-path
/// and `Lagged` re-read guard against an already-terminal status or a dropped
/// event.
pub async fn wait_for_loader_done_event(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    if let Some(entry) = queue.track(track_id)
        && let Some(res) = loader_outcome(&entry.status)
    {
        return res;
    }
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(Event::Queue(QueueEvent::TrackStatusChanged { id, status }))
                    if id == track_id =>
                {
                    if let Some(res) = loader_outcome(&status) {
                        return res;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(entry) = queue.track(track_id)
                        && let Some(res) = loader_outcome(&entry.status)
                    {
                        return res;
                    }
                }
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
/// consecutive virtual ticks — then return the settled value.
///
/// This waits on the real teardown state (the same counter every leak assertion
/// reads): the count is decremented when each spawned thread's closure RETURNS
/// after shutdown/cancel, which is observable under the flash clock. A leak
/// keeps the count high — it still stabilizes high, so the assertion still
/// catches the growth. The watchdog PANICS on genuine non-quiescence rather than
/// returning a mid-teardown reading that an assertion could pass against.
pub async fn wait_thread_count_quiesced(deadline: Duration) -> usize {
    let watchdog = Instant::now() + deadline;
    let mut last = active_named_thread_count();
    let mut stable = 0usize;
    loop {
        sleep(POLL_TICK).await;
        let now = active_named_thread_count();
        if now == last {
            stable += 1;
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
        let this = max_blocks.saturating_sub(rendered).clamp(1, BATCH);
        for _ in 0..this {
            let _ = player.render(block_frames);
        }
        rendered = rendered.saturating_add(this);
        let position = player.position();
        if position >= until_position && rendered >= max_blocks {
            return;
        }
        if position > last_position {
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
