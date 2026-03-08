//! Play an HLS stream (Symphonia decoder).
//!
//! ```
//! cargo run -p kithara --example hls_audio --features rodio [URL]
//! ```

use std::{env, error::Error};

use kithara::prelude::*;
use url::Url;

#[kithara_platform::tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url: Url = env::args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".into())
        .parse()?;

    let config = AudioConfig::<Hls>::new(HlsConfig::new(url));
    let audio = Audio::<Stream<Hls>>::new(config).await?;

    kithara_platform::tokio::task::spawn_blocking(move || {
        let stream = rodio::DeviceSinkBuilder::open_default_sink()?;
        let player = rodio::Player::connect_new(stream.mixer());
        player.append(audio);
        player.sleep_until_end();
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })
    .await??;

    Ok(())
}
