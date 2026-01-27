//! Example: Play audio from an HLS stream using Decoder.
//!
//! This demonstrates the decode architecture:
//! - Stream::<Hls>::new() creates HlsInner (Read + Seek)
//! - Decoder runs symphonia in separate thread with PCM buffer
//! - Decoder impl rodio::Source for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example hls_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_decode::{Decoder, DecoderConfig};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig, HlsEvent, HlsParams};
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_decode=debug".parse()?)
                .add_directive("kithara_hls=debug".parse()?)
                .add_directive("kithara_stream=debug".parse()?)
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

    // Create events channel
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(32);

    let hls_params = HlsParams::default()
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        })
        .with_events(events_tx);

    // Create HLS config
    let config = HlsConfig::new(url).with_params(hls_params);

    // Create Stream via generic API
    let stream = Stream::<Hls>::new(config).await?;

    // Subscribe to HLS events
    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            match ev {
                HlsEvent::VariantApplied {
                    from_variant,
                    to_variant,
                    reason,
                } => {
                    info!(
                        ?reason,
                        "Variant switch: {} -> {}", from_variant, to_variant
                    );
                }
                HlsEvent::SegmentComplete {
                    segment_index,
                    variant,
                    bytes_transferred,
                    ..
                } => {
                    info!(
                        segment_index,
                        variant, bytes_transferred, "Segment complete"
                    );
                }
                HlsEvent::EndOfStream => {
                    info!("End of stream");
                    break;
                }
                _ => {}
            }
        }
    });

    info!("Creating decoder...");

    // Create decoder with streaming config
    let decoder_config = DecoderConfig::streaming();
    let decoder = Decoder::new(stream, decoder_config)?;

    info!("Starting playback...");

    // Play via rodio - Decoder impl rodio::Source
    #[cfg(feature = "rodio")]
    {
        let play_handle = tokio::task::spawn_blocking(move || {
            let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
            let sink = rodio::Sink::connect_new(stream_handle.mixer());
            sink.set_volume(0.3);
            sink.append(decoder);

            info!("Playing HLS stream via rodio...");
            sink.sleep_until_end();

            info!("Playback complete");
            Ok::<_, Box<dyn Error + Send + Sync>>(())
        });

        play_handle.await??;
    }

    #[cfg(not(feature = "rodio"))]
    {
        info!("Rodio feature not enabled, waiting for decode to complete...");
        while decoder.is_running() {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    info!("HLS decode example finished");

    Ok(())
}
