//! Example: Play audio using Resource with auto-detection.
//!
//! Demonstrates the top-level `Resource` API:
//! - `ResourceConfig::new(url)` parses URL and applies defaults
//! - `Resource::new(config)` auto-detects file vs HLS and creates the decoder
//! - `Resource` implements `rodio::Source` for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara --example resource_play --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara::{Resource, ResourceConfig, ThreadPool};
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "current_thread")]
#[cfg_attr(feature = "perf", hotpath::main)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara=info".parse()?)
                .add_directive("kithara_decode=info".parse()?)
                .add_directive("kithara_file=info".parse()?)
                .add_directive("kithara_hls=info".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=warn".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    let url = args().nth(1).unwrap_or_else(|| {
        "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
         6 - Movement 2 Un poco andante.MP3"
            .to_string()
    });

    info!("Opening: {}", url);

    let pool = ThreadPool::with_num_threads(2)?;
    let config = ResourceConfig::new(&url)?.with_thread_pool(pool);
    let resource = Resource::new(config).await?;

    info!(spec = ?resource.spec(), "Format detected");

    // Subscribe to events
    let mut events = resource.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = events.recv().await {
            info!(?ev);
        }
    });

    info!("Starting playback...");

    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(1.0);
        sink.append(resource);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
