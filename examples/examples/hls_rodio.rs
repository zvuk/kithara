//! Play an HLS stream (rodio decoder).
//!
//! ```
//! cargo run -p kithara --example hls_rodio --features rodio [URL]
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

    let stream = Stream::<Hls>::new(HlsConfig::new(url)).await?;
    let decoder = rodio::Decoder::new(stream)?;

    kithara_platform::tokio::task::spawn_blocking(move || {
        let stream = rodio::DeviceSinkBuilder::open_default_sink()?;
        let player = rodio::Player::connect_new(stream.mixer());
        player.append(decoder);
        player.sleep_until_end();
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })
    .await??;

    Ok(())
}
