//! Play audio from a progressive HTTP file.
//!
//! ```
//! cargo run -p kithara --example file_audio --features rodio [URL]
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
        .unwrap_or_else(|| "https://stream.silvercomet.top/track.mp3".to_string())
        .parse()?;

    info!("Opening file: {url}");

    let track = url.to_string();
    let bus = EventBus::new(128);
    let hint = url.path().rsplit('.').next().map(str::to_lowercase);
    let pool = ThreadPool::with_num_threads(2)?;
    let mut config = AudioConfig::<File>::new(
        FileConfig::new(url.into())
            .with_thread_pool(pool)
            .with_events(bus.clone()),
    )
    .with_prefer_hardware(false);
    if let Some(ext) = hint {
        config = config.with_hint(ext);
    }
    let audio = Audio::<Stream<File>>::new(config).await?;
    let total_duration = audio.duration();

    info!("Starting playback... (Press Ctrl+C to stop)");
    controls::play_with_controls(
        audio,
        bus.subscribe(),
        controls::UiConfig::new("file", track).with_total_duration(total_duration),
    )
    .await
}
