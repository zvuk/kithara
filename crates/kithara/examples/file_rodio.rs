//! Play audio from an HTTP file using rodio decoder.
//!
//! Demonstrates: `Stream::<File>::new()` → `rodio::Decoder` → playback.
//!
//! ```
//! cargo run -p kithara --example file_rodio --features rodio [URL]
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
        .unwrap_or_else(|| "https://stream.silvercomet.top/track.mp3".to_string())
        .parse()?;

    info!("Opening file: {url}");

    let bus = EventBus::new(128);
    let config = FileConfig::new(url.into()).with_events(bus.clone());
    let stream = Stream::<File>::new(config).await?;
    let decoder = rodio::Decoder::new(stream)?;
    info!(duration = ?decoder.total_duration(), "Ready");

    events::spawn_event_logger(bus.subscribe());
    playback::play_until_end(decoder).await
}
