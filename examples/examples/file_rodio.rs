//! Play an HTTP audio file (rodio decoder).
//!
//! ```
//! cargo run -p kithara --example file_rodio --features rodio [URL]
//! ```

use std::{env, error::Error};

use kithara::prelude::*;
use url::Url;

#[kithara_platform::tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url: Url = env::args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/track.mp3".into())
        .parse()?;

    let stream = Stream::<File>::new(FileConfig::new(url.into())).await?;
    let decoder = rodio::Decoder::new(stream)?;

    kithara_platform::tokio::task::spawn_blocking(move || {
        let stream = rodio::OutputStreamBuilder::open_default_stream()?;
        let sink = rodio::Sink::connect_new(stream.mixer());
        sink.append(decoder);
        sink.sleep_until_end();
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })
    .await??;

    Ok(())
}
