//! Play audio from an HTTP file using rodio decoder.
//!
//! This demonstrates the Stream API:
//! - `Stream::<File>::new()` creates a Read + Seek stream
//! - `rodio::Decoder` handles audio decoding
//!
//! Run with:
//! ```
//! cargo run -p kithara --example file_rodio --features rodio [URL]
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
        .unwrap_or_else(|| "https://stream.silvercomet.top/track.mp3".to_string())
        .parse()?;

    info!("Opening file: {url}");

    let track = url.to_string();
    let bus = EventBus::new(128);

    let pool = ThreadPool::with_num_threads(2)?;
    let config = FileConfig::new(url.into())
        .with_thread_pool(pool)
        .with_events(bus.clone());
    let stream = Stream::<File>::new(config).await?;
    let decoder = rodio::Decoder::new(stream)?;
    let total_duration = decoder.total_duration();

    info!("Starting playback... (Press Ctrl+C to stop)");
    controls::play_with_controls(
        decoder,
        bus.subscribe(),
        controls::UiConfig::new("file-rodio", track).with_total_duration(total_duration),
    )
    .await
}
