use std::io::Cursor;

use bytes::Bytes;
use kithara::{
    decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    net::{HttpClient, NetError, NetOptions},
    platform::{
        CancelScope,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    },
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, FeedChunk, LivePcmFeed};
use kithara_integration_tests::{
    goertzel::goertzel_magnitude,
    signal_pcm::signal::{SignalFn, SineWave},
};
use url::Url;

pub(super) const CHANNELS: u16 = 2;
pub(super) const SAMPLE_RATE: u32 = 48_000;
pub(super) const TONE_HZ: f64 = 440.0;
pub(super) const TARGET: Duration = Duration::from_millis(500);
pub(super) const WINDOW: usize = 6;
pub(super) const GRACE: usize = 3;
pub(super) const SEGMENT_FRAMES: u64 = 24_000;

const CHUNK_FRAMES: u64 = 2_400;

const TONE_MARGIN: f64 = 50.0;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlaylistEntry {
    pub(super) extinf: String,
    pub(super) uri: String,
    pub(super) seconds: f64,
    pub(super) discontinuity: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Playlist {
    pub(super) text: String,
    pub(super) target: f64,
    pub(super) target_text: String,
    pub(super) media_sequence: u64,
    pub(super) discontinuity_sequence: u64,
    pub(super) entries: Vec<PlaylistEntry>,
}

impl Playlist {
    pub(super) fn parse(text: String) -> Self {
        let mut entries = Vec::new();
        let mut extinf: Option<(String, f64)> = None;
        let mut discontinuity = false;
        for line in text.lines() {
            if line == "#EXT-X-DISCONTINUITY" {
                discontinuity = true;
            } else if let Some(value) = line.strip_prefix("#EXTINF:") {
                let seconds = value
                    .trim_end_matches(',')
                    .parse()
                    .expect("EXTINF carries a duration");
                extinf = Some((line.to_owned(), seconds));
            } else if !line.starts_with('#') && !line.is_empty() {
                let (extinf, seconds) = extinf.take().expect("a segment URI follows its EXTINF");
                entries.push(PlaylistEntry {
                    extinf,
                    uri: line.to_owned(),
                    seconds,
                    discontinuity,
                });
                discontinuity = false;
            }
        }

        let target_text = tag(&text, "#EXT-X-TARGETDURATION:").to_owned();
        Self {
            target: target_text.parse().expect("a numeric target duration"),
            target_text,
            media_sequence: tag(&text, "#EXT-X-MEDIA-SEQUENCE:")
                .parse()
                .expect("a numeric media sequence"),
            discontinuity_sequence: tag(&text, "#EXT-X-DISCONTINUITY-SEQUENCE:")
                .parse()
                .expect("a numeric discontinuity sequence"),
            entries,
            text,
        }
    }

    pub(super) fn spans(&self) -> f64 {
        self.entries.iter().map(|entry| entry.seconds).sum()
    }

    pub(super) fn sequences(&self) -> Vec<u64> {
        self.entries
            .iter()
            .map(|entry| {
                entry
                    .uri
                    .strip_prefix("seg/")
                    .and_then(|uri| uri.strip_suffix(".aac"))
                    .expect("a segment URI")
                    .parse()
                    .expect("a segment sequence number")
            })
            .collect()
    }

    pub(super) fn uris_after_last_discontinuity(&self) -> Option<Vec<&str>> {
        let start = self.entries.iter().rposition(|entry| entry.discontinuity)?;
        Some(
            self.entries[start..]
                .iter()
                .map(|entry| entry.uri.as_str())
                .collect(),
        )
    }
}

fn tag<'a>(text: &'a str, tag: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(tag))
        .unwrap_or_else(|| panic!("{tag} is missing from {text}"))
}

