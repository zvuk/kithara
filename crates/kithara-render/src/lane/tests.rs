use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioReadError, AudioSource, SeekOutcome, TrackStep};
use kithara_command::{Batch, ChannelConfig, Outcome, Sender, Seq, When, channel};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec, SegmentId};
use kithara_test_utils::kithara;
use kithara_warp::{SpeedCurve, Warp, WarpConfig, WarpRenderer};

use crate::{
    LaneCommand, LaneProtocol,
    lane::Lane,
    test_pools::{TestPools, pools},
};

struct ReadySource {
    position: Duration,
}

impl AudioSource for ReadySource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.position = target;
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::Eof
    }
}

struct LaneFixture {
    lane: Lane,
    sender: Sender<LaneProtocol>,
    source: ReadySource,
    renderer: WarpRenderer<TestPools>,
    spec: AudioSpec,
}

impl LaneFixture {
    fn new() -> Self {
        let (sender, inbox) = channel(ChannelConfig::builder().build());
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let warp = Warp::new((), &WarpConfig::builder().build());
        Self {
            lane: Lane::new(
                inbox,
                NonZeroUsize::new(2).expect("preload threshold"),
                crate::consts::DEFAULT_DECLICK,
            ),
            sender,
            source: ReadySource {
                position: Duration::ZERO,
            },
            renderer: warp.renderer(spec, pools()),
            spec,
        }
    }

    fn begin(&mut self, segment: SegmentId) -> Seq {
        self.begin_at(segment, Duration::from_secs(1))
    }

    fn begin_at(&mut self, segment: SegmentId, from: Duration) -> Seq {
        let seq = self
            .sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::Segment {
                        id: segment,
                        from,
                        speed: SpeedCurve::Constant(1.0),
                    }],
                },
            )
            .expect("segment command fits the channel");
        self.lane
            .execute_due(&mut self.source, &mut self.renderer, self.spec)
            .expect("synchronous segment preparation");
        seq
    }

    fn admit_preload(&mut self) {
        self.lane.admitted();
        self.lane.admitted();
    }
}

#[kithara::test]
fn epoch_monotonicity() {
    let mut fixture = LaneFixture::new();
    let first = SegmentId::FIRST.next();
    let second = first.next();
    let third = second.next();
    fixture.begin(first);
    assert_eq!(fixture.lane.cursor().segment, first);
    fixture.begin(second);
    assert_eq!(fixture.lane.cursor().segment, second);
    fixture.begin(third);
    assert_eq!(fixture.lane.cursor().segment, third);
    assert!(first < second && second < third);
}

#[kithara::test]
fn commit_if_epoch_runs_only_for_the_current_epoch() {
    let mut fixture = LaneFixture::new();
    let first = SegmentId::FIRST.next();
    let initial = fixture.begin(first);
    fixture.admit_preload();
    let receipt = fixture.sender.receipts().next().expect("initial receipt");
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(first))
    );
    let second = first.next();
    let current = fixture.begin(second);
    assert_ne!(initial, current);
    fixture.admit_preload();
    let receipt = fixture.sender.receipts().next().expect("current receipt");
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(second))
    );
}

#[kithara::test]
fn seek_epoch_arc_observes_begun_seeks() {
    let mut fixture = LaneFixture::new();
    assert_eq!(fixture.lane.cursor().segment, SegmentId::FIRST);
    let segment = SegmentId::FIRST.next();
    fixture.begin(segment);
    assert_eq!(fixture.lane.cursor().segment, segment);
    fixture.admit_preload();
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("reader-visible readiness");
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
}

#[kithara::test]
fn pending_epoch_marks_and_clears_only_the_matching_seek() {
    let mut fixture = LaneFixture::new();
    assert!(fixture.sender.receipts().next().is_none());
    assert!(!fixture.lane.is_preloaded());
    let first = SegmentId::FIRST.next();
    let stale = fixture.begin(first);
    let current = fixture.begin(first.next());
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("superseded receipt");
    assert_eq!(receipt.seq(), stale);
    assert!(matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready.is_none()));
    assert!(!fixture.lane.is_preloaded());
    fixture.admit_preload();
    let receipt = fixture.sender.receipts().next().expect("current receipt");
    assert_eq!(receipt.seq(), current);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(first.next()))
    );
    assert!(fixture.sender.receipts().next().is_none());
}

