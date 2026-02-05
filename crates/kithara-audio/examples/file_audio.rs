//! Example: Play audio from a progressive HTTP file using Audio pipeline.
//!
//! This demonstrates the audio architecture:
//! - AudioConfig::<File>::new(file_config) creates config with stream settings
//! - Audio::new(config) creates stream and audio pipeline
//! - Audio runs symphonia in separate thread with PCM buffer
//! - Audio impl rodio::Source for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-audio --example file_audio --features rodio,apple [URL]
//! ```

use std::{env::args, error::Error};

use kithara_audio::{Audio, AudioConfig};
use kithara_file::{File, FileConfig};
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
#[hotpath::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_audio=debug".parse()?)
                .add_directive("kithara_file=info".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=warn".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
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

    // Create audio pipeline
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(128);
    let hint = url.path().rsplit('.').next().map(|ext| ext.to_lowercase());
    let file_config = FileConfig::new(url.into());
    let mut config = AudioConfig::<File>::new(file_config)
        .with_prefer_hardware(true)
        .with_events(events_tx);
    if let Some(ext) = hint {
        config = config.with_hint(ext);
    }
    let audio = Audio::<Stream<File>>::new(config).await?;

    // Log events
    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            info!(?ev);
        }
    });

    info!("Starting playback... (Press Ctrl+C to stop)");

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let playback_handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(0.02);
        sink.append(audio);

        info!("Playing...");

        // Wait for stop signal or until track ends
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            if sink.empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        sink.stop();
        info!("Playback stopped");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    // Wait for Ctrl+C or playback completion
    let mut playback_done = false;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, stopping playback...");
            let _ = stop_tx.send(());
        }
        result = playback_handle => {
            result??;
            playback_done = true;
        }
    }

    // If we sent stop signal, wait for playback to finish
    if !playback_done {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(())
}
