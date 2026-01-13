use std::{env::args, error::Error, sync::Arc};

use kithara_assets::{AssetStore, EvictConfig};
use kithara_file::{FileEvent, FileSource, FileSourceOptions};
use kithara_stream::SyncReader;
use tempfile::TempDir;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_file=info".parse()?)
                .add_directive("kithara_stream::io=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive("kithara_storage=info".parse()?)
                .add_directive("kithara_assets=info".parse()?)
                .add_directive(LevelFilter::INFO.into()),
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

    let mut events_rx = source.events();
    let reader = SyncReader::new(Arc::new(source));

    tokio::spawn(async move {
        while let Ok(msg) = events_rx.recv().await {
            match msg {
                FileEvent::DownloadProgress { offset, percent } => {
                    info!(offset, ?percent, "Stream event: download");
                }
                FileEvent::PlaybackProgress { position, percent } => {
                    info!(position, ?percent, "Stream event: playback");
                }
            }
        }
    });

    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.append(rodio::Decoder::new(reader)?);
        sink.sleep_until_end();
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    handle.await??;

    Ok(())
}