#[kithara::test]
fn decoder_seek_latch_is_one_shot_per_begin() {
    let mut fixture = LaneFixture::new();
    assert!(fixture.sender.receipts().next().is_none());
    fixture.begin(SegmentId::FIRST.next());
    fixture.admit_preload();
    assert!(fixture.sender.receipts().next().is_some());
    fixture.lane.admitted();
    assert!(fixture.sender.receipts().next().is_none());
}

#[kithara::test]
fn latch_one_shot() {
    let mut fixture = LaneFixture::new();
    fixture.begin(SegmentId::FIRST.next());
    fixture.admit_preload();
    assert!(
        fixture.sender.receipts().next().is_some(),
        "first take must have a receipt"
    );
    assert!(
        fixture.sender.receipts().next().is_none(),
        "second take must be empty"
    );
    fixture.lane.admitted();
    assert!(
        fixture.sender.receipts().next().is_none(),
        "subsequent takes must be empty"
    );
}

#[kithara::test]
fn initiate_seek_sets_flushing_and_target() {
    let mut fixture = LaneFixture::new();
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.lane.position().is_none());
    let segment = SegmentId::FIRST.next();
    let target = Duration::from_secs(10);
    fixture.begin_at(segment, target);
    assert_eq!(fixture.lane.cursor().segment, segment);
    assert!(!fixture.lane.is_preloaded());
    assert_eq!(fixture.source.position, target);
    assert_eq!(fixture.lane.position(), Some(target));
}

#[kithara::test]
fn complete_seek_clears_flushing() {
    let mut fixture = LaneFixture::new();
    let target = Duration::from_secs(5);
    fixture.begin_at(SegmentId::FIRST.next(), target);
    fixture.admit_preload();
    assert!(fixture.lane.is_preloaded());
    assert_eq!(fixture.lane.position(), Some(target));
}

#[kithara::test]
fn seek_epoch_monotonically_increases() {
    let mut fixture = LaneFixture::new();
    let first = SegmentId::FIRST.next();
    for (segment, seconds) in [(first, 1), (first.next(), 2), (first.next().next(), 3)] {
        fixture.begin_at(segment, Duration::from_secs(seconds));
        assert_eq!(fixture.lane.cursor().segment, segment);
    }
    assert_eq!(fixture.lane.position(), Some(Duration::from_secs(3)));
}

#[kithara::test]
fn initiate_seek_is_visible_across_arc_clones() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    fixture.begin_at(segment, Duration::from_secs(7));
    fixture.admit_preload();
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("cross-owner receipt");
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
    assert_eq!(fixture.source.position, Duration::from_secs(7));
}

#[kithara::test]
fn initiate_seek_sets_seek_pending() {
    let mut fixture = LaneFixture::new();
    fixture.admit_preload();
    assert!(fixture.lane.is_preloaded());
    fixture.begin(SegmentId::FIRST.next());
    assert!(!fixture.lane.is_preloaded());
}

#[kithara::test]
fn clear_seek_pending_only_clears_matching_epoch() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    fixture.begin(segment);
    fixture.lane.admitted();
    fixture.begin(segment.next());
    assert!(!fixture.lane.is_preloaded());
    fixture.lane.admitted();
    assert!(!fixture.lane.is_preloaded());
    fixture.lane.admitted();
    assert!(fixture.lane.is_preloaded());
}

#[kithara::test]
fn new_initiate_seek_resets_seek_pending() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    fixture.begin(segment);
    fixture.admit_preload();
    assert!(fixture.lane.is_preloaded());
    fixture.begin(segment.next());
    assert!(!fixture.lane.is_preloaded());
}

