use std::{cell::RefCell, collections::HashMap, num::NonZeroUsize, rc::Rc};

use kithara::{
    abr::AbrMode,
    assets::StorageBackend,
    drm::{KeyRequest, KeyRequestFactory},
    hls::KeyOptions,
    host::wasm,
    platform::{
        sync::{Arc, mpsc},
        thread::{assert_not_main_thread, keep_worker_alive},
        time::{Duration, sleep},
        tokio::task::spawn as task_spawn,
    },
    play::{
        PlayError, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceSrc,
        policy::{DomainKeyPolicy, DomainKeyRule},
    },
    queue::{QueueConfig, TrackId, Transition},
};

use crate::{
    FfiQueueSettings,
    item::{ItemBuildConfig, SourcePatches},
    observer::{AUTH_TOKEN_HEADER, SALT_HEADER},
    pools::{
        FfiPools, FfiQueue, FfiQueueControl, FfiResourceConfig, FfiStore, FfiTrackSource,
        FfiWorker, Pools,
    },
    types::FfiAbrMode,
    web::{analysis::AnalysisRuns, commands::WorkerCmd, key_processor_bridge},
};

struct Consts;

impl Consts {
    /// Capacity for concurrent playback and crossfade HLS working sets.
    const ASSET_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(128).unwrap();
    /// Bound cached media while leaving wasm linear-memory headroom for decode
    /// and PCM buffers.
    const ASSET_CACHE_MAX_BYTES: u64 = 128 * 1024 * 1024;
}

/// Web's established initial volume, retained by the worker's PlayerConfig.
pub(crate) const DEFAULT_VOLUME: f32 = 0.5;

/// Player-wide DRM + network state owned by the engine Worker, parallel to
/// the `key_options` + `player_headers` fields on
/// [`NativeInner`](crate::native::inner::NativeInner). Held in a
/// `RefCell` shared across the worker's command loop: setters mutate it,
/// and each track build snapshots it into a [`FfiResourceConfig`].
struct BuildState {
    store: FfiStore,
    headers: HashMap<String, String>,
    keys: KeyOptions,
    pools: Pools,
    worker: FfiWorker,
}

impl BuildState {
    fn new(pools: Pools) -> Self {
        let store = FfiStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cache_capacity(Consts::ASSET_CACHE_CAPACITY)
            .max_bytes(Consts::ASSET_CACHE_MAX_BYTES)
            .build();
        let worker = FfiWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
        Self {
            pools,
            store,
            headers: HashMap::new(),
            keys: KeyOptions::default(),
            worker,
        }
    }
}

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

/// Entry called inside a Web Worker thread (via `thread::spawn`).
///
/// Inserts and owns one [`FfiQueue`] member in the canonical Host (mirroring
/// [`NativeInner`](crate::native::inner::NativeInner)'s construction), spawns
/// a periodic `tick` loop, then drives the command channel.
///
/// `keep_worker_alive` is required: without it the Worker's spawn closure returns immediately since
/// it only spawns async tasks, and `wasm_safe_thread` then closes the Worker, killing the command
/// and tick loops.
pub(crate) fn worker_main(
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    host_sender: wasm::HostSender<FfiPools>,
    pools: Pools,
    queue_settings: FfiQueueSettings,
) {
    assert_not_main_thread(concat!(module_path!(), "::worker_main"));
    keep_worker_alive();

    task_spawn(async move {
        let mut host = wasm::remote_host(host_sender);
        let state = BuildState::new(pools);
        let queue_store = state.store.clone();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(host.requested_sample_rate())
                .worker(state.worker.clone())
                .build(),
        );
        player.set_volume(DEFAULT_VOLUME);
        let mut queue_config = QueueConfig::builder()
            .player(player)
            .store(queue_store)
            .build();
        if let Err(error) = queue_settings.apply_to(&mut queue_config) {
            clog!("[WORKER] invalid queue settings: {error}");
            return;
        }
        let queue = FfiQueue::new(queue_config);
        let owner = match host.insert(queue) {
            Ok(owner) => owner,
            Err(error) => {
                clog!("[WORKER] host rejected queue insertion: {error}");
                return;
            }
        };
        let queue = owner.control().clone();
        let analysis = Rc::new(RefCell::new(AnalysisRuns::new(state.pools.clone())));
        let build_state = Rc::new(RefCell::new(state));
        spawn_tick_loop(queue.clone());
        crate::web::observer::source::spawn(&queue);

        while let Ok(cmd) = cmd_rx.recv_async().await {
            dispatch_cmd(cmd, &queue, &build_state, &analysis);
        }
        analysis.borrow_mut().clear();

        match host.remove(&owner) {
            Ok(()) | Err(PlayError::SessionGone { .. }) => {}
            Err(error) => {
                clog!("[WORKER] host queue removal failed; resident retained: {error}");
            }
        }
    });
}

