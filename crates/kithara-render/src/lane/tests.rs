use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioReadError, AudioSource, SeekOutcome, TrackFailureKind, TrackStep};
use kithara_command::{Batch, ChannelConfig, Outcome, Rejection, Sender, Seq, When, channel};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, SegmentId, SourceSpan};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;
use kithara_warp::{Warp, WarpConfig, WarpRenderer};

use crate::{
    LaneCommand, LaneFrame, LaneProtocol,
    lane::Lane,
    test_pools::{TestPools, pools, sample_buffer},
};

struct ReadySource {
    position: Duration,
    pending: bool,
    seeks: Vec<Duration>,
}

impl AudioSource for ReadySource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seeks.push(target);
        if self.pending {
            return Ok(SeekOutcome::Pending {
                target,
                reason: kithara_stream::PendingReason::NotReady(
                    kithara_stream::NotReadyCause::SourcePending,
                ),
            });
        }
        self.position = target;
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn seek_for_render(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seek(target)
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
    entry: AudioChunkInfo,
}

/// A chunk that starts at `from` under the render revision of the segment command `seq`.
fn entering(spec: AudioSpec, seq: Seq, from: Duration) -> AudioChunkInfo {
    let start = spec.frame_at(from).expect("representable entry");
    AudioChunkInfo {
        spec,
        frames: 16,
        frame_offset: start,
        render_revision: seq.get(),
        source_span: SourceSpan::new(start, start + 16, spec.sample_rate, 16)
            .map(|span| span.with_render_revision(seq.get())),
        ..AudioChunkInfo::default()
    }
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
                pending: false,
                seeks: Vec::new(),
            },
            renderer: warp.renderer(spec, pools()),
            spec,
            entry: AudioChunkInfo::default(),
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
        self.entry = entering(self.spec, seq, from);
        seq
    }

    fn admit(&mut self) {
        let entry = AudioChunkInfo {
            segment: self.lane.cursor().segment,
            ..self.entry
        };
        if self
            .lane
            .observe_input(&mut self.renderer, entry, entry.visible_origin)
            .is_err()
        {
            self.lane.reject_preparation(TrackFailureKind::Render);
            return;
        }
        if let Err(failure) = self.lane.check_admission(&entry) {
            self.lane.reject_preparation(failure);
            return;
        }
        self.lane.admitted();
    }

    fn admit_preload(&mut self) {
        self.admit();
        self.admit();
    }
}

/// Media has no PCM before its visible origin, so a segment positioned ahead of
/// it enters at that origin. Inside the media a segment still enters exactly
/// where it was positioned.
#[kithara::test]
#[case::ahead_of_the_media(Duration::ZERO, None)]
#[case::inside_the_media(Duration::from_millis(500), Some(TrackFailureKind::Render))]
fn a_segment_positioned_ahead_of_the_visible_origin_enters_there(
    #[case] from: Duration,
    #[case] refusal: Option<TrackFailureKind>,
) {
    let visible_origin = Duration::from_millis(13);
    let mut fixture = LaneFixture::new();
    let segment = SegmentId::FIRST.next();
    let seq = fixture.begin_at(segment, from);
    fixture.entry = AudioChunkInfo {
        visible_origin: Some(visible_origin),
        ..entering(fixture.spec, seq, visible_origin)
    };
    fixture.admit_preload();

    let receipt = fixture.sender.receipts().next().expect("readiness verdict");
    assert_eq!(receipt.seq(), seq);
    let outcome = receipt.outcome();
    match refusal {
        None => assert!(
            matches!(outcome, Outcome::Applied { data, .. } if data.ready == Some(segment)),
            "first PCM at the visible origin enters a segment positioned ahead of it: {outcome:?}"
        ),
        Some(expected) => assert!(
            matches!(outcome, Outcome::Rejected(Rejection::Refused(refused)) if *refused == expected),
            "first PCM at the visible origin misses a segment positioned inside the media: \
             {outcome:?}"
        ),
    }
}

