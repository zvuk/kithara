use std::{env::args, error::Error, sync::Arc};

use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams};
use kithara_stream::{SyncReader, SyncReaderParams};
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
    let hls_params = HlsParams::default().with_abr(AbrOptions {
        mode: AbrMode::Manual(0),
        ..Default::default()
    });

    // Open an HLS source (async byte source).
    let source = Hls::open(url, hls_params).await?;
    let events_rx = source.events();

    let reader = SyncReader::new(Arc::new(source), SyncReaderParams::default());

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
