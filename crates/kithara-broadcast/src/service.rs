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
    /// encoder cannot open, [`crate::BroadcastError::Bind`] when the origin
    /// cannot bind its address, and [`crate::BroadcastError::Serve`] when it
    /// cannot start the thread behind that address.
    pub fn start<F>(
        config: &BroadcastConfig,
        feed: F,
        parent: Option<CancelToken>,
    ) -> BroadcastResult<BroadcastHandle>
    where
        F: LivePcmFeed + 'static,
    {
        let scope = CancelScope::new(parent);
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker::new(config, feed, scope.token(), Arc::clone(&stop))?;

        let origin = Arc::clone(&worker.origin);
        let counters = Arc::clone(&worker.counters);
        let addr = server::start(config.bind, Arc::clone(&origin), scope.token())?;
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

    /// Build the packaging loop and the origin state it publishes into.
    fn new(
        config: &BroadcastConfig,
        feed: F,
        token: CancelToken,
        stop: Arc<AtomicBool>,
    ) -> BroadcastResult<Self> {
        config.validate()?;

        let window = LiveWindow::new(config)?;
        Ok(Self {
            feed,
            encoder: Some(StreamEncoder::new(
                config.sample_rate,
                config.channels,
                config.bit_rate,
                config.sample_rate,
            )?),
            segmenter: Segmenter::new(config)?,
            origin: Arc::new(Origin {
                snapshot: ArcSwap::from_pointee(window.snapshot()),
                master: Arc::from(server::master_playlist(config.bit_rate)),
            }),
            window,
            counters: Arc::new(Counters::default()),
            token,
            stop,
            samples: Vec::new(),
        })
    }

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
        if chunk.dropped > 0 {
            self.counters
                .dropped
                .fetch_add(chunk.dropped, Ordering::Relaxed);
            if let Some(segment) = self.segmenter.mark_drop() {
                self.publish(segment);
            }
        }
        Ok(chunk.has_ended)
    }

    /// Swallow what the feed still holds, drain the encoder, and publish the
    /// VOD tail.
    fn end(mut self) {
        self.feed.close();

        loop {
            if self.token.is_cancelled() {
                return;
            }
            match self.pump() {
                Ok(true) => break,
                Ok(false) => {
                    if self.samples.is_empty() {
                        thread::paced_backoff(Self::POLL);
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

    use kithara_platform::{
        CancelScope,
        sync::{Arc, atomic::AtomicBool},
        thread,
        time::Duration,
    };

    use super::{Broadcast, BroadcastHandle, Worker};
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

        fn close(&mut self) {
            self.ends = true;
        }
    }

    /// Feed with audio ready every other poll, the way a producer filling a
    /// ring in bursts leaves it.
    struct BurstFeed {
        chunks: VecDeque<Vec<f32>>,
        ready: bool,
        closed: bool,
    }

    impl BurstFeed {
        fn new(chunks: impl IntoIterator<Item = Vec<f32>>) -> Self {
            Self {
                chunks: chunks.into_iter().collect(),
                ready: false,
                closed: false,
            }
        }
    }

    impl LivePcmFeed for BurstFeed {
        fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk {
            self.ready = !self.ready;
            if self.ready
                && let Some(chunk) = self.chunks.pop_front()
            {
                out.extend_from_slice(&chunk);
            }
            FeedChunk {
                dropped: 0,
                has_ended: self.closed && self.chunks.is_empty(),
            }
        }

        fn close(&mut self) {
            self.closed = true;
        }
    }

    /// Drive one worker to its end on this thread and read back what the origin
    /// would serve. `stop` is the flag the handle sets.
    fn run_worker<F: LivePcmFeed>(feed: F, config: &BroadcastConfig, stop: bool) -> Arc<str> {
        let scope = CancelScope::new(None);
        let worker = Worker::new(config, feed, scope.token(), Arc::new(AtomicBool::new(stop)))
            .expect("worker");
        let origin = Arc::clone(&worker.origin);

        worker.run();
        Arc::clone(&origin.snapshot.load().playlist)
    }

    /// Each listed segment as the media seconds it announces and whether the
    /// playlist breaks the timeline in front of it.
    fn listed_segments(playlist: &str) -> Vec<(bool, f64)> {
        let mut listed = Vec::new();
        let mut broke = false;
        for line in playlist.lines() {
            if line == "#EXT-X-DISCONTINUITY" {
                broke = true;
            } else if let Some(value) = line.strip_prefix("#EXTINF:")
                && let Ok(seconds) = value.trim_end_matches(',').parse::<f64>()
            {
                listed.push((broke, seconds));
                broke = false;
            }
        }
        listed
    }

    /// Media seconds the playlist announces across the segments it lists.
    fn listed_seconds(playlist: &str) -> f64 {
        listed_segments(playlist)
            .iter()
            .map(|(_, seconds)| seconds)
            .sum()
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
    fn the_break_falls_behind_the_audio_the_gap_came_after() {
        /// Half a second of audio per chunk, far past what the encoder holds
        /// between a push and the access units it hands back.
        const CHUNK: usize = 24_000;
        const CHUNK_SECONDS: f64 = 0.5;
        /// Halfway between the two chunks that came ahead of the gap and the
        /// one that followed it: a break in place has the first two in front of
        /// it and the third behind, whatever the encoder's framing shifts.
        const MIDPOINT: f64 = 1.5 * CHUNK_SECONDS;

        let feed = VecFeed::new(
            [
                (0, pcm(CHUNK)),
                (u64::from(Consts::SAMPLE_RATE), pcm(CHUNK)),
                (0, pcm(CHUNK)),
            ],
            true,
        );

        let playlist = run_worker(feed, &BroadcastConfig::builder().build(), false);
        let listed = listed_segments(&playlist);

        assert_eq!(listed.len(), 2, "{playlist}");
        assert!(!listed[0].0, "the stream opens on one timeline: {playlist}");
        assert!(listed[1].0, "the gap breaks the timeline: {playlist}");
        assert!(
            listed[0].1 > MIDPOINT,
            "audio the producer handed over ahead of the gap belongs ahead of \
             the break: {playlist}"
        );
        assert!(
            listed[1].1 < MIDPOINT,
            "audio the producer handed over past the gap belongs past the \
             break: {playlist}"
        );
    }

    #[test]
    fn stopping_puts_everything_the_feed_holds_on_air() {
        /// Chunks the feed still holds when the stop lands: more than the one
        /// poll a drain that judged the feed by an empty poll would take.
        const HELD: usize = 8;
        /// Media seconds the encoder's framing shifts the tail by.
        const SLACK: f64 = 0.05;

        let held_seconds = f64::from(
            u32::try_from(HELD * Consts::CHUNK_FRAMES).expect("the held audio fits a count"),
        ) / f64::from(Consts::SAMPLE_RATE);
        let feed = BurstFeed::new((0..HELD).map(|_| pcm(Consts::CHUNK_FRAMES)));

        let playlist = run_worker(feed, &config(), true);

        assert!(
            listed_seconds(&playlist) >= held_seconds - SLACK,
            "a stop airs the audio the feed holds, not the audio one poll \
             happened to catch: {held_seconds} s went in, {playlist}"
        );
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
