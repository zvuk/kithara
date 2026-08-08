use arc_swap::ArcSwap;
use kithara_encode::StreamEncoder;
use kithara_platform::{
    CancelScope, CancelToken,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    BroadcastResult,
    config::BroadcastConfig,
    feed::LivePcmFeed,
    segment::{Segment, Segmenter},
    server::{self, Origin},
    window::LiveWindow,
};

/// What a broadcast looks like from the outside while it runs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BroadcastStatus {
    pub url: Arc<str>,
    /// The stream is still taking audio; `false` once the tail is a VOD
    /// playlist.
    pub is_live: bool,
    pub segments: u64,
    pub dropped_samples: u64,
}

/// Entry point of the live HLS origin.
pub struct Broadcast;

impl Broadcast {
    /// Take `feed` on air: one worker packages it while an HTTP origin serves
    /// the sliding window.
    ///
    /// The returned handle carries the URL of the bound origin. `parent` owns
    /// the broadcast's lifetime — cancelling it stops both threads.
    ///
    /// # Errors
    ///
    /// Returns [`crate::BroadcastError::InvalidConfig`] or
    /// [`crate::BroadcastError::PlaylistTooShort`] for an unservable
    /// configuration, [`crate::BroadcastError::Encode`] when the AAC-LC
    /// encoder cannot open, and [`crate::BroadcastError::Bind`] when the
    /// origin cannot bind its address.
    pub fn start<F>(
        config: &BroadcastConfig,
        feed: F,
        parent: Option<CancelToken>,
    ) -> BroadcastResult<BroadcastHandle>
    where
        F: LivePcmFeed + 'static,
    {
        config.validate()?;

        let encoder = StreamEncoder::new(
            config.sample_rate,
            config.channels,
            config.bit_rate,
            config.sample_rate,
        )?;
        let segmenter = Segmenter::new(config)?;
        let window = LiveWindow::new(config)?;

        let origin = Arc::new(Origin {
            snapshot: ArcSwap::from_pointee(window.snapshot()),
            master: Arc::from(server::master_playlist(config.bit_rate)),
        });
        let counters = Arc::new(Counters::default());
        let scope = CancelScope::new(parent);
        let addr = server::start(config.bind, Arc::clone(&origin), scope.token())?;
        let stop = Arc::new(AtomicBool::new(false));

        let worker = Worker {
            feed,
            encoder: Some(encoder),
            segmenter,
            window,
            origin: Arc::clone(&origin),
            counters: Arc::clone(&counters),
            token: scope.token(),
            stop: Arc::clone(&stop),
            samples: Vec::new(),
        };
        let join = thread::spawn_named("kithara-broadcast-worker", move || worker.run());

        Ok(BroadcastHandle {
            url: Arc::from(format!("http://{addr}/master.m3u8")),
            origin,
            counters,
            stop,
            worker: Mutex::new(Some(join)),
            scope,
        })
    }
}

/// Owner of one live broadcast: the URL it serves, what it has packaged, and
/// the two threads behind it.
pub struct BroadcastHandle {
    url: Arc<str>,
    origin: Arc<Origin>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    scope: CancelScope,
}

impl BroadcastHandle {
    /// Master playlist URL a player joins the stream at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Cancel token the broadcast's threads run under.
    #[must_use]
    pub fn token(&self) -> CancelToken {
        self.scope.token()
    }

    /// What the origin is serving right now.
    #[must_use]
    pub fn status(&self) -> BroadcastStatus {
        BroadcastStatus {
            url: Arc::clone(&self.url),
            is_live: !self.origin.snapshot.load().is_finished,
            segments: self.counters.segments.load(Ordering::Relaxed),
            dropped_samples: self.counters.dropped.load(Ordering::Relaxed),
        }
    }

    /// End the broadcast: the worker swallows what the feed still holds,
    /// finishes the encoder, and publishes the playlist with `EXT-X-ENDLIST`.
    /// The origin keeps serving that tail. Repeat calls do nothing.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);

