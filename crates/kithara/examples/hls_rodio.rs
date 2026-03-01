//! Play audio from an HLS stream using rodio decoder.
//!
//! Demonstrates: `Stream::<Hls>::new()` → `rodio::Decoder` → playback.
//!
//! ```
//! cargo run -p kithara --example hls_rodio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

#[path = "common/events.rs"]
mod events;
#[path = "common/playback.rs"]
mod playback;
#[path = "common/tracing.rs"]
mod tracing_support;

use kithara::prelude::*;
use rodio::Source as _;
use tracing::info;
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_support::init_tracing(&["info"], false)?;

    let url: Url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string())
        .parse()?;

    info!("Opening HLS: {url}");

    let bus = EventBus::new(128);
    let config = HlsConfig::new(url)
        .with_events(bus.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });
    let stream = Stream::<Hls>::new(config).await?;
    let decoder = rodio::Decoder::new(stream)?;
    info!(duration = ?decoder.total_duration(), "Ready");

    events::spawn_event_logger(bus.subscribe());
    playback::play_until_end(decoder).await
}
