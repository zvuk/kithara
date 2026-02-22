//! Example: Play audio using Resource with auto-detection.
//!
//! Demonstrates the top-level `Resource` API:
//! - `ResourceConfig::new(url)` parses URL and applies defaults
//! - `Resource::new(config)` auto-detects file vs HLS and creates the decoder
//! - `Resource` implements `rodio::Source` for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara --example resource_rodio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

#[path = "common/events.rs"]
mod events;
#[path = "common/playback.rs"]
mod playback;
#[path = "common/tracing.rs"]
mod tracing_support;

use kithara::prelude::{Resource, ResourceConfig, ThreadPool};
use tracing::info;

#[tokio::main(flavor = "current_thread")]
#[cfg_attr(feature = "perf", hotpath::main)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_support::init_tracing(
        &[
            "kithara=info",
            "kithara_decode=info",
            "kithara_file=info",
            "kithara_hls=info",
            "kithara_stream=info",
            "kithara_net=warn",
            "symphonia_format_isomp4=warn",
        ],
        false,
    )?;

    let url = args().nth(1).unwrap_or_else(|| {
        "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
         6 - Movement 2 Un poco andante.MP3"
            .to_string()
    });

    info!("Opening: {url}");

    let pool = ThreadPool::with_num_threads(2)?;
    let config = ResourceConfig::new(&url)?.with_thread_pool(pool);
    let resource = Resource::new(config).await?;
    info!(spec = ?resource.spec(), "Format detected");

    events::spawn_event_logger(resource.subscribe());

    info!("Starting playback...");
    playback::play_until_end(resource).await
}
