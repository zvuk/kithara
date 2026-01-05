use std::{env::args, error::Error, sync::Arc, thread};

use kithara_assets::{AssetStore, EvictConfig};
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
                // You can override these via `RUST_LOG=...` (e.g. `kithara_file=trace,kithara_io=trace,kithara_file::internal::fetcher=debug`)
                .add_directive("kithara_file=info".parse()?)
                .add_directive("kithara_file::internal::fetcher=debug".parse()?)
                .add_directive("kithara_file::internal::feeder=debug".parse()?)
                .add_directive("kithara_io=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive("kithara_storage=info".parse()?)
                .add_directive("kithara_assets=info".parse()?),
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

    let temp_dir = TempDir::new()?;
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());

    // Open a file session (async byte source).
    let session = FileSource::open(url, FileSourceOptions::default(), Some(assets)).await?;
    let source = session.source().await?;
    let reader = Reader::new(Arc::new(source));

    let playback_thread = thread::spawn(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        let (_stream, stream_handle) = rodio::OutputStream::try_default()?;
        let sink = rodio::Sink::try_new(&stream_handle)?;

        sink.append(rodio::Decoder::new(reader)?);
        sink.sleep_until_end();

        Ok(())
    });

    playback_thread.join().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::Other, "rodio playback thread panicked")
    })??;

    Ok(())
}
