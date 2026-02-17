//! Play audio from an HLS stream.
//!
//! ```
//! cargo run -p kithara --example hls_audio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara::prelude::*;
use tracing::{info, metadata::LevelFilter, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_audio=debug".parse()?)
                .add_directive("kithara_net=warn".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    let url: Url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string())
        .parse()?;

    info!("Opening HLS stream: {url}");

    let bus = EventBus::new(128);
    let mut events_rx = bus.subscribe();
    let pool = ThreadPool::with_num_threads(2)?;
    let hls_config = HlsConfig::new(url)
        .with_thread_pool(pool)
        .with_events(bus)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });
    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(true);
    let audio = Audio::<Stream<Hls>>::new(config).await?;

    info!("Starting playback... (Press Ctrl+C to stop)");

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let mut playback = tokio::task::spawn_blocking(move || {
        let out = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(out.mixer());
        sink.append(audio);
        info!("Playing...");

        while stop_rx.try_recv().is_err() && !sink.empty() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        sink.stop();
        info!("Playback stopped");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received, stopping...");
                let _ = stop_tx.send(());
                break;
            }
            recv = events_rx.recv() => {
                match recv {
                    Ok(ev) => info!(?ev),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => warn!(n, "events lagged"),
                    Err(_) => break,
                }
            }
            result = &mut playback => {
                result??;
                break;
            }
        }
    }

    Ok(())
}
