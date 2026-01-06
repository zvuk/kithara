use std::{env::args, error::Error, sync::Arc};

use kithara_assets::{AssetStore, EvictConfig};
use kithara_hls::{HlsOptions, HlsSource};
use kithara_stream::io::Reader;
use tempfile::TempDir;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_hls=debug".parse()?)
                .add_directive("kithara_streamå=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive("kithara_storage=info".parse()?)
                .add_directive("kithara_assets=info".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(true)
        .with_file(true)
        .init();

    let url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string());

    let url: Url = url.parse()?;
    let temp_dir = TempDir::new()?;
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());

    // Open an HLS session (async byte source).
    let session = HlsSource::open(url, HlsOptions::default(), assets).await?;
    let source = session.source().await?;

    let mut events_rx = session.events();
    let reader = Reader::new(Arc::new(source));

    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            info!(?ev, "Stream event");
        }
    });

    let handle = tokio::task::spawn_blocking(move || {
        let (_stream, stream_handle) = rodio::OutputStream::try_default()?;
        let sink = rodio::Sink::try_new(&stream_handle)?;

        sink.append(rodio::Decoder::new(reader)?);
        sink.sleep_until_end();

        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    handle.await??;

    Ok(())
}
