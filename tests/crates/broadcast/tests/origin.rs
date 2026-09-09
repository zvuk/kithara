use std::{io::Cursor, num::NonZeroUsize};

use bytes::Bytes;
use kithara::{
    broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, BroadcastOutput},
    decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    net::{HttpClient, NetError, NetOptions},
    output::LiveOutput,
    platform::{CancelScope, sync::Mutex, time::Duration},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
    worker::{Worker, WorkerConfig},
};
use kithara_integration_tests::{
    bufpool_ext::{TestPools, pools},
    waits::wait_until,
};
use kithara_test_fixtures::signal::goertzel_magnitude;
use url::Url;

pub(super) const CHANNELS: u16 = 2;
pub(super) const SAMPLE_RATE: u32 = 48_000;
pub(super) const TONE_HZ: f64 = 440.0;
pub(super) const TARGET: Duration = Duration::from_millis(500);
pub(super) const WINDOW: usize = 6;
pub(super) const GRACE: usize = 3;
pub(super) const SEGMENT_FRAMES: u64 = 24_000;

const CHUNK_FRAMES: usize = 2_400;
const BUFFER_FRAMES: NonZeroUsize = match NonZeroUsize::new(512_000) {
    Some(frames) => frames,
    None => unreachable!(),
};

/// Non-progress watchdog: the wait resolves as soon as the segment is reported.
const PACKAGER_DEADLINE: Duration = Duration::from_secs(20);

const TONE_MARGIN: f64 = 50.0;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlaylistEntry {
    pub(super) extinf: String,
    pub(super) uri: String,
    pub(super) discontinuity: bool,
    pub(super) seconds: f64,
}

#[derive(Debug, Clone)]
pub(super) struct Playlist {
    pub(super) target_text: String,
    pub(super) text: String,
    pub(super) entries: Vec<PlaylistEntry>,
    pub(super) target: f64,
    pub(super) discontinuity_sequence: u64,
    pub(super) media_sequence: u64,
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
                    seconds,
                    discontinuity,
                    uri: line.to_owned(),
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

    pub(super) fn spans(&self) -> f64 {
        self.entries.iter().map(|entry| entry.seconds).sum()
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
        DecoderConfig::<kithara::resampler::NoResamplerBackend, TestPools>::builder()
            .pools(pools())
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
    output: Mutex<SignalOutput>,
    _worker: Worker,
    scope: CancelScope,
    client: HttpClient,
    base: Url,
}

impl Origin {
    pub(super) async fn advance_to(&self, segments: u64) {
        self.output
            .lock()
            .write_until(SEGMENT_FRAMES * segments + SEGMENT_FRAMES / 2);
        wait_until(
            PACKAGER_DEADLINE,
            "the packager reaches the segment",
            || self.handle.status().segments >= segments,
        )
        .await
        .expect("the packager keeps up with the released frames");
    }

    pub(super) fn drop_samples(&self, samples: u64) {
        self.output.lock().drop_samples(samples);
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

    pub(super) fn start(samples: Vec<f32>) -> Self {
        let scope = CancelScope::new(None);
        let worker = Worker::new(WorkerConfig::new());
        let pools = pools();
        let config = BroadcastConfig::builder(worker.clone(), pools.clone())
            .cancel(scope.token())
            .sample_rate(SAMPLE_RATE)
            .channels(CHANNELS)
            .segment_target(TARGET)
            .window(WINDOW)
            .grace(GRACE)
            .buffer_frames(BUFFER_FRAMES)
            .build();
        let (output, handle) = Broadcast::start(config).expect("go on air");
        let base = Url::parse(handle.url()).expect("the handle reports a URL");
        let client = HttpClient::new(NetOptions::default(), pools, scope.token());

        Self {
            handle,
            output: Mutex::new(SignalOutput {
                output,
                produced: 0,
                samples,
            }),
            _worker: worker,
            scope,
            client,
            base,
        }
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SignalOutput {
    output: BroadcastOutput,
    produced: u64,
    samples: Vec<f32>,
}

impl SignalOutput {
    fn drop_samples(&mut self, samples: u64) {
        let frames = samples / u64::from(CHANNELS);
        let frames = usize::try_from(frames).expect("test gap fits one address space");
        self.output.write_stereo(frames, &[], &[]);
    }

    fn write_until(&mut self, target: u64) {
        while self.produced < target {
            let remaining = target - self.produced;
            let chunk_frames = u64::try_from(CHUNK_FRAMES).expect("test chunk fits u64");
            let frames = usize::try_from(remaining.min(chunk_frames))
                .expect("test chunk fits one address space");
            let frames_u64 = u64::try_from(frames).expect("test chunk fits u64");
            let start = usize::try_from(self.produced).expect("test position fits usize");
            let left = &self.samples[start..start + frames];
            self.output.write_stereo(frames, left, left);
            self.produced += frames_u64;
        }
    }
}
