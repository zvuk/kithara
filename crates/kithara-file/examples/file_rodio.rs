//! Example: Play audio from an HTTP file using rodio.
//!
//! This demonstrates the Stream API:
//! - `Stream::<File>::new()` creates a Read + Seek stream
//! - `rodio::Decoder` handles audio decoding
//!
//! Run with:
//! ```
//! cargo run -p kithara-file --example rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_file::{EventBus, File, FileConfig};
use kithara_stream::{Stream, ThreadPool};
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_file=info".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
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
    let url: Url = url.parse()?;

    info!("Opening file: {}", url);

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let pool = ThreadPool::with_num_threads(2)?;
    let config = FileConfig::new(url.into())
        .with_thread_pool(pool)
        .with_events(bus);
    let stream = Stream::<File>::new(config).await?;

    // Log events
    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            info!(?ev);
        }
    });

    info!("Starting playback...");

    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(1.0);
        sink.append(rodio::Decoder::new(stream)?);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
