use std::{env, error::Error, io::Read as _, sync::Arc, thread, time::Duration};

use kithara_assets::{AssetStore, EvictConfig};
use kithara_hls::{HlsOptions, HlsSource};
use kithara_io::Reader;
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                // You can override these via `RUST_LOG=...`
                // (e.g. `kithara_hls=trace,kithara_io=trace,kithara_net=debug,kithara_storage=debug,kithara_assets=debug`)
                .add_directive("kithara_hls=debug".parse()?)
                .add_directive("kithara_io=trace".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive("kithara_storage=info".parse()?)
                .add_directive("kithara_assets=info".parse()?),
        )
        .with_line_number(true)
        .with_file(true)
        .init();

    // Same URL as `stream-download-hls/examples/hls.rs`.
    let url = "https://stream.silvercomet.top/hls/master.m3u8";
    let url: Url = url.parse()?;

    // Persistent disk cache root (deterministic layout inside; see `kithara-hls` cache key helpers).
    let cache_root = env::temp_dir().join("hls-cache");
    let assets = AssetStore::with_root_dir(cache_root, EvictConfig::default());

    // Open an HLS session (async byte stream + caching).
    let session = HlsSource::open(url, HlsOptions::default(), assets).await?;

    // Build a sync `Read + Seek` for rodio.
    //
    // Note: `kithara-io::Reader` blocks on a Tokio runtime handle; create it here (inside the
    // Tokio runtime) and then move it into a dedicated blocking thread.
    let source = session.source().await?;
    let reader = Reader::new(Arc::new(source));

    // Watchdog to verify the process is alive while we're potentially blocked in I/O.
    // We intentionally don't try to "stop" it cleanly; when `main` returns, the process exits.
    let _watchdog_thread = thread::spawn(move || {
        for i in 1u64.. {
            eprintln!("[watchdog] alive: {}s", i);
            thread::sleep(Duration::from_secs(1));
        }
    });

    let playback_thread = thread::spawn(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        eprintln!("[playback] thread started");
        let (_stream, stream_handle) = rodio::OutputStream::try_default()?;
        let sink = rodio::Sink::try_new(&stream_handle)?;

        eprintln!("[playback] pre-probe: reading 4 bytes synchronously");
        let mut reader = reader;
        let mut probe = [0u8; 4];
        reader.read_exact(&mut probe)?;
        eprintln!(
            "[playback] pre-probe: first 4 bytes = {:02x} {:02x} {:02x} {:02x}",
            probe[0], probe[1], probe[2], probe[3]
        );
        // Reset to the start so the decoder sees the full stream from byte 0.
        let _ = std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))?;

        eprintln!("[playback] decoder init begin");
        sink.append(rodio::Decoder::new(reader)?);
        eprintln!("[playback] decoder init done; waiting until end");
        sink.sleep_until_end();
        eprintln!("[playback] sink finished");

        Ok(())
    });

    playback_thread.join().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::Other, "rodio playback thread panicked")
    })??;

    Ok(())
}
