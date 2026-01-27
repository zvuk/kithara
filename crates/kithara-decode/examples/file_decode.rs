//! Example: Play audio from a progressive HTTP file using DecodePipeline.
//!
//! This demonstrates progressive file playback with kithara-decode:
//! - FileMediaSource provides media stream
//! - DecodePipeline runs decoder in separate thread with PCM buffer
//! - AudioSyncReader bridges to rodio for playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example file_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_decode::{DecodePipeline, DecodePipelineConfig, MediaSource};
use kithara_file::{FileEvent, FileMediaSource, FileParams};
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_decode=debug".parse()?)
                .add_directive("kithara_file=debug".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
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

    // Open file media source (streaming architecture)
    let source = FileMediaSource::open(url, FileParams::default()).await?;

    // Subscribe to file events
    let mut events_rx = source.events();
    tokio::spawn(async move {
        while let Ok(msg) = events_rx.recv().await {
            match msg {
                FileEvent::DownloadProgress { offset, percent } => {
                    if offset % (1024 * 1024) < 65536 {
                        // Log every ~1MB
                        info!(offset, ?percent, "Download progress");
                    }
                }
                FileEvent::PlaybackProgress { .. } => {
                    // Don't log playback progress to reduce noise
                }
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

            info!("Playing file via rodio...");
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

    info!("File decode example finished");

    Ok(())
}
