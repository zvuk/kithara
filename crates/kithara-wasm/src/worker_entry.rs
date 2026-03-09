//! Engine Worker entry point.
//!
//! Spawned via [`kithara_platform::spawn`]. Owns [`PlayerImpl`] and processes
//! commands from the main-thread bridge.

use std::{num::NonZeroUsize, sync::Arc};

use kithara_platform::{sync::mpsc, tokio};
use kithara_play::{PlayerConfig, PlayerImpl, Resource, ResourceConfig, SessionDuckingMode};

use crate::commands::WorkerCmd;

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

const CROSSFADE_SECONDS: f32 = 5.0;

/// Entry called inside a Web Worker thread (via `kithara_platform::spawn`).
#[kithara_wasm_macros::assert_not_main_thread]
pub(crate) fn worker_main(cmd_rx: mpsc::Receiver<WorkerCmd>) {
    clog!("[WORKER] engine worker started");

    tokio::task::spawn(async move {
        clog!("[WORKER] spawn: creating PlayerConfig");
        let config = PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS);
        clog!("[WORKER] spawn: creating PlayerImpl");
        let player = Arc::new(PlayerImpl::new(config));
        clog!("[WORKER] spawn: PlayerImpl created, entering command loop");

        loop {
            match cmd_rx.recv_async().await {
                Ok(cmd) => {
                    dispatch_cmd(cmd, &player).await;
                }
                Err(_) => {
                    clog!("[WORKER] command channel closed, shutting down");
                    return;
                }
            }
        }
    });
}

async fn dispatch_cmd(cmd: WorkerCmd, player: &Arc<PlayerImpl>) {
    match cmd {
        WorkerCmd::SelectTrack { url, request_id } => {
            let result = handle_select_track(player, &url).await;
            crate::js_channel::send_reply(request_id, result);
        }
        WorkerCmd::Play => {
            player.play();
        }
        WorkerCmd::Pause => {
            player.pause();
        }
        WorkerCmd::Stop => {
            player.pause();
            let _ = player.seek_seconds(0.0);
        }
        WorkerCmd::Seek(ms) => {
            let _ = player.seek_seconds(ms.max(0.0) / 1000.0);
        }
        WorkerCmd::SetVolume(vol) => {
            player.set_volume(vol);
        }
        WorkerCmd::SetCrossfade(secs) => {
            player.set_crossfade_duration(secs);
        }
        WorkerCmd::SetEqGain { band, gain_db } => {
            let _ = player.set_eq_gain(band as usize, gain_db);
        }
        WorkerCmd::ResetEq => {
            let _ = player.reset_eq();
        }
        WorkerCmd::SetDucking(mode) => {
            let mode = match mode {
                1 => SessionDuckingMode::Soft,
                2 => SessionDuckingMode::Hard,
                _ => SessionDuckingMode::Off,
            };
            let _ = player.set_session_ducking(mode);
        }
    }
}

/// Load resource (async) and start playback.
async fn handle_select_track(player: &Arc<PlayerImpl>, url: &str) -> Result<(), String> {
    clog!("[WORKER] select_track: url={url}");

    let mut config = ResourceConfig::new(url).map_err(|e| format!("invalid URL: {e}"))?;
    // WASM always uses ephemeral storage. Increase LRU cache capacity for
    // smoother seek and ABR transitions (default 5 is too small for HLS with
    // segment throttle).
    if config.store.cache_capacity.is_none() {
        config.store.cache_capacity = NonZeroUsize::new(64);
    }
    // Share the engine's audio worker so all tracks decode on the same thread.
    config = config.with_worker(player.worker().clone());

    let mut resource = Resource::new(config)
        .await
        .map_err(|e| format!("resource load failed: {e:?}"))?;

    resource.preload().await;

    player
        .play_resource(resource)
        .map_err(|e| format!("play_resource: {e}"))?;

    clog!("[WORKER] select_track: playing");
    Ok(())
}