#[kithara::test]
fn complete_seek_does_not_clear_seek_pending() {
    let mut fixture = LaneFixture::new();
    let target = Duration::from_secs(5);
    fixture.begin_at(SegmentId::FIRST.next(), target);
    assert_eq!(fixture.source.position, target);
    assert!(!fixture.lane.is_preloaded());
}

#[kithara::test]
fn is_seek_pending_visible_across_arc_clones() {
    let mut fixture = LaneFixture::new();
    fixture.begin(SegmentId::FIRST.next());
    assert!(fixture.sender.receipts().next().is_none());
}

#[kithara::test]
fn flag_pair_matrix_matches_bitflags_snapshot() {
    for mask in 0u8..4 {
        let mut fixture = LaneFixture::new();
        let mut writer = kithara_stream::ActivityWriter::new();
        let reader = writer.reader();
        let clone = reader.clone();
        let playing = mask & 1 != 0;
        let ready = mask & 2 != 0;
        fixture.begin(SegmentId::FIRST.next());
        writer.set_playing(playing);
        if ready {
            fixture.admit_preload();
        }
        assert_eq!(reader.is_playing(), playing, "mask {mask:#04b} playing");
        assert_eq!(fixture.lane.is_preloaded(), ready, "mask {mask:#04b} ready");
        assert_eq!(
            clone.is_playing(),
            playing,
            "mask {mask:#04b} activity snapshot"
        );
        assert_eq!(
            fixture.sender.receipts().next().is_some(),
            ready,
            "mask {mask:#04b} readiness receipt"
        );
    }
}

#[kithara::test]
fn complete_seek_double_check_re_raises_flushing_when_newer_seek_interleaves() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    fixture.begin(segment);
    fixture.lane.admitted();
    fixture.begin(segment.next());
    assert!(
        !fixture.lane.is_preloaded(),
        "a newer segment needs its own preload admissions"
    );
}

#[kithara::test]
fn concurrent_flag_toggles_preserve_independent_semantics() {
    let mut writer = kithara_stream::ActivityWriter::new();
    let reader = writer.reader();
    let observed = reader.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let clone = std::sync::Arc::clone(&barrier);
    let reader_thread = std::thread::spawn(move || {
        clone.wait();
        for _ in 0..50_000 {
            std::hint::black_box(observed.is_playing());
        }
    });
    let mut fixture = LaneFixture::new();
    let mut segment = SegmentId::FIRST;
    barrier.wait();
    for index in 0..50_000 {
        writer.set_playing(index % 2 == 0);
        segment = segment.next();
        fixture.begin(segment);
        fixture.admit_preload();
        let receipt = fixture.sender.receipts().next().expect("completed segment");
        assert!(
            matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
        );
    }
    reader_thread.join().expect("reader thread");
    assert!(!reader.is_playing(), "activity matches the last write");
    assert!(fixture.lane.is_preloaded(), "last segment is fully ready");
    assert!(
        fixture.sender.receipts().next().is_none(),
        "last completion is consumed"
    );
}

#[kithara::test]
fn playing_is_orthogonal_to_other_flags() {
    for ready in [false, true] {
        for playing in [false, true] {
            let mut writer = kithara_stream::ActivityWriter::new();
            let reader = writer.reader();
            let mut fixture = LaneFixture::new();
            let segment = SegmentId::FIRST.next();
            fixture.begin(segment);
            if ready {
                fixture.admit_preload();
            }
            writer.set_playing(playing);
            assert_eq!(reader.is_playing(), playing);
            assert_eq!(fixture.lane.is_preloaded(), ready);
            assert_eq!(fixture.lane.cursor().segment, segment);
            writer.set_playing(!playing);
            assert_eq!(reader.is_playing(), !playing);
            assert_eq!(fixture.lane.is_preloaded(), ready);
            assert_eq!(fixture.lane.cursor().segment, segment);
        }
    }
}

