use std::{env::args, error::Error, sync::Arc};

use kithara_assets::EvictConfig;
use kithara_hls::{CacheOptions, Hls, HlsOptions};
use kithara_stream::SyncReader;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_hls=debug".parse()?)
                .add_directive("kithara_stream=info".parse()?)
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
    let cancel = CancellationToken::new();

    let hls_options = HlsOptions {
        cache: CacheOptions {
            cache_dir: Some(temp_dir.path().to_path_buf()),
            evict_config: Some(EvictConfig::default()),
        },
        cancel: Some(cancel.clone()),
        ..Default::default()
    };

    // Open an HLS session (async byte source).
    let session = Hls::open(url, hls_options).await?;
    let events_rx = session.events();
    let source = session.source();

    let reader = SyncReader::new(Arc::new(source));

    tokio::spawn(async move {
        let mut events_rx = events_rx;
        while let Ok(ev) = events_rx.recv().await {
            info!(?ev, "Stream event");
        }
    });

    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(0.05);
        sink.append(rodio::Decoder::new(reader)?);
        sink.sleep_until_end();

        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
