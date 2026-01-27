//! Memory test: Read file without rodio to measure base memory usage.

use std::{env::args, error::Error, io::Read};

use kithara_file::{File, FileConfig};
use kithara_stream::Stream;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_file=info".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .init();

    let url = args().nth(1).unwrap_or_else(|| {
        "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
         6 - Movement 2 Un poco andante.MP3"
            .to_string()
    });
    let url: Url = url.parse()?;

    info!("Opening file: {}", url);

    let config = FileConfig::new(url);
    let mut stream = Stream::<File>::new(config).await?;

    info!("Stream created, reading in blocking thread...");

    let handle = tokio::task::spawn_blocking(move || {
        // Read in chunks and count
        let mut buf = [0u8; 32768]; // 32KB buffer
        let mut total_bytes = 0u64;
        let mut read_count = 0u64;

        loop {
            let n = stream.read(&mut buf).expect("read failed");
            if n == 0 {
                break;
            }
            total_bytes += n as u64;
            read_count += 1;

            if read_count % 100 == 0 {
                info!(total_bytes, read_count, "Progress");
            }
        }

        info!(total_bytes, read_count, "Done reading");
        (total_bytes, read_count)
    });

    let (total_bytes, read_count) = handle.await?;
    info!(total_bytes, read_count, "Complete");

    Ok(())
}
