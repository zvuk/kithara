//! Example: Play audio from an HLS stream using decode pipeline.
//!
//! This demonstrates the decode pipeline architecture:
//! - HlsMediaSource provides media stream
//! - DecodePipeline runs decoder in separate thread with PCM buffer
//! - Smooth playback during variant switches
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example hls_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_decode::{DecodePipeline, DecodePipelineConfig, MediaSource};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsEvent, HlsParams};
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

    // Open HLS media source
    let source = Hls::open_media_source(url, hls_params).await?;

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

    // Open media stream
    let stream = source.open()?;

    info!("Creating decode pipeline...");

    // Create decode pipeline (runs decoder in separate thread with PCM buffer)
    let config = DecodePipelineConfig {
        pcm_buffer_chunks: 20, // ~2 seconds buffer
    };
    let pipeline = DecodePipeline::new(stream, config)?;

    info!("Starting playback...");

    // Play via rodio
    #[cfg(feature = "rodio")]
    {
        let pcm_rx = pipeline.pcm_rx_clone();

        // Get initial spec
        let spec = kithara_decode::PcmSpec {
            sample_rate: 44100,
            channels: 2,
        };

        let audio_source = kithara_decode::AudioSyncReader::new(pcm_rx, spec);

        // Play via rodio in blocking thread
        let play_handle = tokio::task::spawn_blocking(move || {
            let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
            let sink = rodio::Sink::connect_new(stream_handle.mixer());
            sink.set_volume(0.3);
            sink.append(audio_source);

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
        while pipeline.is_running() {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    // Pipeline dropped here, decode thread will be joined
    drop(pipeline);

    info!("HLS decode example finished");

    Ok(())
}
