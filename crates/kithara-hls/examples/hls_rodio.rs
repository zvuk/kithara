//! Example: Play audio from an HLS stream using rodio.
//!
//! This demonstrates the Stream API:
//! - `Stream::<Hls>::new()` creates a Read + Seek stream
//! - `rodio::Decoder` handles audio decoding
//!
//! Run with:
//! ```
//! cargo run -p kithara-hls --example hls --features rodio [URL]
//! ```

use std::{env::args, error::Error, time::Duration};

use kithara_hls::{AbrMode, AbrOptions, EventBus, Hls, HlsConfig};
use kithara_platform::ThreadPool;
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "current_thread")]
#[cfg_attr(feature = "perf", hotpath::main)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_hls=info".parse()?)
                .add_directive("kithara_abr=debug".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    let url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string());
    let url: Url = url.parse()?;

    info!("Opening HLS stream: {}", url);

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let pool = ThreadPool::with_num_threads(2)?;
    let config = HlsConfig::new(url)
        .with_thread_pool(pool)
        .with_abr(AbrOptions {
            // Aggressive ABR for testing
            min_buffer_for_up_switch_secs: 0.0, // no buffer requirement
            min_switch_interval: Duration::from_secs(3), // switch every 3s
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.1, // use 91% of throughput
            up_hysteresis_ratio: 1.05,     // 5% headroom
            ..Default::default()
        })
        .with_events(bus);
    let stream = Stream::<Hls>::new(config).await?;

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
