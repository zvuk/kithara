//! Engine Worker entry point.
//!
//! Spawned via [`kithara_platform::spawn`]. Owns [`PlayerImpl`] and processes
//! commands from the main-thread bridge asynchronously.
//!
//! Session graph operations are transparently proxied to the main thread via
//! the remote session channel (the Worker's [`SessionClient`] is in `Remote` mode).

use std::sync::Arc;

use kithara_abr::{AbrMode, AbrOptions};
use kithara_hls::{Hls, HlsConfig};
use kithara_platform::Mutex;
use kithara_play::{PlayerConfig, PlayerImpl, Resource, ResourceConfig, SessionDuckingMode};
use kithara_stream::Stream;
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};
use url::Url;

use crate::commands::WorkerCmd;

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

const CROSSFADE_SECONDS: f32 = 5.0;

/// Blocking entry called inside a Web Worker thread (via `kithara_platform::spawn`).
///
/// Sets up an async event loop via [`kithara_platform::spawn_task`] and
/// returns immediately — the Worker stays alive until the async task finishes.
pub(crate) fn worker_main(
    cmd_rx: kithara_platform::sync::mpsc::Receiver<WorkerCmd>,
    event_log: Arc<Mutex<Vec<String>>>,
) {
    clog!("[WORKER] engine worker started");

    kithara_platform::spawn_task(async move {
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS),
        ));

        loop {
            let cmd = match cmd_rx.recv_async().await {
                Ok(cmd) => cmd,
                Err(_) => {
                    clog!("[WORKER] command channel closed, shutting down");
                    break;
                }
            };

            match cmd {
                WorkerCmd::SelectTrack { url, reply_tx } => {
                    let p = Arc::clone(&player);
                    let log = Arc::clone(&event_log);
                    kithara_platform::spawn_task(async move {
                        let result = handle_select_track(&p, &url, &log).await;
                        let _ = reply_tx.send(result);
                    });
                }
                WorkerCmd::Play => {
                    let p = Arc::clone(&player);
                    kithara_platform::spawn_blocking(move || p.play());
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
                    let p = Arc::clone(&player);
                    kithara_platform::spawn_blocking(move || p.set_volume(vol));
                }
                WorkerCmd::SetCrossfade(secs) => {
                    player.set_crossfade_duration(secs);
                }
                WorkerCmd::SetEqGain { band, gain_db } => {
                    let p = Arc::clone(&player);
                    kithara_platform::spawn_blocking(move || {
                        let _ = p.set_eq_gain(band as usize, gain_db);
                    });
                }
                WorkerCmd::ResetEq => {
                    let p = Arc::clone(&player);
                    kithara_platform::spawn_blocking(move || {
                        let _ = p.reset_eq();
                    });
                }
                WorkerCmd::SetDucking(mode) => {
                    let mode = match mode {
                        1 => SessionDuckingMode::Soft,
                        2 => SessionDuckingMode::Hard,
                        _ => SessionDuckingMode::Off,
                    };
                    let p = Arc::clone(&player);
                    kithara_platform::spawn_blocking(move || {
                        let _ = p.set_session_ducking(mode);
                    });
                }
                WorkerCmd::TestHlsRead => {
                    kithara_platform::spawn_task(async move {
                        hls_test_read().await;
                    });
                }
            }
        }

        clog!("[WORKER] engine worker exiting");
    });
}

