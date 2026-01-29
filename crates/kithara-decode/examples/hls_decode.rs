//! Example: Play audio from an HLS stream using Decoder.
//!
//! This demonstrates the decode architecture:
//! - DecoderConfig::<Hls>::new(hls_config) creates config with stream settings
//! - Decoder::new(config) creates stream and decoder
//! - Decoder runs symphonia in separate thread with PCM buffer
//! - Decoder impl rodio::Source for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example hls_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_decode::{Decoder, DecoderConfig};
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
                .add_directive("kithara_decode=info".parse()?)
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

    // Create decoder via target API
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(128);
    let hls_config = HlsConfig::new(url).with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..Default::default()
    });
    let config = DecoderConfig::<Hls>::new(hls_config).with_events(events_tx);
    let decoder = Decoder::<Stream<Hls>>::new(config).await?;

    // Log events
    tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(ev) => info!(?ev),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    info!(skipped = n, "events receiver lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    info!("Starting playback...");

    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(1.0);
        sink.append(decoder);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
