use std::{cell::RefCell, collections::HashMap, rc::Rc};

use kithara_abr::AbrMode;
use kithara_bufpool::Region;
use kithara_drm::{KeyRequest, KeyRequestFactory};
use kithara_hls::{KeyOptions, KeyProcessorRule};
use kithara_platform::{
    sync::{Arc, mpsc},
    thread::{assert_not_main_thread, keep_worker_alive},
    time::{Duration, sleep},
    tokio::task::spawn as task_spawn,
};
use kithara_play::{ResourceConfig, wasm};
use kithara_queue::{Queue, QueueConfig, TrackId, TrackSource};

use crate::{
    observer::{AUTH_TOKEN_HEADER, SALT_HEADER},
    web::{commands::WorkerCmd, key_processor_bridge},
};

/// Player-wide DRM + network state owned by the engine Worker, parallel to
/// the `key_options` + `player_headers` fields on
/// [`NativeInner`](crate::native::inner::NativeInner). Held in a
/// `RefCell` shared across the worker's command loop: setters mutate it,
/// and each track build snapshots it into a [`ResourceConfig`].
#[derive(Default)]
struct BuildState {
    headers: HashMap<String, String>,
    keys: KeyOptions,
    region: Region,
}

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

/// Entry called inside a Web Worker thread (via `thread::spawn`).
///
/// Creates and owns the [`Queue`] (mirroring
/// [`NativeInner`](crate::native::inner::NativeInner)'s construction),
/// spawns a periodic `tick` loop, then drives the command channel.
pub(crate) fn worker_main(
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    session_tx: mpsc::Sender<kithara_play::CmdMsg>,
) {
    /// Default crossfade window, in seconds. Mirrors the legacy worker.
    const CROSSFADE_SECONDS: f32 = 5.0;

    assert_not_main_thread(concat!(module_path!(), "::worker_main"));
    // Without this the Worker's spawn closure returns immediately (it only
    // spawns async tasks) and `wasm_safe_thread` `close()`s the Worker, killing
    // the command + tick loops. Keeps the Worker's event loop pumping for the
    // page's lifetime so the spawned futures keep running.
    keep_worker_alive();

    task_spawn(async move {
        let session = wasm::remote_session(session_tx);
        let state = BuildState::default();
        let player = Arc::new(kithara_play::PlayerImpl::new(
            kithara_play::PlayerConfig::builder()
                .byte_pool(state.region.byte_pool())
                .pcm_pool(state.region.pcm_pool())
                .session(session.dispatcher())
                .build(),
        ));
        let queue = Rc::new(Queue::new(QueueConfig::builder().player(player).build()));
        queue.set_crossfade_duration(CROSSFADE_SECONDS);

        let build_state = Rc::new(RefCell::new(state));
        spawn_tick_loop(Rc::clone(&queue));
        crate::web::observer::source::spawn(&queue);

        while let Ok(cmd) = cmd_rx.recv_async().await {
            dispatch_cmd(cmd, &queue, &build_state);
        }
    });
}

