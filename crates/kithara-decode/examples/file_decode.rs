//! Example: Play audio from a progressive HTTP file using kithara-decode.
//!
//! This demonstrates the integration between kithara-file and kithara-decode.
//! AudioPipeline works directly with Source.
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example file_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error, sync::Arc};

use kithara_decode::{AudioPipeline, AudioSyncReader};
use kithara_file::{File, FileEvent, FileParams};
use kithara_stream::StreamSource;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

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

    info!("Opening file: {}", url);

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

    // Create pipeline directly from Source
    let mut pipeline = AudioPipeline::open(source_arc).await?;

    info!(
        sample_rate = pipeline.spec().sample_rate,
        channels = pipeline.spec().channels,
        "Pipeline created"
    );

    // Create AudioSyncReader from pipeline
    let spec = pipeline.spec();
    let audio_rx = pipeline
        .take_audio_receiver()
        .ok_or("Audio receiver already taken")?;
    let audio_source = AudioSyncReader::new(audio_rx, spec);

    // Play via rodio in blocking thread
    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.append(audio_source);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
