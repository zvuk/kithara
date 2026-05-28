use std::sync::Arc;

use kithara_events::{Event, QueueEvent, TrackStatus};
use kithara_platform::{
    sync::mpsc,
    time::{Duration, sleep},
    tokio::task::spawn as task_spawn,
};
use kithara_play::SessionDuckingMode;
use kithara_queue::{Queue, QueueConfig, TrackId, TrackSource, Transition};

use crate::web::commands::WorkerCmd;

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

/// Entry called inside a Web Worker thread (via `kithara_platform::spawn`).
///
/// Creates and owns the [`Queue`] (mirroring
/// [`NativeInner`](crate::native::inner::NativeInner)'s construction),
/// spawns a periodic `tick` loop, then drives the command channel.
pub(crate) fn worker_main(cmd_rx: mpsc::Receiver<WorkerCmd>) {
    /// Default crossfade window, in seconds. Mirrors the legacy worker.
    const CROSSFADE_SECONDS: f32 = 5.0;

    kithara_platform::thread::assert_not_main_thread(concat!(module_path!(), "::worker_main"));
    clog!("[WORKER] engine worker started");

    task_spawn(async move {
        clog!("[WORKER] spawn: building Queue");
        let queue = Arc::new(Queue::new(QueueConfig::default()));
        queue.set_crossfade_duration(CROSSFADE_SECONDS);
        clog!("[WORKER] spawn: Queue ready, starting tick loop + command loop");

        spawn_tick_loop(Arc::clone(&queue));

        while let Ok(cmd) = cmd_rx.recv_async().await {
            dispatch_cmd(cmd, &queue);
        }
        clog!("[WORKER] command channel closed, shutting down");
    });
}

/// Spawn the periodic `Queue::tick` loop. `tick` is synchronous; the
/// loop awaits a `setTimeout`-backed `sleep` between ticks so it yields
/// to the worker's task executor without busy-spinning.
fn spawn_tick_loop(queue: Arc<Queue>) {
    /// Tick cadence for the queue's internal `tick()` loop, in
    /// milliseconds. Drives auto-advance / crossfade arming and drains
    /// engine events. Wall clock; not tied to the audio-thread process
    /// callback.
    const TICK_INTERVAL_MS: u64 = 100;

    task_spawn(async move {
        loop {
            if let Err(err) = queue.tick() {
                clog!("[WORKER] queue tick error: {err}");
            }
            sleep(Duration::from_millis(TICK_INTERVAL_MS)).await;
        }
    });
}

