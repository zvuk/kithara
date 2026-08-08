use bytes::Bytes;
use kithara::{
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
};
use kithara_broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, FeedChunk, LivePcmFeed};
use kithara_integration_tests::signal_pcm::signal::{SignalFn, SineWave};
use url::Url;

pub(super) const CHANNELS: u16 = 2;
pub(super) const SAMPLE_RATE: u32 = 48_000;
pub(super) const TONE_HZ: f64 = 440.0;
/// Half-second segments keep the tests short; the window still spans the three
/// target durations RFC 8216 §6.2.2 asks for.
pub(super) const TARGET: Duration = Duration::from_millis(500);
pub(super) const WINDOW: usize = 6;
pub(super) const GRACE: usize = 3;
/// Frames of one target segment at [`SAMPLE_RATE`].
pub(super) const SEGMENT_FRAMES: u64 = 24_000;

/// Frames the feed hands over in one poll.
const CHUNK_FRAMES: u64 = 2_400;

/// A live origin under test: a sine feed the test releases in steps, the
/// service, and an HTTP client pointed at the bound address.
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

    /// Release exactly `segments` segments of tone — plus a half segment the
    /// packager holds open — and wait for the origin to publish them. Nothing
    /// past that ceiling is ever handed over, so the playlist the test reads
    /// cannot grow another segment behind its back.
    pub(super) fn advance_to(&self, segments: u64) {
        self.released.fetch_max(
            SEGMENT_FRAMES * segments + SEGMENT_FRAMES / 2,
            Ordering::Release,
        );
        while self.handle.status().segments < segments {
            thread::paced_backoff(Duration::from_millis(1));
        }
    }

    /// Report `samples` the producer lost, which the feed hands to the worker
    /// with its next poll.
    pub(super) fn drop_samples(&self, samples: u64) {
        self.dropped.fetch_add(samples, Ordering::Release);
    }

    /// Fetch `path` relative to the master playlist, reporting the HTTP status
    /// when the origin refuses.
    pub(super) async fn get(&self, path: &str) -> Result<Bytes, u16> {
        let url = self.base.join(path).expect("a servable path");
        match self.client.get_bytes(url, None).await {
            Ok(bytes) => Ok(bytes),
            Err(NetError::Status { status, .. }) => Err(status.get()),
            Err(error) => panic!("the origin is unreachable: {error}"),
        }
    }

    /// The media playlist as text.
    pub(super) async fn media_playlist(&self) -> String {
        let bytes = self.get("v/0/live.m3u8").await.expect("a live playlist");
        String::from_utf8(bytes.to_vec()).expect("the playlist is text")
    }

    /// Stop serving and let both threads go.
    pub(super) fn shutdown(&self) {
        self.scope.cancel();
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Sine the test releases in steps: a poll hands over what has been released
/// and nothing more, and the feed never ends on its own.
struct PacedSine {
    released: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    produced: u64,
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
            has_ended: false,
        }
    }
}
