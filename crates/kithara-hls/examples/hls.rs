//! Example: Play audio from an HLS stream using rodio.
//!
//! This demonstrates the Stream API:
//! - Stream::<Hls>::new() creates a Read + Seek stream
//! - rodio::Decoder handles audio decoding
//!
//! Run with:
//! ```
//! cargo run -p kithara-hls --example hls --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig, HlsEvent, HlsParams};
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_hls=debug".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
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
            mode: AbrMode::Manual(0),
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

    info!("Starting playback...");

    // Play via rodio (rodio::Decoder handles audio decoding)
    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(0.1);
        sink.append(rodio::Decoder::new(stream)?);

        info!("Playing HLS stream via rodio...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