pub(super) fn decode_adts_left(bytes: Vec<u8>) -> Vec<f32> {
    let mut decoder = DecoderFactory::create_from_media_info(
        Cursor::new(bytes),
        &MediaInfo::builder()
            .codec(AudioCodec::AacLc)
            .container(ContainerFormat::Adts)
            .build(),
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .build(),
    )
    .expect("create the ADTS AAC-LC decoder");

    let mut left = Vec::new();
    while let DecoderChunkOutcome::Chunk(chunk) = decoder.next_chunk().expect("decode chunk") {
        let channels = usize::from(chunk.spec().channels);
        left.extend(chunk.samples.iter().step_by(channels));
    }
    left
}

pub(super) fn assert_carries_the_tone(pcm: &[f32], tone_hz: f64, sample_rate: u32, label: &str) {
    let tone = goertzel_magnitude(pcm, tone_hz, sample_rate);
    let off_tone = goertzel_magnitude(pcm, tone_hz * 3.0, sample_rate);

    assert!(
        tone > off_tone * TONE_MARGIN,
        "{label}: expected a {tone_hz} Hz tone over {} frames: |tone| = {tone:.1}, \
         |off tone| = {off_tone:.1}",
        pcm.len()
    );
}

pub(super) struct Origin {
    pub(super) handle: BroadcastHandle,
    released: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    scope: CancelScope,
    client: HttpClient,
    base: Url,
}

impl Origin {
    pub(super) fn start() -> Self {
        let released = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let scope = CancelScope::new(None);
        let config = BroadcastConfig::builder()
            .sample_rate(SAMPLE_RATE)
            .channels(CHANNELS)
            .segment_target(TARGET)
            .window(WINDOW)
            .grace(GRACE)
            .build();
        let feed = PacedSine {
            released: Arc::clone(&released),
            dropped: Arc::clone(&dropped),
            produced: 0,
            closed: false,
        };
        let handle = Broadcast::start(&config, feed, Some(scope.token())).expect("go on air");
        let base = Url::parse(handle.url()).expect("the handle reports a URL");
        let client = HttpClient::new(NetOptions::default(), scope.token());

        Self {
            handle,
            released,
            dropped,
            scope,
            client,
            base,
        }
    }

    pub(super) fn advance_to(&self, segments: u64) {
        self.released.fetch_max(
            SEGMENT_FRAMES * segments + SEGMENT_FRAMES / 2,
            Ordering::Release,
        );
        while self.handle.status().segments < segments {
            thread::paced_backoff(Duration::from_millis(1));
        }
    }

    pub(super) fn drop_samples(&self, samples: u64) {
        self.dropped.fetch_add(samples, Ordering::Release);
    }

    pub(super) async fn get(&self, path: &str) -> Result<Bytes, u16> {
        let url = self.base.join(path).expect("a servable path");
        match self.client.get_bytes(url, None).await {
            Ok(bytes) => Ok(bytes),
            Err(NetError::Status { status, .. }) => Err(status.get()),
            Err(error) => panic!("the origin is unreachable: {error}"),
        }
    }

    pub(super) async fn media_playlist(&self) -> String {
        let bytes = self.get("v/0/live.m3u8").await.expect("a live playlist");
        String::from_utf8(bytes.to_vec()).expect("the playlist is text")
    }

    pub(super) fn shutdown(&self) {
        self.scope.cancel();
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct PacedSine {
    released: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    produced: u64,
    closed: bool,
}

impl LivePcmFeed for PacedSine {
    fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk {
        let dropped = self.dropped.swap(0, Ordering::AcqRel);
        let pending = self
            .released
            .load(Ordering::Acquire)
            .saturating_sub(self.produced);
        let frames = pending.min(CHUNK_FRAMES);

        let tone = SineWave(TONE_HZ);
        for frame in self.produced..self.produced + frames {
            let frame = usize::try_from(frame).expect("the feed stays inside one address space");
            let sample = f32::from(tone.sample(frame, SAMPLE_RATE)) / 32_768.0;
            for _ in 0..CHANNELS {
                out.push(sample);
            }
        }
        self.produced += frames;

        FeedChunk {
            dropped,
            has_ended: self.closed && pending == frames,
        }
    }

    fn close(&mut self) {
        self.closed = true;
    }
}
