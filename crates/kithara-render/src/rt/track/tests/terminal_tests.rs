use std::num::NonZeroU32;

use kithara_assets::AssetStore;
use kithara_audio::{
    Audio, AudioConfig, AudioEvent, AudioReadError, AudioSource, DecodeErrorKind, FailureSource,
    NoResamplerBackend, SeekOutcome, TrackFailureKind, TrackStep,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, SegmentId, SessionFrame};
use kithara_stream::{Stream, mock::NoopWorkerWake};
use kithara_test_utils::{cancel_token, kithara};
use kithara_worker::{Task, TickResult};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

use super::super::{PcmConsumer, PlayerResource, PlayerTrack, ReadOutcome, RtSink};
use crate::{
    bridge::{DeckEvent, Fade, PlaybackFault, RtMetrics, Slot},
    test_pools::{TestPools, pools, sample_buffer},
    worker::{PcmPacket, PcmReceiver, terminal_node, terminal_ring},
};

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"))
}

fn resource(receiver: PcmReceiver) -> PlayerResource {
    PlayerResource::new(
        PcmConsumer::new(receiver),
        Arc::from("repeated.wav"),
        &pools(),
    )
    .expect("packet resource")
}

fn observe(reader: &mut PlayerResource) -> Option<ReadOutcome> {
    reader.poll_end(&mut 8)
}

fn failed_packet(failure: TrackFailureKind) -> PcmPacket {
    PcmPacket::Failed {
        segment: SegmentId::FIRST,
        failure,
    }
}

#[kithara::test]
#[case::nonblocking(false)]
#[case::blocking(true)]
fn a_committed_failure_marker_wins_over_producer_closure(#[case] blocking: bool) {
    let (receiver, mut producer) = terminal_ring(blocking, spec());
    let failure = TrackFailureKind::RecreateFailed { offset: 91 };
    assert!(producer(failed_packet(failure)).is_ok());
    drop(producer);
    let mut reader = resource(receiver);
    for _ in 0..2 {
        assert_eq!(
            observe(&mut reader),
            Some(ReadOutcome::Failed(FailureSource::Producer { failure }))
        );
        assert!(!reader.eof);
    }
}

#[kithara::test]
#[case::nonblocking(false)]
#[case::blocking(true)]
fn committed_pcm_is_drained_before_an_unmarked_producer_closure(#[case] blocking: bool) {
    let (receiver, mut producer) = terminal_ring(blocking, spec());
    let chunk = AudioChunk::new(
        AudioChunkInfo {
            spec: spec(),
            frames: 1,
            ..Default::default()
        },
        sample_buffer(&pools(), &[0.25, 0.5]),
    );
    assert!(producer(PcmPacket::Chunk(Box::new(chunk))).is_ok());
    drop(producer);
    let mut reader = resource(receiver);
    assert_eq!(observe(&mut reader), None);
    assert!(!reader.eof);
    assert_eq!(reader.failed, None);
    let mut left = [0.0];
    let mut right = [0.0];
    assert_eq!(
        reader.read(&mut [&mut left, &mut right], 0..1, &mut 8),
        ReadOutcome::Full { frames: 1 }
    );
    assert_eq!(left, [0.25]);
    assert_eq!(right, [0.5]);
    assert_eq!(
        observe(&mut reader),
        Some(ReadOutcome::Failed(FailureSource::ChannelClosed))
    );
}

#[kithara::test]
fn process_fetch_must_distinguish_failure_from_natural_eof() {
    let (receiver, mut producer) = terminal_ring(false, spec());
    let eof = AudioChunk::new(
        AudioChunkInfo {
            spec: spec(),
            end_of_track: true,
            ..Default::default()
        },
        sample_buffer(&pools(), &[]),
    );
    assert!(producer(PcmPacket::Chunk(Box::new(eof))).is_ok());
    let mut natural = resource(receiver);
    assert_eq!(observe(&mut natural), Some(ReadOutcome::Eof));
    let (receiver, mut producer) = terminal_ring(false, spec());
    assert!(producer(failed_packet(TrackFailureKind::SourceCancelled)).is_ok());
    let mut failed = resource(receiver);
    assert_ne!(observe(&mut failed), Some(ReadOutcome::Eof));
    assert_eq!(
        observe(&mut failed),
        Some(ReadOutcome::Failed(FailureSource::Producer {
            failure: TrackFailureKind::SourceCancelled
        }))
    );
}

