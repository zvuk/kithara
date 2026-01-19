//! Example: Play audio from a progressive HTTP file using kithara-decode.
//!
//! This demonstrates the integration between kithara-file and kithara-decode,
//! including optional resampling and speed control.
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example file_decode --features rodio [URL] [SPEED]
//! ```
//!
//! Speed is optional (default 1.0). Examples:
//! - 1.5 for 1.5x playback speed
//! - 0.75 for 0.75x playback speed

use std::{env::args, error::Error, sync::Arc};

use kithara_decode::{AudioSyncReader, Pipeline};
use kithara_file::{File, FileEvent, FileParams};
use kithara_stream::StreamSource;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

const TARGET_SAMPLE_RATE: u32 = 44100;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_decode=debug".parse()?)
                .add_directive("kithara_file=info".parse()?)
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

    let speed: f32 = args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0.5);

    info!("Opening file: {}", url);
    info!("Playback speed: {}x", speed);

    let params = FileParams::default();

    // Open StreamSource
    let source = StreamSource::<File>::open(url, params).await?;
    let source_arc = Arc::new(source);

    // Subscribe to events
    let mut events_rx = source_arc.events();
    tokio::spawn(async move {
        while let Ok(msg) = events_rx.recv().await {
            match msg {
                FileEvent::DownloadProgress { offset, percent } => {
                    info!(offset, ?percent, "Download progress");
                }
                FileEvent::PlaybackProgress { position, percent } => {
                    info!(position, ?percent, "Playback progress");
                }
            }
        }
    });

    info!("Creating audio pipeline...");

    // Create unified pipeline (decoder + resampler in one)
    let pipeline = Pipeline::open(source_arc, TARGET_SAMPLE_RATE).await?;

    let output_spec = pipeline.output_spec();
    info!(
        sample_rate = output_spec.sample_rate,
        channels = output_spec.channels,
        "Pipeline created"
    );

    // Set playback speed
    pipeline.set_speed(speed)?;

    info!(
        target_rate = TARGET_SAMPLE_RATE,
        speed,
        "Speed configured"
    );

    // Create rodio adapter from buffer
    let audio_source = AudioSyncReader::new(
        pipeline.consumer().clone(),
        pipeline.buffer().clone(),
        output_spec,
    );

    // Play via rodio in blocking thread
    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.append(audio_source);

        info!("Playing at {}x speed...", speed);
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
