//! Play audio from an HLS stream using rodio decoder.
//!
//! This demonstrates the Stream API:
//! - `Stream::<Hls>::new()` creates a Read + Seek stream
//! - `rodio::Decoder` handles audio decoding
//!
//! Run with:
//! ```
//! cargo run -p kithara --example hls_rodio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

#[path = "common/controls.rs"]
mod controls;
#[path = "common/tracing.rs"]
mod tracing_support;
#[path = "common/tui.rs"]
mod tui;

use kithara::prelude::*;
use rodio::Source as _;
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
    let config = HlsConfig::new(url)
        .with_thread_pool(pool)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        })
        .with_events(bus.clone());
    let stream = Stream::<Hls>::new(config).await?;
    let decoder = rodio::Decoder::new(stream)?;
    let total_duration = decoder.total_duration();

    info!("Starting playback... (Press Ctrl+C to stop)");
    controls::play_with_controls(
        decoder,
        bus.subscribe(),
        controls::UiConfig::new("hls-rodio", track).with_total_duration(total_duration),
    )
    .await
}
