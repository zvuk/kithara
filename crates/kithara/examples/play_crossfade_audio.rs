//! Play with `kithara-play`: start with a file URL, then crossfade to HLS after 30 seconds.
//!
//! ```
//! cargo run -p kithara --example play_crossfade_audio [FILE_URL] [HLS_URL]
//! ```

use std::{env::args, error::Error, time::Duration};

use kithara::prelude::*;
use tracing::{info, metadata::LevelFilter, warn};
use tracing_subscriber::EnvFilter;

const SWITCH_AFTER: Duration = Duration::from_secs(30);
const CROSSFADE_SECONDS: f32 = 5.0;
const FILE_URL_DEFAULT: &str = "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
     6 - Movement 2 Un poco andante.MP3";
const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";

#[tokio::main(flavor = "current_thread")]
#[cfg_attr(feature = "perf", hotpath::main)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_play=debug".parse()?)
                .add_directive("kithara_audio=debug".parse()?)
                .add_directive("kithara_net=warn".parse()?)
                .add_directive("symphonia_format_isomp4=warn".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(false)
        .with_file(false)
        .init();

    let mut cli_args = args().skip(1);
    let file_url = cli_args
        .next()
        .unwrap_or_else(|| FILE_URL_DEFAULT.to_string());
    let hls_url = cli_args
        .next()
        .unwrap_or_else(|| HLS_URL_DEFAULT.to_string());
    if cli_args.next().is_some() {
        warn!("Ignoring extra CLI args beyond [FILE_URL] [HLS_URL]");
    }

    let player =
        PlayerImpl::new(PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS));

    info!("Loading first track (file): {file_url}");
    let file = Resource::new(ResourceConfig::new(&file_url)?).await?;
    player.insert(file, None);
    player.play();
    info!(
        "Playing first track. Switching to HLS in {}s",
        SWITCH_AFTER.as_secs()
    );

    tokio::time::sleep(SWITCH_AFTER).await;

    info!("Loading second track (HLS): {hls_url}");
    let hls = Resource::new(ResourceConfig::new(&hls_url)?).await?;
    player.insert(hls, None);
    player.advance_to_next_item();
    player.play();
    info!("Crossfade started. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received, pausing player...");
    player.pause();

    Ok(())
}