/// Spawn the periodic `FfiQueue::tick` loop. `tick` is synchronous; the
/// loop awaits a `setTimeout`-backed `sleep` between ticks so it yields
/// to the worker's task executor without busy-spinning.
fn spawn_tick_loop(queue: FfiQueueControl) {
    /// Tick cadence for the queue's internal `tick()` loop, in
    /// milliseconds. Drives auto-advance / crossfade arming and drains
    /// engine events. Wall clock; not tied to the audio-thread process
    /// callback.
    const TICK_INTERVAL_MS: u64 = 100;

    task_spawn(async move {
        loop {
            if queue.is_closed() {
                break;
            }
            if let Err(err) = queue.tick() {
                clog!("[WORKER] queue tick error: {err}");
            }
            sleep(Duration::from_millis(TICK_INTERVAL_MS)).await;
        }
    });
}

fn dispatch_cmd(
    cmd: WorkerCmd,
    queue: &FfiQueueControl,
    build_state: &Rc<RefCell<BuildState>>,
    analysis: &Rc<RefCell<AnalysisRuns>>,
) {
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
        WorkerCmd::SetMuted(muted) => queue.set_muted(muted),
        WorkerCmd::SetPlayingRate(rate) => queue.set_default_rate(rate),
        WorkerCmd::SetCrossfade(settings) => {
            let _ = queue.set_crossfade_settings(settings);
        }
        WorkerCmd::Next => {
            let _ = queue.next(Transition::None);
        }
        WorkerCmd::Previous => {
            let _ = queue.previous(Transition::None);
        }
        WorkerCmd::SetEqGain { band, gain_db } => {
            let band_idx: usize = num_traits::cast(band).unwrap_or(0);
            let _ = queue.set_eq_gain(band_idx, gain_db);
        }
        WorkerCmd::SetEqLayout(layout) => {
            let _ = queue.set_eq_layout(layout);
        }
        WorkerCmd::ResetEq => {
            let _ = queue.reset_eq();
        }
        WorkerCmd::Append { id, config } => {
            let source = build_source(&build_state.borrow(), config);
            if let Err(error) = queue.append_with_id(id, source) {
                clog!("[WORKER] append rejected for {id:?}: {error}");
            }
        }
        WorkerCmd::Insert {
            id,
            config,
            after,
            request_id,
        } => {
            let source = build_source(&build_state.borrow(), config);
            let result = queue
                .insert_with_id(id, source, after)
                .map(|_| ())
                .map_err(|e| e.to_string());
            crate::web::interop::send_reply(request_id, result);
        }
        WorkerCmd::Analyze { id, request_id } => {
            let state = build_state.borrow();
            if let Err(error) = analysis
                .borrow_mut()
                .start_queued(queue, id, request_id, |url| {
                    build_config(&state, url, None, None, 0.0, None)
                })
            {
                crate::web::interop::send_reply(request_id, Err(error));
            }
        }
        WorkerCmd::Remove { id, request_id } => {
            analysis.borrow_mut().cancel(id);
            let result = queue.remove(id).map_err(|e| e.to_string());
            crate::web::interop::send_reply(request_id, result);
        }
        WorkerCmd::Replace {
            index,
            id,
            config,
            request_id,
        } => {
            let result = replace_track(
                queue,
                &build_state.borrow(),
                ReplaceTrackArgs { config, id, index },
            )
            .map(|dropped| analysis.borrow_mut().cancel(dropped));
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
        WorkerCmd::RemoveAll => {
            analysis.borrow_mut().clear();
            queue.clear();
        }
        WorkerCmd::SetAbrMode { variant_index } => {
            apply_abr_mode(queue, variant_index);
        }
        WorkerCmd::SetRepeat(mode) => queue.set_repeat(mode),
        WorkerCmd::SetPlaybackOrder(order) => queue.set_playback_order(order),
        WorkerCmd::SetActionAtItemEnd(action) => queue.set_action_at_item_end(action),
        WorkerCmd::SetDucking(mode) => {
            if let Err(err) = queue.set_session_ducking(mode) {
                clog!("[WORKER] session ducking failed: {err}");
            }
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
                SetupHlsAesArgs {
                    headers,
                    query_params,
                    salt,
                    domains,
                },
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

fn apply_abr_mode(queue: &FfiQueueControl, variant_index: Option<u32>) {
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

fn apply_peak_bitrate(queue: &FfiQueueControl, wifi_bps: f64, cellular_bps: f64) {
    if let Some(handle) = queue.current_abr_handle() {
        handle.set_max_bandwidth_bps(effective_cap(wifi_bps, cellular_bps));
    }
}

struct SetupHlsAesArgs {
    headers: Option<HashMap<String, String>>,
    query_params: Option<HashMap<String, String>>,
    salt: String,
    domains: Vec<String>,
}

/// Fold a DRM rule into the worker's [`BuildState`]. Builds the
/// cross-thread [`KeyRequestFactory`] (the real JS callback lives on the
/// main thread; the worker-side processor routes each decrypt through
/// [`key_processor_bridge`]) and writes the salt / static headers into the
/// player-wide header map. Mirrors
/// [`NativeInner::setup_hls_aes_with_rule`](crate::native::inner::NativeInner).
fn register_key_rule(state: &mut BuildState, args: SetupHlsAesArgs) {
    let SetupHlsAesArgs {
        salt,
        domains,
        headers,
        query_params,
    } = args;

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
    let rule = DomainKeyRule::for_domains(&domains, factory)
        .maybe_headers(headers)
        .maybe_query_params(query_params)
        .build();

    let mut registry = state.keys.key_registry.take().unwrap_or_default();
    registry.register(Arc::new(DomainKeyPolicy::new([rule])));
    state.keys = KeyOptions::builder().key_registry(registry).build();
}

/// Build a worker source from the item's immutable configuration. Keep the
/// common no-policy URI path allocation-light.
fn build_source(state: &BuildState, item: ItemBuildConfig) -> FfiTrackSource {
    let ItemBuildConfig {
        config: item,
        source,
    } = item;
    let url = item.url.clone();
    if state.keys.key_registry.is_none()
        && state.headers.is_empty()
        && item.headers.as_ref().is_none_or(HashMap::is_empty)
        && item.abr_mode.is_none()
        && !(item.preferred_peak_bitrate.is_finite() && item.preferred_peak_bitrate > 0.0)
        && source.is_none()
    {
        return FfiTrackSource::Uri(url);
    }
    build_config(
        state,
        &url,
        item.headers,
        item.abr_mode,
        item.preferred_peak_bitrate,
        source,
    )
    .map_or(FfiTrackSource::Uri(url), |config| {
        FfiTrackSource::Config(Box::new(config))
    })
}

fn build_config(
    state: &BuildState,
    url: &str,
    item_headers: Option<HashMap<String, String>>,
    abr_mode: Option<FfiAbrMode>,
    preferred_peak_bitrate: f64,
    source: Option<SourcePatches>,
) -> Option<FfiResourceConfig> {
    let src = ResourceSrc::parse(url)
        .inspect_err(|err| {
            clog!("[WORKER] build_config: invalid url {url}: {err}");
        })
        .ok()?;
    let mut headers = state.headers.clone();
    headers.extend(item_headers.unwrap_or_default());
    let abr_mode = abr_mode.map(|mode| match mode {
        FfiAbrMode::Auto => AbrMode::Auto(None),
        FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
    });
    Some(
        FfiResourceConfig::for_src(src)
            .keys(state.keys.clone())
            .maybe_headers((!headers.is_empty()).then(|| headers.into()))
            .initial_abr_mode(abr_mode.unwrap_or_default())
            .preferred_peak_bitrate(preferred_peak_bitrate)
            .file(
                source
                    .as_ref()
                    .map_or_else(Default::default, |source| source.file.clone()),
            )
            .hls(
                source
                    .as_ref()
                    .map_or_else(Default::default, |source| source.hls.clone()),
            )
            .store(state.store.clone())
            .worker(state.worker.clone())
            .build(),
    )
}

struct ReplaceTrackArgs {
    config: ItemBuildConfig,
    id: TrackId,
    index: u32,
}

/// Mirror of [`NativeInner::replace_item`](crate::native::inner::NativeInner::replace_item):
/// insert the new track after the predecessor of `index`, then drop the
/// old track at `index`.
fn replace_track(
    queue: &FfiQueueControl,
    state: &BuildState,
    args: ReplaceTrackArgs,
) -> Result<TrackId, String> {
    let ReplaceTrackArgs { index, id, config } = args;

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
        .insert_with_id(id, build_source(state, config), after)
        .map_err(|e| e.to_string())?;
    queue.remove(old_id).map_err(|e| e.to_string())?;
    Ok(old_id)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn shared_store_keeps_two_hls_working_sets() {
        let state = BuildState::default();

        assert_eq!(
            state.store.ephemeral_cache_capacity(),
            Some(Consts::ASSET_CACHE_CAPACITY)
        );
        assert_eq!(Consts::ASSET_CACHE_MAX_BYTES, 128 * 1024 * 1024);
    }
}