#[kithara::test]
fn a_stale_producer_failure_survives_a_new_seek_epoch() {
    let (receiver, mut producer) = terminal_ring(false, spec());
    assert!(producer(failed_packet(TrackFailureKind::SourceCancelled)).is_ok());
    let mut reader = resource(receiver);
    reader.select_segment(SegmentId::FIRST.next());
    assert_eq!(
        observe(&mut reader),
        Some(ReadOutcome::Failed(FailureSource::ProducerAfterSeek {
            failure: TrackFailureKind::SourceCancelled
        }))
    );
}

#[kithara::test]
fn a_stale_producer_failure_terminates_the_consumer() {
    let (receiver, mut producer) = terminal_ring(false, spec());
    let mut reader = resource(receiver);
    reader.lane.segment = SegmentId::FIRST.next().next().next();
    assert!(producer(failed_packet(TrackFailureKind::SourceCancelled)).is_ok());
    assert_eq!(
        observe(&mut reader),
        Some(ReadOutcome::Failed(FailureSource::Producer {
            failure: TrackFailureKind::SourceCancelled
        }))
    );
    assert!(!reader.eof);
}

struct FailedSource(TrackFailureKind);

impl AudioSource for FailedSource {
    type Chunk = AudioChunk;
    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::Failed(self.0)
    }
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
}

type FileAudio = Audio<Stream<kithara_file::File<TestPools>>>;

async fn real_audio(cancel: kithara_platform::CancelToken) -> FileAudio {
    let pools = pools();
    let path = kithara_test_fixtures::assets::signal_wav_sine440_120ms()
        .path()
        .expect("native WAV fixture path");
    let stream = kithara_file::FileConfig::for_src(kithara_file::FileSrc::Local(path.to_owned()))
        .store(AssetStore::builder(pools.clone()).build())
        .pools(pools.clone())
        .cancel(cancel.clone())
        .build();
    let config = AudioConfig::<_, NoResamplerBackend>::for_stream(stream)
        .cancel(cancel)
        .build();
    Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
        .await
        .expect("real file audio")
}

