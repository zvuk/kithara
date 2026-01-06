use std::{env::args, error::Error, sync::Arc, thread};

use kithara_assets::{AssetStore, EvictConfig};
use kithara_file::{FileEvent, FileSource, FileSourceOptions};
use kithara_io::Reader;
use kithara_stream::StreamMsg;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_file=info".parse()?)
                .add_directive("kithara_io=info".parse()?)
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
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<StreamMsg<(), FileEvent>>();

    tokio::spawn(async move {
        while let Some(ev) = events_rx.recv().await.ok() {
            let _ = event_tx.send(StreamMsg::Event(ev));
        }
    });

    tokio::spawn(async move {
        while let Some(msg) = event_rx.recv().await {
            if let StreamMsg::Event(ev) = msg {
                match ev {
                    FileEvent::DownloadProgress { offset, percent } => {
                        info!(offset, ?percent, "Stream event: download");
                    }
                    FileEvent::PlaybackProgress { position, percent } => {
                        info!(position, ?percent, "Stream event: playback");
                    }
                }
            }
        }
    });

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