/// Spawn the periodic `Queue::tick` loop. `tick` is synchronous; the
/// loop awaits a `setTimeout`-backed `sleep` between ticks so it yields
/// to the worker's task executor without busy-spinning.
fn spawn_tick_loop(queue: Rc<Queue>) {
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

fn dispatch_cmd(cmd: WorkerCmd, queue: &Rc<Queue>, build_state: &Rc<RefCell<BuildState>>) {
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    match cmd {
        WorkerCmd::Play => queue.play(),
        WorkerCmd::Pause => queue.pause(),
        WorkerCmd::Stop => {
            queue.pause();
            let _ = queue.seek(0.0);
        }
        WorkerCmd::Seek(ms) => {
            let _ = queue.seek(ms.max(0.0) / MS_PER_SECOND);
        }
        WorkerCmd::SetVolume(vol) => queue.set_volume(vol),
        WorkerCmd::SetCrossfade(secs) => queue.set_crossfade_duration(secs),
        WorkerCmd::SetEqGain { band, gain_db } => {
            let band_idx: usize = num_traits::cast(band).unwrap_or(0);
            let _ = queue.set_eq_gain(band_idx, gain_db);
        }
        WorkerCmd::ResetEq => {
            let _ = queue.reset_eq();
        }
        WorkerCmd::Append { id, url } => {
            let source = build_source(&build_state.borrow(), url);
            queue.append_with_id(id, source);
        }
        WorkerCmd::Insert {
            id,
            url,
            after,
            request_id,
        } => {
            let source = build_source(&build_state.borrow(), url);
            let result = queue
                .insert_with_id(id, source, after)
                .map(|_| ())
                .map_err(|e| e.to_string());
            crate::web::interop::send_reply(request_id, result);
        }
        WorkerCmd::Remove { id, request_id } => {
            let result = queue.remove(id).map_err(|e| e.to_string());
            crate::web::interop::send_reply(request_id, result);
        }
        WorkerCmd::Replace {
            index,
            id,
            url,
            request_id,
        } => {
            let result = replace_track(queue, &build_state.borrow(), index, id, url);
            crate::web::interop::send_reply(request_id, result);
        }
        WorkerCmd::SelectQueue {
            id,
            transition,
            request_id,
        } => {
            let result = queue.select(id, transition).map_err(|e| e.to_string());
            crate::web::interop::send_reply(request_id, result);
        }
        WorkerCmd::RemoveAll => queue.clear(),
        WorkerCmd::SetAbrMode { variant_index } => {
            apply_abr_mode(queue, variant_index);
        }
        WorkerCmd::PeakBitrate {
            wifi_bps,
            cellular_bps,
        } => {
            apply_peak_bitrate(queue, wifi_bps, cellular_bps);
        }
        WorkerCmd::AuthToken { token } => {
            let mut state = build_state.borrow_mut();
            if token.is_empty() {
                state.headers.remove(AUTH_TOKEN_HEADER);
            } else {
                state.headers.insert(AUTH_TOKEN_HEADER.to_string(), token);
            }
        }
        WorkerCmd::SetupHlsAes {
            salt,
            domains,
            headers,
            query_params,
        } => {
            register_key_rule(
                &mut build_state.borrow_mut(),
                &salt,
                &domains,
                headers,
                query_params,
            );
        }
    }
}

/// Effective ABR cap (bits/sec) for the wifi/cellular ceilings. `None`
/// when both are unset. Mirrors
/// [`PeakBitrate::effective_cap`](crate::native::inner::PeakBitrate) on
/// native.
fn effective_cap(wifi_bps: f64, cellular_bps: f64) -> Option<u64> {
    const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;
    let cap = [wifi_bps, cellular_bps]
        .into_iter()
        .filter(|v| *v > 0.0)
        .reduce(f64::min)
        .filter(|v| v.is_finite() && *v > 0.0)?;
    if cap >= U64_MAX_AS_F64 {
        return Some(u64::MAX);
    }
    Some(num_traits::cast(cap.trunc()).unwrap_or(u64::MAX))
}

fn apply_abr_mode(queue: &Rc<Queue>, variant_index: Option<u32>) {
    let Some(handle) = queue.current_abr_handle() else {
        return;
    };
    let mode = variant_index.map_or(AbrMode::Auto(None), |index| {
        AbrMode::manual(num_traits::cast(index).unwrap_or(0))
    });
    if let Err(err) = handle.set_mode(mode) {
        clog!("[WORKER] set_abr_mode rejected by ABR state: {err:?}");
    }
}

fn apply_peak_bitrate(queue: &Rc<Queue>, wifi_bps: f64, cellular_bps: f64) {
    if let Some(handle) = queue.current_abr_handle() {
        handle.set_max_bandwidth_bps(effective_cap(wifi_bps, cellular_bps));
    }
}

/// Fold a DRM rule into the worker's [`BuildState`]. Builds the
/// cross-thread [`KeyRequestFactory`] (the real JS callback lives on the
/// main thread; the worker-side processor routes each decrypt through
/// [`key_processor_bridge`]) and writes the salt / static headers into the
/// player-wide header map. Mirrors
/// [`NativeInner::setup_hls_aes_with_rule`](crate::native::inner::NativeInner).
fn register_key_rule(
    state: &mut BuildState,
    salt: &str,
    domains: &[String],
    headers: Option<HashMap<String, String>>,
    query_params: Option<HashMap<String, String>>,
) {
    if let Some(rule_headers) = headers.as_ref() {
        for (k, v) in rule_headers {
            state.headers.insert(k.clone(), v.clone());
        }
    }
    state
        .headers
        .insert(SALT_HEADER.to_string(), salt.to_owned());

    let factory: KeyRequestFactory = {
        let salt = salt.to_owned();
        Arc::new(move || {
            let mut req_headers = HashMap::new();
            req_headers.insert(SALT_HEADER.to_string(), salt.clone());
            KeyRequest::new(
                req_headers,
                key_processor_bridge::worker_key_processor(salt.clone()),
            )
        })
    };
    let rule = KeyProcessorRule::for_domains(domains, factory)
        .maybe_headers(headers)
        .maybe_query_params(query_params)
        .build();

    let mut registry = state.keys.key_registry.take().unwrap_or_default();
    registry.add(rule);
    state.keys = KeyOptions::builder().key_registry(registry).build();
}

/// Build a [`TrackSource`] for `url`, snapshotting the player-wide DRM keys
/// and headers from `state` (mirrors native `build_source_for_item`). Falls
/// back to a bare [`TrackSource::Uri`] when no keys or headers are set so
/// the common non-DRM path stays allocation-light.
fn build_source(state: &BuildState, url: String) -> TrackSource {
    if state.keys.key_registry.is_none() && state.headers.is_empty() {
        return TrackSource::Uri(url);
    }
    match ResourceConfig::for_src(&url) {
        Ok(builder) => {
            let headers = (!state.headers.is_empty()).then(|| state.headers.clone());
            let config = builder
                .keys(state.keys.clone())
                .maybe_headers(headers.map(Into::into))
                .byte_pool(state.region.byte_pool())
                .pcm_pool(state.region.pcm_pool())
                .build();
            TrackSource::Config(Box::new(config))
        }
        Err(err) => {
            clog!("[WORKER] build_source: invalid url {url}: {err}; using raw URI");
            TrackSource::Uri(url)
        }
    }
}

/// Mirror of [`NativeInner::replace_item`](crate::native::inner::NativeInner::replace_item):
/// insert the new track after the predecessor of `index`, then drop the
/// old track at `index`.
fn replace_track(
    queue: &Rc<Queue>,
    state: &BuildState,
    index: u32,
    id: TrackId,
    url: String,
) -> Result<(), String> {
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
        .insert_with_id(id, build_source(state, url), after)
        .map_err(|e| e.to_string())?;
    let _ = queue.remove(old_id);
    Ok(())
}