#[kithara::test(native, tokio)]
#[case::normal(false)]
#[case::first_seek(true)]
async fn real_source_cancellation_survives_the_decoder_marker(#[case] seek_before_read: bool) {
    let cancel = cancel_token();
    let audio = real_audio(cancel.clone()).await;
    let mut events = audio.event_bus().subscribe();
    let (mut node, receiver, _lane) = terminal_node(audio, spec(), false);
    cancel.cancel();
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(matches!(
        receiver.peek(),
        Some(PcmPacket::Failed {
            segment: SegmentId::FIRST,
            failure: TrackFailureKind::SourceCancelled
        })
    ));
    node.recycle();
    let failures = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|envelope| match envelope.event {
            AudioEvent::TrackFailed { failure } => Some(failure),
            AudioEvent::EndOfStream => panic!("source cancellation is not natural EOF"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failures, [TrackFailureKind::SourceCancelled]);
    let mut reader = resource(receiver);
    if seek_before_read {
        reader.select_segment(SegmentId::FIRST.next());
        assert_eq!(
            reader.lane.segment,
            SegmentId::FIRST.next(),
            "the terminal marker is now stale"
        );
    }
    let first = observe(&mut reader).expect("real source emitted terminal marker");
    let ReadOutcome::Failed(first) = first else {
        panic!("failure is not EOF")
    };
    assert!(first.to_string().contains("source cancelled"));
    reader.select_segment(SegmentId::FIRST.next().next());
    let repeated = observe(&mut reader).expect("terminal survives another seek");
    assert_eq!(repeated, ReadOutcome::Failed(first));
    assert!(first.to_string().contains("source cancelled"));
}

#[kithara::test(native)]
#[case::normal(false)]
#[case::first_seek(true)]
fn typed_failures_survive_the_real_warp_node_and_reader(#[case] seek_before_read: bool) {
    let kinds = [
        DecodeErrorKind::Io,
        DecodeErrorKind::UnsupportedCodec,
        DecodeErrorKind::UnsupportedContainer,
        DecodeErrorKind::InvalidData,
        DecodeErrorKind::SeekFailed,
        DecodeErrorKind::SeekOutOfRange,
        DecodeErrorKind::Parse,
        DecodeErrorKind::ProbeFailed,
        DecodeErrorKind::BackendUnavailable,
        DecodeErrorKind::InvalidSampleRate,
        DecodeErrorKind::BackendStatus,
        DecodeErrorKind::Interrupted,
        DecodeErrorKind::Backend,
    ];
    let failures = kinds
        .into_iter()
        .map(|kind| TrackFailureKind::Decode { kind })
        .chain([
            TrackFailureKind::RecreateFailed { offset: 0 },
            TrackFailureKind::RecreateFailed { offset: 91 },
            TrackFailureKind::RecreateFailed { offset: u64::MAX },
            TrackFailureKind::SourceCancelled,
            TrackFailureKind::ChannelClosed,
            TrackFailureKind::Render,
        ]);
    for failure in failures {
        let (mut node, receiver, _lane) = terminal_node(FailedSource(failure), spec(), false);
        assert_eq!(node.tick(), TickResult::Progress, "{failure:?}");
        assert!(
            matches!(receiver.peek(), Some(PcmPacket::Failed { segment: SegmentId::FIRST, failure: actual }) if *actual == failure)
        );
        let mut reader = resource(receiver);
        if seek_before_read {
            reader.select_segment(SegmentId::FIRST.next());
        }
        let expected = if seek_before_read {
            FailureSource::ProducerAfterSeek { failure }
        } else {
            FailureSource::Producer { failure }
        };
        assert_eq!(
            observe(&mut reader),
            Some(ReadOutcome::Failed(expected)),
            "{failure:?}"
        );
        reader.select_segment(SegmentId::FIRST.next().next());
        let mut left = [0.0; 8];
        let mut right = [0.0; 8];
        assert_eq!(
            reader.read(&mut [&mut left, &mut right], 0..8, &mut 8),
            ReadOutcome::Failed(expected),
            "{failure:?}"
        );
        assert_eq!(node.tick(), TickResult::Backpressured);
    }
}

#[kithara::test(native, tokio)]
async fn an_unmarked_producer_drop_with_a_live_token_is_channel_closed() {
    let cancel = cancel_token();
    let (node, receiver, _lane) = terminal_node(real_audio(cancel.clone()).await, spec(), false);
    assert!(receiver.peek().is_none());
    assert!(!receiver.is_closed());
    drop(node);
    assert!(
        !cancel.is_cancelled(),
        "producer drop must not cancel the source token"
    );
    let mut reader = resource(receiver);
    for _ in 0..2 {
        assert_eq!(
            observe(&mut reader),
            Some(ReadOutcome::Failed(FailureSource::ChannelClosed))
        );
    }
}

#[kithara::test(native, tokio)]
async fn an_unmarked_decoder_shutdown_is_distinct_from_eof_and_source_cancel() {
    let cancel = cancel_token();
    let (node, receiver, _lane) = terminal_node(real_audio(cancel.clone()).await, spec(), false);
    drop(node);
    let mut reader = resource(receiver);
    let Some(ReadOutcome::Failed(source)) = observe(&mut reader) else {
        panic!("unmarked shutdown is not EOF")
    };
    let reason = source.to_string();
    assert!(reason.contains("channel closed"), "{reason}");
    assert!(reason.contains("no failure marker"), "{reason}");
    assert!(!reason.contains("source cancelled"), "{reason}");
    assert_eq!(observe(&mut reader), Some(ReadOutcome::Failed(source)));
}

#[kithara::test(native, tokio)]
async fn a_consumed_terminal_marker_keeps_its_cause_after_another_seek() {
    let cancel = cancel_token();
    let (mut node, receiver, _lane) =
        terminal_node(real_audio(cancel.clone()).await, spec(), false);
    cancel.cancel();
    assert_eq!(node.tick(), TickResult::Progress);
    let mut reader = resource(receiver);
    let Some(ReadOutcome::Failed(source)) = observe(&mut reader) else {
        panic!("terminal marker")
    };
    let first_reason = source.to_string();
    assert!(first_reason.contains("producer"), "{first_reason}");
    assert!(!first_reason.contains("channel closed"), "{first_reason}");
    reader.select_segment(SegmentId::FIRST.next());
    assert_eq!(reader.lane.segment, SegmentId::FIRST.next());
    let Some(ReadOutcome::Failed(repeated)) = observe(&mut reader) else {
        panic!("terminal persists")
    };
    assert_eq!(repeated.to_string(), first_reason);
}

#[kithara::rtsan_forbid_blocking]
fn checked_stream_terminal_reads(reader: &mut PlayerResource) -> [bool; 3] {
    std::array::from_fn(|_| {
        let mut left = [0.0; 8];
        let mut right = [0.0; 8];
        let result = reader.read(&mut [&mut left, &mut right], 0..8, &mut 8);
        let ReadOutcome::Failed(source) = result else {
            return false;
        };
        let error = AudioReadError::Stream {
            what: "read PCM",
            source,
        };
        let result = matches!(
            error,
            AudioReadError::Stream {
                source: FailureSource::Producer {
                    failure: TrackFailureKind::SourceCancelled
                },
                ..
            }
        );
        drop(error);
        result
    })
}

#[kithara::test(native, tokio)]
async fn real_stream_terminal_error_construction_and_drop_are_rt_safe() {
    let cancel = cancel_token();
    let (mut node, receiver, _lane) =
        terminal_node(real_audio(cancel.clone()).await, spec(), false);
    cancel.cancel();
    assert_eq!(node.tick(), TickResult::Progress);
    let mut reader = resource(receiver);
    assert_eq!(checked_stream_terminal_reads(&mut reader), [true; 3]);
}

#[kithara::test(native, tokio)]
async fn a_real_source_failure_keeps_its_cause_in_the_single_stop_notification() {
    let cancel = cancel_token();
    let (mut node, receiver, _lane) =
        terminal_node(real_audio(cancel.clone()).await, spec(), false);
    cancel.cancel();
    assert_eq!(node.tick(), TickResult::Progress);
    let mut track = PlayerTrack::builder()
        .sample_rate(spec().sample_rate)
        .build(Box::new(resource(receiver)));
    track.start(Fade::Declick);
    let (mut events, mut received) = HeapRb::<DeckEvent>::new(8).split();
    let metrics = RtMetrics::default();
    let fault = PlaybackFault::Source(TrackFailureKind::SourceCancelled);
    let slot = Slot::new(0);
    let mut sink = RtSink::new(&mut events, &metrics, slot, SessionFrame::new(7));
    assert!(track.poll_end(0, &mut 8, &mut sink));
    assert_eq!(track.src().as_ref(), "repeated.wav");
    assert_eq!(
        received.try_pop(),
        Some(DeckEvent::Failed {
            slot,
            at: SessionFrame::new(7),
            fault
        })
    );
    assert_ne!(observe(&mut track.resource), Some(ReadOutcome::Eof));
    assert!(
        fault.to_string().contains("source cancelled"),
        "the source cause must survive the failed-end handler: {fault}"
    );
    assert!(!track.poll_end(0, &mut 8, &mut sink));
    assert!(received.try_pop().is_none());
    assert!(!track.poll_end(0, &mut 8, &mut sink));
    assert!(received.try_pop().is_none());
}

#[kithara::test(native)]
fn decoder_node_distinguishes_failed_from_eof_on_the_wire() {
    let failure = TrackFailureKind::Decode {
        kind: DecodeErrorKind::InvalidData,
    };
    let (mut node, receiver, _lane) = terminal_node(FailedSource(failure), spec(), false);
    assert_eq!(node.tick(), TickResult::Progress);
    let mut reader = resource(receiver);
    assert_ne!(observe(&mut reader), Some(ReadOutcome::Eof));
    assert_eq!(
        observe(&mut reader),
        Some(ReadOutcome::Failed(FailureSource::Producer { failure }))
    );
}

#[kithara::test]
fn consumer_phase_terminal() {
    use super::super::TrackReadOutcome;
    assert_eq!(
        TrackReadOutcome::Full {
            position: 0.0,
            frames: 1,
            duration: 1.0,
            frames_until_eof: None
        }
        .ended_at(&(3..4)),
        None
    );
    let (receiver, _producer) = terminal_ring(false, spec());
    let mut reader = resource(receiver);
    assert_eq!(observe(&mut reader), None);
    reader.select_segment(SegmentId::FIRST.next());
    assert_eq!(observe(&mut reader), None);
    assert_eq!(TrackReadOutcome::Eof.ended_at(&(3..4)), Some(3));
    assert_eq!(
        TrackReadOutcome::Failed(PlaybackFault::Source(TrackFailureKind::SourceCancelled))
            .ended_at(&(3..4)),
        Some(3)
    );
}

struct SeekFailureSource;

impl AudioSource for SeekFailureSource {
    type Chunk = AudioChunk;
    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::StateChanged
    }
    fn seek(&mut self, _position: Duration) -> Result<SeekOutcome, AudioReadError> {
        Err(kithara_audio::DecodeError::Interrupted.into())
    }
}

