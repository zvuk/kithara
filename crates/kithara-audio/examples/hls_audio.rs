#![forbid(unsafe_code)]

//! Example: Play audio from an HLS stream using Audio pipeline.
//!
//! This demonstrates the audio architecture:
//! - AudioConfig::<Hls>::new(hls_config) creates config with stream settings
//! - Audio::new(config) creates stream and audio pipeline
//! - Audio runs symphonia in a separate thread with PCM buffer
//! - Audio impl rodio::Source for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-audio --example hls_audio --features rodio [URL]
//! ```

use std::{
    env::args,
    error::Error,
    io::{Error as IoError, ErrorKind},
    sync::mpsc,
    thread,
    time::Duration,
};

use kithara_audio::{Audio, AudioConfig};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::Stream;
use tokio::sync::{broadcast, oneshot};
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
                .add_directive("kithara_decode=debug".parse()?)
                .add_directive("kithara_hls=debug".parse()?)
                .add_directive("kithara_stream=debug".parse()?)
                .add_directive("kithara_net=warn".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    let url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string());
    let url: Url = url.parse()?;

    info!("Opening HLS stream: {}", url);

    // Create audio pipeline
    let (events_tx, mut events_rx) = broadcast::channel(128);
    let hls_config = HlsConfig::new(url).with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        // Aggressive ABR for testing (faster switching)
        min_buffer_for_up_switch_secs: 2.0, // default: 10.0
        min_switch_interval: Duration::from_secs(5), // default: 30
        up_hysteresis_ratio: 1.1,           // default: 1.3
        throughput_safety_factor: 1.2,      // default: 1.5
        ..Default::default()
    });
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_prefer_hardware(true)
        .with_events(events_tx);
    let audio = Audio::<Stream<Hls>>::new(config).await?;

    info!("Starting playback... (Press Ctrl+C to stop)");

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (done_tx, mut done_rx) = oneshot::channel::<Result<(), Box<dyn Error + Send + Sync>>>();

    let playback_handle = thread::Builder::new()
        .name("kithara-hls-playback".to_string())
        .spawn(move || {
            let result: Result<(), Box<dyn Error + Send + Sync>> = (|| {
                let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
                let sink = rodio::Sink::connect_new(stream_handle.mixer());
                sink.set_volume(1.0);
                sink.append(audio);

                info!("Playing...");

                loop {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    if sink.empty() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }

                sink.stop();
                info!("Playback stopped");
                Ok(())
            })();

            let _ = done_tx.send(result);
        })
        .map_err(|e| Box::<dyn Error + Send + Sync>::from(e))?;

    // Inline event loop on current-thread runtime
    let mut playback_done = false;
    while !playback_done {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received Ctrl+C, stopping playback...");
                let _ = stop_tx.send(());
            }
            recv = events_rx.recv() => {
                match recv {
                    Ok(ev) => info!(?ev),
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "Event receiver lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Nothing more to read.
                    }
                }
            }
            done = &mut done_rx => {
                playback_done = true;
                done??; // propagate playback result
            }
        }
    }

    playback_handle.join().map_err(|_| {
        Box::<dyn Error + Send + Sync>::from(IoError::new(
            ErrorKind::Other,
            "playback thread panicked",
        ))
    })?;

    Ok(())
}
