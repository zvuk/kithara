//! Example: Play audio from an HLS stream using Audio pipeline.
//!
//! This demonstrates the audio architecture:
//! - AudioConfig::<Hls>::new(hls_config) creates config with stream settings
//! - Audio::new(config) creates stream and audio pipeline
//! - Audio runs symphonia in separate thread with PCM buffer
//! - Audio impl rodio::Source for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-audio --example hls_audio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_audio::{Audio, AudioConfig};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_audio=debug".parse()?)
                .add_directive("kithara_hls=info".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=warn".parse()?)
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

    // Create audio pipeline
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(128);
    let hls_config = HlsConfig::new(url).with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..Default::default()
    });
    let config = AudioConfig::<Hls>::new(hls_config).with_events(events_tx);
    let audio = Audio::<Stream<Hls>>::new(config).await?;

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
        sink.append(audio);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