#[kithara::test]
fn failed_seek_commits_its_epoch_for_the_terminal_marker() {
    use kithara_command::{Batch, When};
    use kithara_warp::SpeedCurve;
    let selected = SegmentId::FIRST.next().next().next();
    let (mut node, receiver, mut lane) = terminal_node(SeekFailureSource, spec(), false);
    lane.send(
        When::Next,
        Batch {
            basis: Vec::new(),
            commands: vec![crate::LaneCommand::Segment {
                id: selected,
                from: Duration::from_secs(2),
                speed: SpeedCurve::Constant(1.0),
            }],
        },
    )
    .expect("seek command reaches the lane");
    let mut reader = resource(receiver);
    reader.select_segment(selected);
    assert_eq!(node.tick(), TickResult::Progress);
    let failure = TrackFailureKind::Decode {
        kind: DecodeErrorKind::Interrupted,
    };
    assert!(
        matches!(reader.consumer.get().receiver.peek(), Some(PcmPacket::Failed { segment, failure: actual }) if *segment == selected && *actual == failure)
    );
    assert_eq!(
        observe(&mut reader),
        Some(ReadOutcome::Failed(FailureSource::ProducerAfterSeek {
            failure
        }))
    );
    assert!(!reader.eof);
}

#[kithara::test]
fn read_returns_failed_not_eof_on_decoder_error() {
    let failure = TrackFailureKind::Decode {
        kind: DecodeErrorKind::Io,
    };
    let (mut node, receiver, _lane) = terminal_node(FailedSource(failure), spec(), false);
    assert_eq!(node.tick(), TickResult::Progress);
    let mut reader = resource(receiver);
    let mut left = vec![0.0_f32; 4096];
    let mut right = vec![0.0_f32; 4096];
    let result = reader.read(&mut [&mut left, &mut right], 0..4096, &mut 8);
    match result {
        ReadOutcome::Failed(source) => assert_eq!(
            TrackFailureKind::from(source),
            TrackFailureKind::Decode {
                kind: DecodeErrorKind::Io
            },
            "the decoder own error kind must survive the read that returned it"
        ),
        ReadOutcome::Eof | ReadOutcome::Partial { .. } => {
            panic!("decoder Err must NOT be conflated with natural EOF: {result:?}")
        }
        ReadOutcome::Full { .. } => {
            panic!("decoder Err must surface as Failed, not Full silence: {result:?}")
        }
    }
    assert!(
        !reader.eof,
        "frames_until_eof must NOT report an EOF after a decode failure"
    );
}