#[kithara::test]
#[case::next(When::Next)]
#[case::at(When::At(LaneFrame { segment: SegmentId::FIRST, frame: 16 }))]
fn a_prepared_segment_receipt_keeps_its_actual_apply_point(#[case] when: When<LaneFrame>) {
    let mut fixture = LaneFixture::new();
    let mut chunk = AudioChunk::new(
        AudioChunkInfo {
            spec: fixture.spec,
            frames: 16,
            ..AudioChunkInfo::default()
        },
        sample_buffer(&pools(), &[0.25; 32]),
    );
    fixture.lane.stamp(&mut chunk);
    let applied_at = fixture.lane.cursor();
    let segment = SegmentId::FIRST.next();
    let seq = fixture
        .sender
        .send(
            when,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: segment,
                    from: Duration::from_secs(1),
                    speed: SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("segment command fits the channel");
    fixture
        .lane
        .execute_due(&mut fixture.source, &mut fixture.renderer, fixture.spec)
        .expect("segment applies before its preload");
    assert_eq!(fixture.lane.cursor(), LaneFrame { segment, frame: 0 });
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());
    let entry = AudioChunkInfo {
        segment,
        ..entering(fixture.spec, seq, Duration::from_secs(1))
    };
    fixture
        .lane
        .observe_input(&mut fixture.renderer, entry, entry.visible_origin)
        .expect("physical entry");
    fixture
        .lane
        .check_admission(&entry)
        .expect("the first native PCM and rendered span match the segment entry");
    fixture.lane.stamp(&mut chunk);
    fixture.lane.admitted();
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());
    fixture.lane.stamp(&mut chunk);
    fixture.lane.admitted();
    assert!(fixture.lane.is_preloaded());
    assert_eq!(fixture.lane.cursor(), LaneFrame { segment, frame: 32 });
    let receipt = fixture.sender.receipts().next().expect("prepared receipt");
    assert_eq!(receipt.seq(), seq);
    assert!(matches!(receipt.outcome(), Outcome::Applied { at, data }
        if *at == applied_at && data.ready == Some(segment) && data.source_basis == Some(entry)));
    assert_eq!(&chunk.samples[..], &[0.25; 32]);
    assert!(fixture.sender.receipts().next().is_none());
    fixture.lane.admitted();
    assert!(fixture.sender.receipts().next().is_none());
    fixture
        .sender
        .send(
            When::Deferred,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: segment.next(),
                    from: Duration::ZERO,
                    speed: SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("deferred repeat");
    fixture
        .lane
        .execute_due(&mut fixture.source, &mut fixture.renderer, fixture.spec)
        .expect("repeat waits for EOF");
    let before = fixture.lane.cursor();
    let restore = fixture
        .sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: segment,
                    from: Duration::from_secs(1),
                    speed: SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("cancel the pending repeat");
    fixture
        .lane
        .execute_due(&mut fixture.source, &mut fixture.renderer, fixture.spec)
        .expect("unchanged segment restored");
    assert_eq!(fixture.lane.cursor(), before);
    let receipt = fixture
        .sender
        .receipts()
        .find(|receipt| receipt.seq() == restore)
        .expect("unchanged segment receipt");
    assert!(matches!(receipt.outcome(), Outcome::Applied { data, .. }
        if data.ready == Some(segment) && data.source_basis.is_none()));

    let send = |fixture: &mut LaneFixture, when, id, from| {
        fixture
            .sender
            .send(
                when,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::Segment {
                        id,
                        from,
                        speed: SpeedCurve::Constant(1.0),
                    }],
                },
            )
            .expect("segment credit")
    };
    let original_target = Duration::from_secs(2);
    let repeat_target = Duration::from_secs(7);
    let restore_target = Duration::from_secs(9);
    let current = SegmentId::FIRST.next();
    let repeat = current.next();

    let mut waiting = LaneFixture::new();
    waiting.source.pending = true;
    let original = waiting.begin_at(current, original_target);
    let revision = waiting.renderer.render_revision();
    assert_eq!(revision, original.get());
    assert_eq!(waiting.lane.position(), None);
    assert!(!waiting.lane.is_preloaded());
    send(&mut waiting, When::Deferred, repeat, repeat_target);
    waiting
        .lane
        .execute_due(&mut waiting.source, &mut waiting.renderer, waiting.spec)
        .expect("repeat waits for unresolved original");
    let cancelled = send(&mut waiting, When::Next, current, restore_target);
    waiting
        .lane
        .execute_due(&mut waiting.source, &mut waiting.renderer, waiting.spec)
        .expect("cancel queued repeat");
    assert_eq!(waiting.source.seeks, [original_target]);
    assert_eq!(waiting.renderer.render_revision(), revision);
    assert_eq!(waiting.lane.position(), None);
    assert!(!waiting.lane.is_preloaded());
    let mut cancellation = 0;
    for receipt in waiting.sender.receipts() {
        assert_ne!(
            receipt.seq(),
            original,
            "unresolved original has no receipt"
        );
        if receipt.seq() == cancelled {
            assert!(matches!(receipt.outcome(), Outcome::Applied { data, .. }
                if data.ready.is_none() && data.source_basis.is_none()));
            cancellation += 1;
        }
    }
    assert_eq!(cancellation, 1);
    waiting.admit_preload();
    assert!(waiting.lane.is_preloaded());
    let receipt = waiting
        .sender
        .receipts()
        .next()
        .expect("original PCM admission");
    assert_eq!(receipt.seq(), original);
    assert!(matches!(receipt.outcome(), Outcome::Applied { data, .. }
        if data.ready == Some(current) && data.source_basis.is_some_and(|meta|
            meta.render_revision == revision &&
            meta.source_span.and_then(|span| span.position_at(0)) == Some(original_target))));
    assert!(waiting.sender.receipts().next().is_none());
    assert_eq!(waiting.source.seeks, [original_target]);

    for known_position in [true, false] {
        let mut repeated = LaneFixture::new();
        let old = if known_position {
            let seq = repeated.begin_at(current, original_target);
            repeated.admit_preload();
            assert_eq!(
                repeated
                    .sender
                    .receipts()
                    .next()
                    .expect("old preparation")
                    .seq(),
                seq
            );
            let start = repeated
                .spec
                .frame_at(original_target)
                .expect("actual source start");
            let mut pcm = AudioChunk::new(
                AudioChunkInfo {
                    segment: current,
                    frames: 48,
                    source_span: SourceSpan::new(start, start + 48, repeated.spec.sample_rate, 48)
                        .map(|span| span.with_render_revision(seq.get())),
                    ..entering(repeated.spec, seq, original_target)
                },
                sample_buffer(&pools(), &[0.25; 96]),
            );
            repeated.lane.stamp(&mut pcm);
            assert_eq!(pcm.samples.as_ref(), &[0.25; 96]);
            assert_eq!(
                repeated.lane.position(),
                Some(original_target + Duration::from_millis(1))
            );
            current
        } else {
            SegmentId::FIRST
        };
        let actual = repeated.lane.position();
        let old_cursor = repeated.lane.cursor();
        assert_eq!(actual.is_some(), known_position);
        if let Some(actual) = actual {
            assert_ne!(actual, repeat_target);
            assert_ne!(actual, restore_target);
            assert_ne!(actual, Duration::ZERO);
        }
        repeated.lane.finish_preload();
        repeated.lane.finish_segment();
        repeated.source.pending = true;
        let executing_repeat = send(&mut repeated, When::Deferred, repeat, repeat_target);
        repeated
            .lane
            .execute_due(&mut repeated.source, &mut repeated.renderer, repeated.spec)
            .expect("deferred repeat executes after terminal admission");
        assert_eq!(repeated.lane.cursor().segment, repeat);
        assert_eq!(repeated.lane.position(), None);
        let before_restore = repeated.source.seeks.len();
        let restore = send(&mut repeated, When::Next, old, restore_target);
        let result =
            repeated
                .lane
                .execute_due(&mut repeated.source, &mut repeated.renderer, repeated.spec);
        if let Some(actual) = actual {
            result.expect("known outgoing coordinate restores");
            assert_eq!(repeated.lane.cursor(), old_cursor);
            assert_eq!(repeated.source.seeks.len(), before_restore + 1);
            assert_eq!(repeated.source.seeks.last(), Some(&actual));
            assert_eq!(repeated.lane.position(), None);
            assert!(!repeated.lane.is_preloaded());
            let receipt = repeated
                .sender
                .receipts()
                .next()
                .expect("superseded repeat receipt");
            assert_eq!(receipt.seq(), executing_repeat);
            assert!(
                matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready.is_none())
            );
            assert!(repeated.sender.receipts().next().is_none());
            repeated.entry = AudioChunkInfo {
                lane_frame: old_cursor.frame,
                ..entering(repeated.spec, restore, actual)
            };
            repeated.admit_preload();
            assert!(repeated.lane.is_preloaded());
            let receipt = repeated
                .sender
                .receipts()
                .next()
                .expect("restored PCM admission");
            assert_eq!(receipt.seq(), restore);
            assert!(matches!(receipt.outcome(), Outcome::Applied { data, .. }
                if data.ready == Some(old) && data.source_basis.is_some_and(|meta|
                    meta.source_span.and_then(|span| span.position_at(0)) == Some(actual))));
            assert!(repeated.sender.receipts().next().is_none());
        } else {
            assert!(matches!(result, Err(TrackFailureKind::Render)));
            assert_eq!(repeated.source.seeks.len(), before_restore);
            assert_eq!(repeated.lane.cursor().segment, repeat);
            repeated.lane.reject_preparation(TrackFailureKind::Render);
            let receipts: Vec<_> = repeated.sender.receipts().collect();
            assert_eq!(receipts.len(), 2);
            for seq in [restore, executing_repeat] {
                assert_eq!(
                    receipts
                        .iter()
                        .filter(|receipt| receipt.seq() == seq
                            && matches!(
                                receipt.outcome(),
                                Outcome::Rejected(Rejection::Refused(TrackFailureKind::Render))
                            ))
                        .count(),
                    1
                );
            }
            repeated.lane.reject_preparation(TrackFailureKind::Render);
            repeated
                .lane
                .execute_due(&mut repeated.source, &mut repeated.renderer, repeated.spec)
                .expect("refused commands cannot run again");
            assert!(repeated.sender.receipts().next().is_none());
            assert_eq!(repeated.source.seeks.len(), before_restore);
        }
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
    fixture.admit();
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
    fixture.admit();
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
    fixture.admit();
    fixture.begin(segment.next());
    assert!(!fixture.lane.is_preloaded());
    fixture.admit();
    assert!(!fixture.lane.is_preloaded());
    fixture.admit();
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
    fixture.admit();
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
    fixture.admit();
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
    fixture.admit();
    assert!(fixture.sender.receipts().next().is_none());
    fixture.admit();
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
    fixture.admit();
    assert!(!fixture.lane.is_preloaded());
    assert!(fixture.sender.receipts().next().is_none());
    fixture.admit();
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
    fixture.admit();
    assert!(fixture.sender.receipts().next().is_none());
    fixture.admit();
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
