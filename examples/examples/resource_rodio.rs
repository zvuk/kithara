//! Play audio with auto-detection (file or HLS).
//!
//! ```
//! cargo run -p kithara --example resource_rodio --features rodio [URL]
//! ```

use std::{env, error::Error};

use kithara::prelude::*;

#[kithara_platform::tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url = env::args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/track.mp3".into());

    let config = ResourceConfig::new(&url)?;
    let resource = Resource::new(config).await?;

    kithara_platform::tokio::task::spawn_blocking(move || {
        let stream = rodio::DeviceSinkBuilder::open_default_sink()?;
        let player = rodio::Player::connect_new(stream.mixer());
        player.append(resource);
        player.sleep_until_end();
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })
    .await??;

    Ok(())
}
