//! Test symphonia directly without rodio.

use std::{env::args, error::Error, io::{Read, Seek}};

use kithara_file::{File, FileConfig};
use kithara_stream::Stream;
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

struct StreamWrapper(Stream<File>);

impl Read for StreamWrapper {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for StreamWrapper {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

impl symphonia::core::io::MediaSource for StreamWrapper {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        // We don't know the length upfront
        None
    }
}

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
    let stream = Stream::<File>::new(config).await?;

    info!("Stream created, decoding with symphonia...");

    let handle = tokio::task::spawn_blocking(move || {
        let wrapper = StreamWrapper(stream);
        let mss = MediaSourceStream::new(Box::new(wrapper), Default::default());

        let mut hint = Hint::new();
        hint.with_extension("mp3");

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .expect("probe failed");

        let mut format = probed.format;
        let track = format.default_track().expect("no track").clone();

        info!(track_id = track.id, "Found track");

        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .expect("decoder creation failed");

        let mut sample_count: u64 = 0;
        let mut packet_count: u64 = 0;

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    info!("EOF reached");
                    break;
                }
                Err(e) => {
                    info!(?e, "Error reading packet");
                    break;
                }
            };

            if packet.track_id() != track.id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let duration = decoded.capacity() as u64;
                    sample_count += duration;
                    packet_count += 1;

                    if packet_count % 500 == 0 {
                        info!(packet_count, sample_count, "Progress");
                    }
                }
                Err(e) => {
                    info!(?e, "Decode error");
                }
            }
        }

        info!(packet_count, sample_count, "Done decoding");
        (packet_count, sample_count)
    });

    let (packet_count, sample_count) = handle.await?;
    info!(packet_count, sample_count, "Complete");

    Ok(())
}
