use std::{env::args, error::Error, sync::Arc, thread};

use kithara_assets::{EvictConfig, asset_store};
use kithara_file::{FileSource, FileSourceOptions};
use kithara_io::Reader;
use tempfile::TempDir;
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_file=trace".parse()?)
                .add_directive("kithara_net=debug".parse()?)
                .add_directive("kithara_assets=debug".parse()?)
                .add_directive("kithara_storage=debug".parse()?),
        )
        .with_line_number(true)
        .with_file(true)
        .init();

    let url = args().nth(1).unwrap_or_else(|| {
        "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
         6 - Movement 2 Un poco andante.MP3"
            .to_string()
    });
    let url: Url = url.parse()?;

    // Asset store is required for kithara-file streaming (it uses assets-backed storage).
    let temp_dir = TempDir::new()?;
    let assets = asset_store(temp_dir.path().to_path_buf(), EvictConfig::default());

    // Open a file session (async byte source).
    let session = FileSource::open(url, FileSourceOptions::default(), Some(assets)).await?;

    // Build a sync `Read + Seek` for rodio by adapting the session into a `kithara-io::Source`.
    //
    // Important: `kithara-io::Reader` blocks on a Tokio runtime handle; create it here (inside
    // the Tokio runtime) and then move it into a dedicated blocking thread.
    let source = session.source().await?;
    let reader = Reader::new(Arc::new(source));

    // Spawn a blocking thread to play audio via rodio, consuming the sync reader.
    let playback_thread = thread::spawn(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        let (_stream, stream_handle) = rodio::OutputStream::try_default()?;
        let sink = rodio::Sink::try_new(&stream_handle)?;

        sink.append(rodio::Decoder::new(reader)?);
        sink.sleep_until_end();

        Ok(())
    });

    // Wait for playback to complete.
    playback_thread.join().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::Other, "rodio playback thread panicked")
    })??;

    Ok(())
}
