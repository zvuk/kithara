//! Play audio from an AES-128 encrypted HLS stream.
//!
//! ```
//! cargo run -p kithara --example hls_drm_audio --features rodio [URL]
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
        .unwrap_or_else(|| "https://stream.silvercomet.top/drm/master.m3u8".to_string())
        .parse()?;

    info!("Opening encrypted HLS: {url}");

    let bus = EventBus::new(128);
    let config = AudioConfig::<Hls>::new(HlsConfig::new(url).with_events(bus.clone()).with_abr(
        AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        },
    ));
    let audio = Audio::<Stream<Hls>>::new(config).await?;
    info!(duration = ?audio.duration(), "Ready");

    events::spawn_event_logger(bus.subscribe());
    playback::play_until_end(audio).await
}
