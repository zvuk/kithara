use arc_swap::ArcSwap;
use kithara::{
    host::{HostConfig, wasm},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep},
        tokio::{sync::mpsc, task},
    },
    play::wasm as play_wasm,
};
use tracing::Level;
use tracing_log::LogTracer;
use tracing_wasm::WASMLayerConfigBuilder;

use super::worker;
use crate::{
    document::Config,
    engine::{EngineError, EngineSnapshot},
    gui::{Boot, FrontendError, immediate},
    pools::{self, AppHost, AppPools},
    theme::Palette,
};

/// Starts the audio host and the engine Worker, then the studio on the page
/// once the Worker reports the engine built. Returns once the engine stopped,
/// with `shutdown` cancelled.
///
/// # Errors
/// Returns an error when the document, the pools, the audio host, the studio
/// or the engine cannot be built, or when the event loop fails.
pub async fn run(shutdown: CancelToken) -> Result<(), FrontendError> {
    let _ = LogTracer::init();
    tracing_wasm::set_as_global_default_with_config(
        WASMLayerConfigBuilder::new()
            .set_report_logs_in_timings(false)
            .set_max_level(Level::INFO)
            .build(),
    );

    let document = Config::load(None, None)?;
    let app = document.app();
    let mut palette = Palette::default();
    palette.apply(app.palette);
    let pools = pools::build(&document.pools())?;
    let host = AppHost::new(
        HostConfig::builder()
            .maybe_sample_rate_hint(app.sample_rate.flatten())
            .maybe_output_block_frames(app.output_block_frames.flatten())
            .build(),
    )?;
    let (sender, receiver) = wasm::worker_host_channel(&host)?;
    play_wasm::spawn_webcodecs_probe(pools.clone());
    wasm::warm_up_audio(&host)?;
    let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
    let (commands, received) = mpsc::unbounded_channel();
    let boot = Boot::builder()
        .maybe_package(app.ui_package.as_ref().and_then(Option::as_deref))
        .settings(&document.ui()?)
        .tracks(document.tracks().to_vec())
        .palette(palette)
        .snapshots(Arc::clone(&snapshots))
        .commands(commands)
        .chrome_hidden(true)
        .build()?;

    task::spawn(pump(host, receiver, shutdown.child()));
    let (built, stopped) = worker::spawn(
        document,
        pools,
        sender,
        snapshots,
        received,
        shutdown.clone(),
    );
    let started = built
        .await
        .unwrap_or(Err(EngineError::Panicked))
        .map_err(FrontendError::from)
        .and_then(|()| immediate(boot));
    let Err(_) = stopped.await;
    shutdown.cancel();
    started
}

async fn pump(host: AppHost, receiver: wasm::HostReceiver<AppPools>, cancel: CancelToken) {
    const INTERVAL: Duration = Duration::from_millis(16);

    let _host = host;
    while !cancel.is_cancelled() {
        wasm::tick_and_poll(&receiver);
        sleep(INTERVAL).await;
    }
}
