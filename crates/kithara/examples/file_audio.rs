//! Play an HTTP audio file (Symphonia decoder).
//!
//! ```
//! cargo run -p kithara --example file_audio --features rodio [URL]
//! ```

use std::error::Error;

use kithara::prelude::*;
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url: Url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/track.mp3".into())
        .parse()?;

    let config = AudioConfig::<File>::new(FileConfig::new(url.into()));
    let audio = Audio::<Stream<File>>::new(config).await?;

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