        let join = self.worker.lock().take();
        if let Some(join) = join
            && join.join().is_err()
        {
            tracing::error!("the broadcast worker panicked");
        }
    }
}

/// The packaging loop: feed → encoder → segmenter → window → published
/// snapshot.
struct Worker<F> {
    feed: F,
    encoder: Option<StreamEncoder>,
    segmenter: Segmenter,
    window: LiveWindow,
    origin: Arc<Origin>,
    counters: Arc<Counters>,
    token: CancelToken,
    stop: Arc<AtomicBool>,
    samples: Vec<f32>,
}

/// What the broadcast has packaged, for the handle to report. The origin does
/// not read them.
#[derive(Debug, Default)]
struct Counters {
    segments: AtomicU64,
    dropped: AtomicU64,
}

impl<F: LivePcmFeed> Worker<F> {
    /// Backoff between polls of an empty feed, far below one segment.
    const POLL: Duration = Duration::from_millis(2);

    fn run(mut self) {
        while !self.token.is_cancelled() {
            let ended = match self.pump() {
                Ok(ended) => ended,
                Err(error) => {
                    tracing::error!(%error, "the live packager stopped");
                    break;
                }
            };
            if ended || self.stop.load(Ordering::Acquire) {
                break;
            }
            if self.samples.is_empty() {
                thread::paced_backoff(Self::POLL);
            }
        }

        if !self.token.is_cancelled() {
            self.end();
        }
    }

    /// One poll of the feed, packaged. Returns whether the producer is gone.
    fn pump(&mut self) -> BroadcastResult<bool> {
        self.samples.clear();
        let chunk = self.feed.poll(&mut self.samples);

        if chunk.dropped > 0 {
            self.counters
                .dropped
                .fetch_add(chunk.dropped, Ordering::Relaxed);
            if let Some(segment) = self.segmenter.mark_drop() {
                self.publish(segment);
            }
        }
        if let Some(encoder) = self.encoder.as_mut()
            && !self.samples.is_empty()
        {
            let encoded = encoder.push(&self.samples)?;
            for unit in &encoded {
                if let Some(segment) = self.segmenter.push(unit)? {
                    self.publish(segment);
                }
            }
        }
        Ok(chunk.has_ended)
    }

    /// Swallow what the feed still holds, drain the encoder, and publish the
    /// VOD tail.
    fn end(mut self) {
        loop {
            if self.token.is_cancelled() {
                return;
            }
            match self.pump() {
                Ok(ended) => {
                    if ended || self.samples.is_empty() {
                        break;
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "the live packager stopped draining");
                    break;
                }
            }
        }

        if let Some(encoder) = self.encoder.take() {
            match encoder.finish() {
                Ok(units) => {
                    for unit in &units {
                        match self.segmenter.push(unit) {
                            Ok(Some(segment)) => self.publish(segment),
                            Ok(None) => {}
                            Err(error) => {
                                tracing::error!(%error, "the live packager dropped a tail unit");
                            }
                        }
                    }
                }
                Err(error) => tracing::error!(%error, "the live encoder failed to drain"),
            }
        }
        if let Some(segment) = self.segmenter.flush() {
            self.publish(segment);
        }

        self.window.finish();
        self.origin.snapshot.store(Arc::new(self.window.snapshot()));
    }

