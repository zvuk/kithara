//! Example: Play audio from an HLS stream using streaming decode.
//!
//! This demonstrates the streaming decode architecture:
//! - HlsMediaSource provides media stream
//! - StreamDecoder reads bytes incrementally
//! - Decoding starts before full segment downloads
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example hls_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_decode::{MediaSource, StreamDecoder};
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

    // Open HLS media source (streaming architecture)
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

    info!("Creating stream decoder...");

    // Open media stream
    let stream = source.open()?;

    // Create stream decoder
    let mut decoder = StreamDecoder::new(stream)?;

    // Create channel for streaming PCM to rodio
    let (sample_tx, sample_rx) = kanal::bounded::<Vec<f32>>(16);

    info!("Starting decode loop...");

    // Decode loop - runs in blocking thread
    let decode_handle = tokio::task::spawn_blocking(move || {
        let mut chunks_decoded = 0;

        while let Ok(Some(chunk)) = decoder.decode_next() {
            chunks_decoded += 1;

            if !chunk.pcm.is_empty() {
                if chunks_decoded % 100 == 0 {
                    info!(
                        chunk = chunks_decoded,
                        frames = chunk.frames(),
                        sample_rate = chunk.spec.sample_rate,
                        channels = chunk.spec.channels,
                        "Decoded PCM chunk"
                    );
                }

                // Send to rodio via channel
                if sample_tx.send(chunk.pcm).is_err() {
                    // Receiver dropped (playback stopped)
                    break;
                }
            }
        }

        info!(total_chunks = chunks_decoded, "Decode loop finished");
    });

    // Play via rodio
    #[cfg(feature = "rodio")]
    {
        // Get initial spec from first decoded chunk
        let spec = kithara_decode::PcmSpec {
            sample_rate: 44100, // Default, will be overridden by decoder
            channels: 2,
        };

        let audio_source = kithara_decode::AudioSyncReader::new(sample_rx, spec);

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
        info!("Rodio feature not enabled, just decoding without playback");
        drop(sample_rx);
    }

    // Wait for decode loop to complete
    decode_handle.await?;

    info!("HLS decode example finished");

    Ok(())
}