/// Load resource (async) and start playback (sync — via `spawn_blocking`).
async fn handle_select_track(
    player: &Arc<PlayerImpl>,
    url: &str,
    event_log: &Arc<Mutex<Vec<String>>>,
) -> Result<(), String> {
    clog!("[WORKER] select_track: loading resource url={url}");

    let mut config = ResourceConfig::new(url).map_err(|e| format!("invalid URL: {e}"))?;
    // WASM always uses ephemeral storage. Increase LRU cache capacity for
    // smoother seek and ABR transitions (default 5 is too small for HLS with
    // segment throttle).
    if config.store.cache_capacity.is_none() {
        config.store.cache_capacity = std::num::NonZeroUsize::new(64);
    }

    let resource = Resource::new(config)
        .await
        .map_err(|e| format!("resource load failed: {e:?}"))?;

    clog!("[WORKER] select_track: resource loaded, subscribing events");
    let events_rx = resource.subscribe();
    let url_owned = url.to_owned();
    let log = Arc::clone(event_log);
    kithara_platform::spawn_task(async move {
        log_resource_events(events_rx, url_owned, log).await;
    });

    clog!("[WORKER] select_track: calling play_resource via spawn_blocking");
    let p = Arc::clone(player);
    kithara_platform::spawn_blocking(move || {
        p.play_resource(resource)
            .map_err(|e| format!("play_resource: {e}"))
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))??;

    clog!("[WORKER] select_track: done");
    Ok(())
}

async fn log_resource_events<T>(
    mut events_rx: tokio::sync::broadcast::Receiver<T>,
    url: String,
    event_log: Arc<Mutex<Vec<String>>>,
) where
    T: core::fmt::Debug + Clone + Send + 'static,
{
    loop {
        match events_rx.recv().await {
            Ok(ev) => {
                let line = format!("resource src={url} event={ev:?}");
                info!("{line}");
                push_event(&event_log, line);
            }
            Err(RecvError::Lagged(n)) => {
                let line = format!("resource src={url} events lagged n={n}");
                warn!("{line}");
                push_event(&event_log, line);
            }
            Err(RecvError::Closed) => break,
        }
    }
}

fn push_event(event_log: &Arc<Mutex<Vec<String>>>, line: String) {
    let mut events = event_log.lock_sync();
    events.push(line);
    const MAX_EVENTS: usize = 1024;
    if events.len() > MAX_EVENTS {
        let keep_from = events.len() - MAX_EVENTS;
        events.drain(0..keep_from);
    }
}

// -- HLS diagnostic read ----------------------------------------------------------

const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";

async fn hls_test_read() {
    clog!("[HLS-TEST] Creating stream for {}", HLS_URL_DEFAULT);

    let url = match Url::parse(HLS_URL_DEFAULT) {
        Ok(u) => u,
        Err(e) => {
            clog!("[HLS-TEST] URL PARSE ERROR: {}", e);
            return;
        }
    };

    let config = HlsConfig::new(url).with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..Default::default()
    });

    match Stream::<Hls>::new(config).await {
        Ok(stream) => {
            clog!("[HLS-TEST] Stream created, spawning read worker...");
            let result = kithara_platform::spawn_blocking(move || hls_read_loop(stream)).await;
            match result {
                Ok(Ok(total)) => {
                    clog!("[HLS-TEST] DONE total={} bytes", total);
                }
                Ok(Err(e)) => {
                    clog!("[HLS-TEST] READ ERROR: {}", e);
                }
                Err(_) => {
                    clog!("[HLS-TEST] WORKER PANICKED");
                }
            }
        }
        Err(e) => {
            clog!("[HLS-TEST] STREAM CREATE ERROR: {:?}", e);
        }
    }
}

fn hls_read_loop(mut stream: Stream<Hls>) -> std::io::Result<usize> {
    use std::io::Read;
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    let mut last_log = 0usize;
    let mut zero_count = 0u32;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => {
                zero_count += 1;
                clog!(
                    "[HLS-TEST] read returned 0 (#{}) at total={} bytes",
                    zero_count,
                    total
                );
                if zero_count >= 3 {
                    clog!("[HLS-TEST] 3 consecutive zero reads, treating as EOF");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(n) => {
                zero_count = 0;
                total += n;
                if total - last_log >= 100_000 {
                    clog!("[HLS-TEST] progress: {} bytes", total);
                    last_log = total;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                clog!(
                    "[HLS-TEST] variant change at total={} bytes, clearing fence",
                    total
                );
                stream.clear_variant_fence();
                zero_count = 0;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}