    fn publish(&mut self, segment: Segment) {
        self.window.push(segment);
        self.counters.segments.fetch_add(1, Ordering::Relaxed);
        self.origin.snapshot.store(Arc::new(self.window.snapshot()));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use kithara_platform::{sync::Arc, thread, time::Duration};

    use super::{Broadcast, BroadcastHandle};
    use crate::{
        config::BroadcastConfig,
        feed::{FeedChunk, LivePcmFeed},
    };

    struct Consts;

    impl Consts {
        const AMPLITUDE: f32 = 0.25;
        const CHUNK_FRAMES: usize = 4_800;
        const SAMPLE_RATE: u32 = 48_000;
        const TARGET: Duration = Duration::from_millis(500);
    }

    /// Feed of prepared chunks: each poll hands over one, and the feed reports
    /// end-of-stream only when it was built to.
    struct VecFeed {
        chunks: VecDeque<(u64, Vec<f32>)>,
        ends: bool,
    }

    impl VecFeed {
        fn new(chunks: impl IntoIterator<Item = (u64, Vec<f32>)>, ends: bool) -> Self {
            Self {
                chunks: chunks.into_iter().collect(),
                ends,
            }
        }
    }

    impl LivePcmFeed for VecFeed {
        fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk {
            match self.chunks.pop_front() {
                Some((dropped, samples)) => {
                    out.extend_from_slice(&samples);
                    FeedChunk {
                        dropped,
                        has_ended: false,
                    }
                }
                None => FeedChunk {
                    dropped: 0,
                    has_ended: self.ends,
                },
            }
        }
    }

    /// Alternating full-scale steps: enough for the encoder to produce access
    /// units, and the playlist is what these tests read.
    fn pcm(frames: usize) -> Vec<f32> {
        (0..frames * usize::from(BroadcastConfig::CHANNELS))
            .map(|index| {
                if index.is_multiple_of(2) {
                    Consts::AMPLITUDE
                } else {
                    -Consts::AMPLITUDE
                }
            })
            .collect()
    }

    fn config() -> BroadcastConfig {
        BroadcastConfig::builder()
            .segment_target(Consts::TARGET)
            .build()
    }

    fn chunks(count: usize) -> Vec<(u64, Vec<f32>)> {
        (0..count).map(|_| (0, pcm(Consts::CHUNK_FRAMES))).collect()
    }

    fn wait_for(handle: &BroadcastHandle, segments: u64) {
        while handle.status().segments < segments {
            thread::paced_backoff(Duration::from_millis(1));
        }
    }

    fn playlist(handle: &BroadcastHandle) -> Arc<str> {
        Arc::clone(&handle.origin.snapshot.load().playlist)
    }

    #[test]
    fn a_gap_in_the_feed_marks_the_next_segment_discontinuous() {
        let mut feed = chunks(6);
        feed.push((Consts::SAMPLE_RATE.into(), pcm(Consts::CHUNK_FRAMES)));
        feed.extend(chunks(6));
        let handle = Broadcast::start(&config(), VecFeed::new(feed, true), None).expect("on air");

        handle.stop();

        assert!(
            playlist(&handle).contains("#EXT-X-DISCONTINUITY\n"),
            "a reported gap closes the segment and marks the next: {}",
            playlist(&handle)
        );
        assert_eq!(
            handle.status().dropped_samples,
            u64::from(Consts::SAMPLE_RATE)
        );
        handle.token().cancel();
    }

    #[test]
    fn the_end_of_the_feed_finishes_the_stream() {
        let handle =
            Broadcast::start(&config(), VecFeed::new(chunks(8), true), None).expect("on air");

        wait_for(&handle, 1);
        while handle.status().is_live {
            thread::paced_backoff(Duration::from_millis(1));
        }

        assert!(playlist(&handle).contains("#EXT-X-ENDLIST\n"));
        assert!(
            !handle.status().is_live,
            "a producer that left takes the stream off air"
        );
        handle.token().cancel();
    }

    #[test]
    fn stopping_twice_is_the_same_as_stopping_once() {
        let handle =
            Broadcast::start(&config(), VecFeed::new(chunks(8), false), None).expect("on air");

        wait_for(&handle, 1);
        handle.stop();
        let after_first = handle.status();
        handle.stop();

        assert!(playlist(&handle).contains("#EXT-X-ENDLIST\n"));
        assert!(!after_first.is_live);
        assert_eq!(handle.status().segments, after_first.segments);
        handle.token().cancel();
    }
}
