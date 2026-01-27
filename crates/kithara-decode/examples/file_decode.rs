//! Example: Play audio from a progressive HTTP file using Decoder.
//!
//! This demonstrates the decode architecture:
//! - DecoderConfig::<File>::new(file_config) creates config with stream settings
//! - Decoder::new(config) creates stream and decoder
//! - Decoder runs symphonia in separate thread with PCM buffer
//! - Decoder impl rodio::Source for direct playback
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example file_decode --features rodio [URL]
//! ```

use std::{env::args, error::Error};

use kithara_decode::{Decoder, DecoderConfig};
use kithara_file::{File, FileConfig, FileParams};
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_decode=debug".parse()?)
                .add_directive("kithara_file=debug".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    let url = args().nth(1).unwrap_or_else(|| {
        "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
         6 - Movement 2 Un poco andante.MP3"
            .to_string()
    });
    let url: Url = url.parse()?;

    info!("Opening file: {}", url);

    // Detect format hint from URL extension
    let hint = url
        .path()
        .rsplit('.')
        .next()
        .map(|ext| ext.to_lowercase());

    let file_config = FileConfig::new(url).with_params(FileParams::default());

    info!("Creating decoder...");

    // Create decoder via target API
    let mut config = DecoderConfig::<File>::new(file_config);
    if let Some(ext) = hint {
        config = config.with_hint(ext);
    }
    let decoder = Decoder::<Stream<File>>::new(config).await?;

    info!("Starting playback...");

    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(0.3);
        sink.append(decoder);

        info!("Playing...");
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
