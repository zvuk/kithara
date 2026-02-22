//! Play audio from an HLS stream.
//!
//! ```
//! cargo run -p kithara --example hls_audio --features rodio [URL]
//! ```
//!
//! Controls:
//! - Left/Right arrows: seek ±5 seconds
//! - Up/Down arrows: volume ±5%
//! - Ctrl+C: stop playback

use std::{env::args, error::Error};

#[path = "common/controls.rs"]
mod controls;
#[path = "common/tracing.rs"]
mod tracing_support;
#[path = "common/tui.rs"]
mod tui;

use kithara::prelude::*;
use tracing::info;
use url::Url;

#[tokio::main(flavor = "current_thread")]
#[cfg_attr(feature = "perf", hotpath::main)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_support::init_tracing(&["off"], true)?;

    let url: Url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string())
        .parse()?;

    info!("Opening HLS stream: {url}");

    let track = url.to_string();
    let bus = EventBus::new(128);
    let pool = ThreadPool::with_num_threads(2)?;
    let hls_config = HlsConfig::new(url)
        .with_thread_pool(pool)
        .with_events(bus.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });
    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(true);
    let audio = Audio::<Stream<Hls>>::new(config).await?;
    let total_duration = audio.duration();

    info!("Starting playback... (Press Ctrl+C to stop)");
    controls::play_with_controls(
        audio,
        bus.subscribe(),
        controls::UiConfig::new("hls", track).with_total_duration(total_duration),
    )
    .await
}
