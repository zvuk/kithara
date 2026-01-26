//! Example: Play audio from a progressive HTTP file using SyncReader.
//!
//! This demonstrates progressive file playback with streaming decode.
//! Uses SyncReader + rodio::Decoder for efficient streaming.
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example file_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_file::{File, FileEvent, FileParams};
use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
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

    // Open file with SyncReader (efficient streaming)
    let reader =
        SyncReader::<StreamSource<File>>::open(url, params, SyncReaderParams::default()).await?;
    let mut events_rx = reader.events();

    // Monitor download/playback progress
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

    // Play via rodio in blocking thread
    #[cfg(feature = "rodio")]
    {
        let handle = tokio::task::spawn_blocking(move || {
            let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
            let sink = rodio::Sink::connect_new(stream_handle.mixer());
            sink.append(rodio::Decoder::new(reader)?);

            info!("Playing file...");
            sink.sleep_until_end();

            info!("Playback complete");
            Ok::<_, Box<dyn Error + Send + Sync>>(())
        });
        handle.await??;
    }

    #[cfg(not(feature = "rodio"))]
    {
        info!("Rodio feature not enabled, decoding without playback");
        drop(reader);
    }

    info!("File decode example finished");

    Ok(())
}
