//! Play audio from a progressive HTTP file.
//!
//! ```
//! cargo run -p kithara-audio --example file_audio --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_audio::{Audio, AudioConfig};
use kithara_file::{File, FileConfig};
use kithara_stream::Stream;
use tokio::sync::broadcast;
use tracing::{info, metadata::LevelFilter, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "current_thread")]
#[hotpath::main]
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
        .unwrap_or_else(|| {
            "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 \
             No 6 - Movement 2 Un poco andante.MP3"
                .to_string()
        })
        .parse()?;

    info!("Opening file: {url}");

    let (events_tx, mut events_rx) = broadcast::channel(128);
    let hint = url.path().rsplit('.').next().map(|ext| ext.to_lowercase());
    let mut config = AudioConfig::<File>::new(FileConfig::new(url.into()))
        .with_prefer_hardware(true)
        .with_events(events_tx);
    if let Some(ext) = hint {
        config = config.with_hint(ext);
    }
    let audio = Audio::<Stream<File>>::new(config).await?;

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
                    Err(broadcast::error::RecvError::Lagged(n)) => warn!(n, "events lagged"),
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
