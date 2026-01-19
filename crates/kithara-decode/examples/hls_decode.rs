//! Example: Play audio from an HLS stream using kithara-decode.
//!
//! This demonstrates the integration between kithara-hls and kithara-decode,
//! including resampling and speed control (plays at 0.5x speed by default).
//!
//! Run with:
//! ```
//! cargo run -p kithara-decode --example hls_decode --features rodio [URL] [SPEED]
//! ```
//!
//! Speed is optional (default 0.5 = 2x slower). Examples:
//! - 0.5 for half speed (2x slower)
//! - 1.0 for normal speed
//! - 2.0 for double speed

use std::{env::args, error::Error, sync::Arc};

use kithara_decode::{AudioSyncReader, Pipeline};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsEvent, HlsParams};
use kithara_stream::StreamSource;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

const TARGET_SAMPLE_RATE: u32 = 44100;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
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

    // Default speed 0.5 = 2x slower
    let speed: f32 = args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);

    info!("Opening HLS stream: {}", url);
    info!("Playback speed: {}x ({}x slower)", speed, 1.0 / speed);

    let hls_params = HlsParams::default().with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..Default::default()
    });

    // Open HLS source
    let source = StreamSource::<Hls>::open(url, hls_params).await?;
    let source_arc = Arc::new(source);

    // Subscribe to events
    let mut events_rx = source_arc.events();
    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            match ev {
                HlsEvent::VariantApplied {
                    from_variant,
                    to_variant,
                    reason,
                } => {
                    info!(
                        ?reason,
                        "Variant switch: {} -> {}", from_variant, to_variant
                    );
                }
                HlsEvent::SegmentComplete {
                    segment_index,
                    variant,
                    bytes_transferred,
                    ..
                } => {
                    info!(
                        segment_index,
                        variant, bytes_transferred, "Segment complete"
                    );
                }
                HlsEvent::EndOfStream => {
                    info!("End of stream");
                    break;
                }
                _ => {}
            }
        }
    });

    info!("Creating audio pipeline...");

    // Create unified pipeline (decoder + resampler in one)
    let pipeline = Pipeline::open(source_arc, TARGET_SAMPLE_RATE).await?;

    let output_spec = pipeline.output_spec();
    info!(
        sample_rate = output_spec.sample_rate,
        channels = output_spec.channels,
        "Pipeline created"
    );

    // Set playback speed
    pipeline.set_speed(speed)?;

    info!(
        target_rate = TARGET_SAMPLE_RATE,
        speed,
        "Speed configured ({}x slower playback)",
        1.0 / speed
    );

    // Create rodio adapter from buffer
    let audio_source = AudioSyncReader::new(
        pipeline.consumer().clone(),
        pipeline.buffer().clone(),
        output_spec,
    );

    // Play via rodio in blocking thread
    let handle = tokio::task::spawn_blocking(move || {
        let stream_handle = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream_handle.mixer());
        sink.set_volume(0.3);
        sink.append(audio_source);

        info!("Playing at {}x speed ({}x slower)...", speed, 1.0 / speed);
        sink.sleep_until_end();

        info!("Playback complete");
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    handle.await??;

    Ok(())
}