#[kithara::test]
fn wait_resolves_after_signal() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    let seq = fixture.begin(segment);
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());

    fixture.admit_preload();

    assert!(fixture.lane.is_preloaded());
    let receipt = fixture.sender.receipts().next().expect("preload receipt");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
    assert!(fixture.sender.receipts().next().is_none());
}

#[kithara::test]
fn rearm_reblocks_a_fresh_wait() {
    let mut fixture = LaneFixture::new();
    let first = SegmentId::FIRST.next();
    fixture.begin(first);
    fixture.admit_preload();
    assert!(fixture.sender.receipts().next().is_some());

    let second = first.next();
    let seq = fixture.begin(second);
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());

    fixture.admit_preload();
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("fresh preload receipt");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(second))
    );
}

#[kithara::test]
fn old_epoch_signal_does_not_open_new_epoch_wait() {
    let mut fixture = LaneFixture::new();
    let first = SegmentId::FIRST.next();
    let old_seq = fixture.begin(first);
    fixture.lane.admitted();
    let second = first.next();
    let current_seq = fixture.begin(second);

    let old = fixture
        .sender
        .receipts()
        .next()
        .expect("superseded receipt");
    assert_eq!(old.seq(), old_seq);
    assert!(matches!(old.outcome(), Outcome::Applied { data, .. } if data.ready.is_none()));
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());

    fixture.admit_preload();

    assert!(fixture.lane.is_preloaded());
    let current = fixture
        .sender
        .receipts()
        .next()
        .expect("current preload receipt");
    assert_eq!(current.seq(), current_seq);
    assert!(
        matches!(current.outcome(), Outcome::Applied { data, .. } if data.ready == Some(second))
    );
    assert!(fixture.sender.receipts().next().is_none());
}

#[kithara::test]
fn seek_preserves_preload_completed_before_begin_returns() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    fixture.begin(segment);
    fixture.admit_preload();
    assert!(
        fixture.lane.is_preloaded(),
        "seek must preserve the worker's readiness signal"
    );
    let receipt = fixture.sender.receipts().next().expect("ready receipt");
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
}

#[kithara::test]
fn seek_rearms_preload_gate_before_worker_refill() {
    let mut fixture = LaneFixture::new();
    fixture.begin(SegmentId::FIRST.next());
    fixture.admit_preload();
    assert!(fixture.lane.is_preloaded());
    fixture.begin_at(SegmentId::FIRST.next().next(), Duration::from_millis(250));
    assert!(!fixture.lane.is_preloaded());
}
#[kithara::test]
fn off_rt_read_publishes_the_seek_completion_it_births() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    let seq = fixture.begin(segment);
    fixture.admit_preload();
    assert!(fixture.lane.is_preloaded());
    let receipt = fixture.sender.receipts().next().expect("ready receipt");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
    assert!(fixture.sender.receipts().next().is_none());
}
#[kithara::test]
fn an_adopted_realtime_mode_moves_the_reader_events_with_the_ring() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    let seq = fixture.begin(segment);
    fixture.lane.admitted();
    assert!(fixture.sender.receipts().next().is_none());
    fixture.lane.admitted();
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("worker-owned readiness");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
    assert!(fixture.sender.receipts().next().is_none());
}
#[kithara::test]
fn realtime_read_leaves_its_seek_completion_for_the_shell() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    let seq = fixture.begin(segment);
    fixture.lane.admitted();
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());
    fixture.lane.admitted();
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("owner publishes readiness");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
    assert!(fixture.sender.receipts().next().is_none());
}
#[kithara::test]
fn post_seek_output_reaches_the_bus_once_the_shell_flushes() {
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    let target = Duration::from_secs(1);
    let seq = fixture.begin_at(segment, target);
    fixture.lane.admitted();
    assert!(fixture.sender.receipts().next().is_none());
    fixture.lane.admitted();
    let receipt = fixture
        .sender
        .receipts()
        .next()
        .expect("readiness replaces seek bus completion");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(segment))
    );
    assert_eq!(fixture.lane.position(), Some(target));
    assert!(fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());
}
