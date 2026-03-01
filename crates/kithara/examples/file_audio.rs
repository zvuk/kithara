//! Play audio from an HTTP file using the built-in Symphonia decoder.
//!
//! ```
//! cargo run -p kithara --example file_audio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

#[path = "common/events.rs"]
mod events;
#[path = "common/playback.rs"]
mod playback;
#[path = "common/tracing.rs"]
mod tracing_support;

use kithara::prelude::*;
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
    let config = AudioConfig::<File>::new(FileConfig::new(url.into()).with_events(bus.clone()));
    let audio = Audio::<Stream<File>>::new(config).await?;
    info!(duration = ?audio.duration(), "Ready");

    events::spawn_event_logger(bus.subscribe());
    playback::play_until_end(audio).await
}
