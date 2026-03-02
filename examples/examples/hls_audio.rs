//! Play an HLS stream (Symphonia decoder).
//!
//! ```
//! cargo run -p kithara --example hls_audio --features rodio [URL]
//! ```

use std::error::Error;

use kithara::prelude::*;
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url: Url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".into())
        .parse()?;

    let config = AudioConfig::<Hls>::new(HlsConfig::new(url));
    let audio = Audio::<Stream<Hls>>::new(config).await?;

    tokio::task::spawn_blocking(move || {
        let stream = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream.mixer());
        sink.append(audio);
        sink.sleep_until_end();
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })
    .await??;

    Ok(())
}