fn dispatch_cmd(cmd: WorkerCmd, queue: &Arc<Queue>) {
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    /// Ducking mode for soft attenuation.
    const DUCKING_SOFT: u32 = 1;

    /// Ducking mode for hard attenuation.
    const DUCKING_HARD: u32 = 2;

    match cmd {
        WorkerCmd::SelectTrack { url, request_id } => {
            spawn_play_single(Arc::clone(queue), url, request_id);
        }
        WorkerCmd::Play => queue.play(),
        WorkerCmd::Pause => queue.pause(),
        WorkerCmd::Stop => {
            queue.pause();
            queue.clear();
        }
        WorkerCmd::Seek(ms) => {
            let _ = queue.seek(ms.max(0.0) / MS_PER_SECOND);
        }
        WorkerCmd::SetVolume(vol) => queue.set_volume(vol),
        WorkerCmd::SetCrossfade(secs) => queue.set_crossfade_duration(secs),
        WorkerCmd::SetEqGain { band, gain_db } => {
            let _ = queue.set_eq_gain(band as usize, gain_db);
        }
        WorkerCmd::ResetEq => {
            let _ = queue.reset_eq();
        }
        WorkerCmd::SetDucking(mode) => {
            let mode = match mode {
                DUCKING_SOFT => SessionDuckingMode::Soft,
                DUCKING_HARD => SessionDuckingMode::Hard,
                _ => SessionDuckingMode::Off,
            };
            // The queue does not expose a session-ducking setter (it is a
            // player-wide concern surfaced via the facade in Wave 4). The
            // legacy command is accepted and logged as a no-op here so the
            // existing `player_set_session_ducking` export keeps compiling.
            clog!("[WORKER] SetDucking({mode:?}) is a no-op on the queue path (Wave 4)");
        }
        WorkerCmd::Append { id, url } => {
            queue.append_with_id(id, TrackSource::Uri(url));
        }
        WorkerCmd::Insert {
            id,
            url,
            after,
            request_id,
        } => {
            let result = queue
                .insert_with_id(id, TrackSource::Uri(url), after)
                .map(|_| ())
                .map_err(|e| e.to_string());
            crate::web::js::send_reply(request_id, result);
        }
        WorkerCmd::Remove { id, request_id } => {
            let result = queue.remove(id).map_err(|e| e.to_string());
            crate::web::js::send_reply(request_id, result);
        }
        WorkerCmd::Replace {
            index,
            id,
            url,
            request_id,
        } => {
            let result = replace_track(queue, index, id, url);
            crate::web::js::send_reply(request_id, result);
        }
        WorkerCmd::SelectQueue {
            id,
            transition,
            request_id,
        } => {
            let result = queue.select(id, transition).map_err(|e| e.to_string());
            crate::web::js::send_reply(request_id, result);
        }
        WorkerCmd::RemoveAll => queue.clear(),
    }
}

/// Legacy single-track entry point. Clears the queue, appends the URL,
/// and selects it, then resolves the `request_id` reply once the track
/// reaches [`TrackStatus::Loaded`] (preserving the pre-Wave-3 contract
/// where `player_select_track` resolves when playback is ready) or
/// rejects on [`TrackStatus::Failed`].
fn spawn_play_single(queue: Arc<Queue>, url: String, request_id: u32) {
    task_spawn(async move {
        let result = play_single(&queue, url).await;
        crate::web::js::send_reply(request_id, result);
    });
}

async fn play_single(queue: &Arc<Queue>, url: String) -> Result<(), String> {
    clog!("[WORKER] select_track: url={url}");
    queue.clear();
    let mut rx = queue.subscribe();
    let id = queue.append(TrackSource::Uri(url));
    // `select` on a still-loading track records a pending select that the
    // loader fires once the resource is ready, so playback starts as soon
    // as the load completes.
    queue
        .select(id, Transition::None)
        .map_err(|e| e.to_string())?;
    queue.play();

    loop {
        match rx.recv().await {
            Ok(Event::Queue(QueueEvent::TrackStatusChanged { id: ev_id, status }))
                if ev_id == id =>
            {
                match status {
                    TrackStatus::Loaded | TrackStatus::Consumed => {
                        clog!("[WORKER] select_track: ready ({status:?})");
                        return Ok(());
                    }
                    TrackStatus::Failed(reason) => return Err(reason),
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => return Err("event stream closed before track became ready".to_string()),
        }
    }
}

/// Mirror of [`NativeInner::replace_item`](crate::native::inner::NativeInner::replace_item):
/// insert the new track after the predecessor of `index`, then drop the
/// old track at `index`.
fn replace_track(queue: &Arc<Queue>, index: u32, id: TrackId, url: String) -> Result<(), String> {
    let idx = index as usize;
    let tracks = queue.tracks();
    let old = tracks
        .get(idx)
        .ok_or_else(|| format!("item index {idx} out of range (len: {})", tracks.len()))?;
    let old_id = old.id;
    let after = if idx == 0 {
        None
    } else {
        tracks.get(idx - 1).map(|e| e.id)
    };
    queue
        .insert_with_id(id, TrackSource::Uri(url), after)
        .map_err(|e| e.to_string())?;
    let _ = queue.remove(old_id);
    Ok(())
}
