//! Example: Play audio from an HLS stream using stream-based architecture.
//!
//! This demonstrates the new StreamPipeline architecture:
//! - HlsWorkerSource → GenericStreamDecoder → PCM output (3-loop design)
//! - Stream messages with full metadata (variant, codec, boundaries)
//! - Generic decoder that works with any StreamMetadata implementation
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example hls_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error, sync::Arc};

use bytes::Bytes;
use kithara_decode::{GenericStreamDecoder, PcmBuffer, StreamPipeline};
use kithara_hls::{worker::HlsSegmentMetadata, AbrMode, AbrOptions, Hls, HlsEvent, HlsParams};
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

    let hls_params = HlsParams::default().with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..Default::default()
    });

    // Open HLS stream and get worker source
    let (worker_source, mut events_rx) = Hls::open_stream(url, hls_params).await?;

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

    info!("Creating stream decoder and pipeline...");

    // Create generic stream decoder for HLS
    let decoder = GenericStreamDecoder::<HlsSegmentMetadata, Bytes>::new();

    // Create StreamPipeline (connects worker → decoder → PCM output)
    let pipeline = StreamPipeline::new(worker_source, decoder).await?;

    info!("Pipeline created, consuming PCM chunks...");

    // Create simple PCM buffer for accumulating audio
    let spec = kithara_decode::PcmSpec {
        sample_rate: 44100,
        channels: 2,
    };
    let (buffer, sample_rx) = PcmBuffer::new(spec);
    let buffer = Arc::new(buffer);

    // Spawn consumer task for PCM chunks
    let pcm_rx = pipeline.pcm_rx().clone();
    let buffer_clone = buffer.clone();
    let _consumer_handle = tokio::spawn(async move {
        let mut chunks_received = 0;
        loop {
            match pcm_rx.recv() {
                Ok(chunk) => {
                    chunks_received += 1;
                    if !chunk.pcm.is_empty() {
                        info!(
                            chunk = chunks_received,
                            frames = chunk.frames(),
                            sample_rate = chunk.spec.sample_rate,
                            channels = chunk.spec.channels,
                            "Received PCM chunk"
                        );

                        // STREAMING MODE: Send to rodio via channel (no Vec accumulation)
                        buffer_clone.append(&chunk);
                    }
                }
                Err(_) => {
                    info!("PCM channel closed");
                    break;
                }
            }
        }
        info!(total_chunks = chunks_received, "Consumer finished");
    });

    // Create rodio adapter from sample channel
    #[cfg(feature = "rodio")]
    {
        let audio_source = kithara_decode::AudioSyncReader::new(sample_rx, buffer.clone(), spec);

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
        info!("Rodio feature not enabled, just consuming PCM chunks without playback");
        // Wait for consumer to finish
        _consumer_handle.await?;
    }

    // Wait for pipeline to complete
    pipeline.wait().await?;

    info!("HLS decode example finished");

    Ok(())
}
