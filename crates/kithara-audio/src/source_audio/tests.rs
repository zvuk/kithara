use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_platform::CancelToken;
use kithara_test_utils::kithara;

use super::{
    SourceAudioError, SourceAudioReadOutcome, SourceAudioReader, SourceAudioTap, SourceFrameRange,
    connect_source_audio, model::SourceAudioCaptureOutcome,
};
use crate::renderer::AudioWorkerHandle;

struct Fixture {
    reader: SourceAudioReader,
    tap: SourceAudioTap,
    pool: PcmPool,
    worker: AudioWorkerHandle,
}

impl Fixture {
    fn new(capacity: usize, max_frames: usize, spec: PcmSpec) -> Self {
        let pool = PcmPool::default();
        let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
        let (mut reader, mut tap) = connect_source_audio(
            &pool,
            NonZeroUsize::new(capacity).expect("non-zero test capacity"),
            NonZeroUsize::new(max_frames).expect("non-zero test frame limit"),
            worker.clone(),
        )
        .expect("source audio connection");
        reader.activate(spec).expect("source audio activation");
        tap.service().expect("source audio tap service");
        Self {
            reader,
            tap,
            pool,
            worker,
        }
    }

    fn chunk(&self, spec: PcmSpec, frame_offset: u64, samples: &[f32]) -> PcmChunk {
        let frames = samples.len() / usize::from(spec.channels);
        PcmChunk::new(
            PcmMeta {
                spec,
                frames: u32::try_from(frames).expect("small test chunk"),
                frame_offset,
                ..Default::default()
            },
            self.pool.attach(samples.to_vec()),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.worker.shutdown();
    }
}

fn mono() -> PcmSpec {
    PcmSpec::new(1, NonZeroU32::new(48_000).expect("static sample rate"))
}

fn stereo() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(48_000).expect("static sample rate"))
}

#[kithara::test]
fn source_frame_range_99_to_103_is_half_open_and_checked() {
    let range = SourceFrameRange::new(99, 103).expect("valid source range");
    assert_eq!(range.start(), 99);
    assert_eq!(range.end(), 103);
    assert_eq!(range.len(), 4);
    assert!(!range.is_empty());
    assert_eq!(
        SourceFrameRange::new(103, 99),
        Err(SourceAudioError::InvertedRange {
            start: 103,
            end: 99,
        })
    );
}

#[kithara::test]
fn demand_tokens_are_rejected_across_reader_lanes() {
    let mut owner = Fixture::new(2, 4, mono());
    let mut other = Fixture::new(2, 4, mono());
    let range = SourceFrameRange::new(99, 103).expect("valid source range");
    let foreign = other.reader.request(range, 0, 1).expect("foreign demand");
    let mut output = [0.0; 4];

    assert_eq!(
        owner.reader.read_into(&foreign, &mut output),
        Err(SourceAudioError::ForeignDemand)
    );
}

#[kithara::test]
#[cfg(not(target_arch = "wasm32"))]
fn source_activity_wakes_when_data_becomes_ready() {
    let mut fixture = Fixture::new(2, 4, mono());
    let activity = fixture
        .reader
        .take_activity()
        .expect("source activity ownership");
    assert!(fixture.reader.take_activity().is_none());
    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 1).expect("source demand");
    fixture.tap.service().expect("install demand");
    let snapshot = activity.snapshot();

    let chunk = fixture.chunk(mono(), 0, &[1.0, 2.0]);
    assert_eq!(
        fixture.tap.capture(&chunk, 1),
        Ok(SourceAudioCaptureOutcome::Captured)
    );
    activity.wait(snapshot);

    let mut output = [0.0; 2];
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );
    assert_eq!(output, [1.0, 2.0]);
}

#[kithara::test]
#[cfg(not(target_arch = "wasm32"))]
fn source_activity_wakes_when_terminal_status_arrives() {
    let mut fixture = Fixture::new(2, 4, mono());
    fixture
        .reader
        .activate_authoritative(mono())
        .expect("authoritative activation");
    fixture.tap.service().expect("install activation");
    let activity = fixture
        .reader
        .take_activity()
        .expect("source activity ownership");
    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 1).expect("source demand");
    fixture.tap.service().expect("install demand");
    let snapshot = activity.snapshot();

    assert!(fixture.tap.finish(1, super::SourceAudioTerminal::Eof));
    assert_ne!(activity.snapshot(), snapshot);
    activity.wait(snapshot);

    let mut output = [0.0; 2];
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Eof)
    );
}

#[kithara::test]
fn deactivation_stops_capture_and_keeps_cached_ranges_reusable() {
    let mut fixture = Fixture::new(2, 4, mono());
    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 1).expect("source demand");
    fixture.tap.service().expect("install demand");
    let cached = fixture.chunk(mono(), 0, &[1.0, 2.0]);
    fixture
        .tap
        .capture(&cached, 1)
        .expect("capture source range");
    let mut output = [0.0; 2];
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );

    fixture.reader.deactivate().expect("deactivate source lane");
    fixture.tap.service().expect("install deactivation");
    assert_eq!(
        fixture.reader.request(range, 0, 2),
        Err(SourceAudioError::Inactive)
    );
    assert_eq!(
        fixture.tap.capture(&cached, 1),
        Ok(SourceAudioCaptureOutcome::Ignored)
    );

    fixture
        .reader
        .activate(mono())
        .expect("reactivate source lane");
    fixture.tap.service().expect("install reactivation");
    let reused = fixture.reader.request(range, 0, 2).expect("reused demand");
    assert_eq!(
        fixture.reader.read_into(&reused, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );
    assert_eq!(output, [1.0, 2.0]);
}

#[kithara::test]
fn stale_seek_epoch_packets_are_retired_without_discarding_cached_audio() {
    let mut fixture = Fixture::new(2, 4, mono());
    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let stale = fixture.reader.request(range, 0, 4).expect("stale demand");
    fixture.tap.service().expect("install stale demand");
    let chunk = fixture.chunk(mono(), 0, &[1.0, 2.0]);
    assert_eq!(
        fixture.tap.capture(&chunk, 4),
        Ok(SourceAudioCaptureOutcome::Captured)
    );

    let current = fixture.reader.request(range, 0, 5).expect("current demand");
    fixture.tap.service().expect("install current demand");
    let mut output = [0.0; 2];
    assert_eq!(
        fixture.reader.read_into(&current, &mut output),
        Ok(SourceAudioReadOutcome::Pending)
    );
    assert_eq!(
        fixture.reader.read_into(&stale, &mut output),
        Err(SourceAudioError::StaleDemand)
    );
    assert_eq!(
        fixture.tap.capture(&chunk, 4),
        Ok(SourceAudioCaptureOutcome::Ignored)
    );
    assert_eq!(
        fixture.tap.capture(&chunk, 5),
        Ok(SourceAudioCaptureOutcome::Captured)
    );
    assert_eq!(
        fixture.reader.read_into(&current, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );
    assert_eq!(output, [1.0, 2.0]);
}

#[kithara::test]
fn overlapping_windows_keep_first_write_and_fill_the_uncovered_suffix() {
    let mut fixture = Fixture::new(3, 4, mono());
    let range = SourceFrameRange::new(0, 6).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 1).expect("source demand");
    fixture.tap.service().expect("install demand");
    let original = fixture.chunk(mono(), 0, &[1.0, 2.0, 3.0, 4.0]);
    let overlap = fixture.chunk(mono(), 2, &[8.0, 8.0, 8.0, 8.0]);
    fixture.tap.capture(&original, 1).expect("original capture");
    fixture.tap.capture(&overlap, 1).expect("overlap capture");
    let mut output = [0.0; 6];
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 6 })
    );
    assert_eq!(output, [1.0, 2.0, 3.0, 4.0, 8.0, 8.0]);
}

#[kithara::test]
fn poll_flushes_a_demand_parked_behind_activation() {
    let pool = PcmPool::default();
    let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
    let (mut reader, mut tap) = connect_source_audio(
        &pool,
        NonZeroUsize::new(1).expect("non-zero test capacity"),
        NonZeroUsize::new(2).expect("non-zero test frame limit"),
        worker.clone(),
    )
    .expect("source audio connection");
    reader.activate(mono()).expect("source audio activation");
    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let demand = reader.request(range, 0, 7).expect("source demand");

    reader.poll();
    tap.service().expect("install activation");
    reader.poll();
    tap.service().expect("install demand");
    let chunk = PcmChunk::new(
        PcmMeta {
            spec: mono(),
            frames: 2,
            ..Default::default()
        },
        pool.attach(vec![1.0, 2.0]),
    );
    assert_eq!(
        tap.capture(&chunk, 7),
        Ok(SourceAudioCaptureOutcome::Captured)
    );
    let mut output = [0.0; 2];
    assert_eq!(
        reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );
    worker.shutdown();
}

#[kithara::test]
fn capture_grows_its_prepared_window_in_the_worker_shell() {
    let mut fixture = Fixture::new(2, 2, mono());
    let range = SourceFrameRange::new(0, 4).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 3).expect("source demand");
    fixture.tap.service().expect("install demand");
    let chunk = fixture.chunk(mono(), 0, &[1.0, 2.0, 3.0, 4.0]);

    assert_eq!(
        fixture.tap.capture(&chunk, 3),
        Ok(SourceAudioCaptureOutcome::PreparationPending)
    );
    fixture.tap.service().expect("grow source buffers");
    assert_eq!(
        fixture.tap.capture(&chunk, 3),
        Ok(SourceAudioCaptureOutcome::Captured)
    );
    let mut output = [0.0; 4];
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 4 })
    );
}

#[kithara::test]
fn look_ahead_extends_coverage_without_changing_requested_range() {
    let mut fixture = Fixture::new(2, 4, mono());
    let requested = SourceFrameRange::new(10, 12).expect("requested range");
    let demand = fixture
        .reader
        .request(requested, 3, 2)
        .expect("look-ahead demand");

    assert_eq!(demand.requested, requested);
    assert_eq!(
        demand.coverage,
        SourceFrameRange::new(10, 15).expect("coverage range")
    );
    assert_eq!(demand.decode_seek_epoch(), 2);
    fixture.tap.service().expect("install demand");
    let look_ahead = fixture.chunk(mono(), 12, &[3.0, 4.0, 5.0]);
    assert_eq!(
        fixture.tap.capture(&look_ahead, 2),
        Ok(SourceAudioCaptureOutcome::Captured)
    );
}

#[kithara::test]
fn returned_source_buffers_release_data_backpressure() {
    let mut fixture = Fixture::new(1, 2, mono());
    let range = SourceFrameRange::new(0, 6).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 3).expect("source demand");
    fixture.tap.service().expect("install demand");
    let first = fixture.chunk(mono(), 0, &[1.0, 2.0]);
    let second = fixture.chunk(mono(), 2, &[3.0, 4.0]);
    let third = fixture.chunk(mono(), 4, &[5.0, 6.0]);
    fixture.tap.capture(&first, 3).expect("first capture");
    fixture.tap.capture(&second, 3).expect("overflow capture");
    assert!(!fixture.tap.can_step());
    assert_eq!(
        fixture.tap.capture(&third, 3),
        Err(SourceAudioError::DataBackpressure)
    );

    let mut output = [0.0; 6];
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Pending)
    );
    assert!(fixture.tap.can_step());
    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Pending)
    );
    let before_service = fixture.tap.available_buffers();
    fixture.tap.service().expect("return retired source buffer");
    assert!(fixture.tap.available_buffers() > before_service);
    assert_eq!(
        fixture.tap.capture(&third, 3),
        Ok(SourceAudioCaptureOutcome::Captured)
    );
}

#[kithara::test]
#[cfg(not(target_arch = "wasm32"))]
fn closed_capture_lane_fails_an_uncached_demand() {
    let pool = PcmPool::default();
    let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
    let (mut reader, mut tap) = connect_source_audio(
        &pool,
        NonZeroUsize::new(1).expect("non-zero test capacity"),
        NonZeroUsize::new(2).expect("non-zero test frame limit"),
        worker.clone(),
    )
    .expect("source audio connection");
    let activity = reader.take_activity().expect("source activity ownership");
    reader.activate(mono()).expect("source audio activation");
    tap.service().expect("install activation");
    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let demand = reader.request(range, 0, 1).expect("source demand");
    tap.service().expect("install demand");
    let snapshot = activity.snapshot();
    drop(tap);
    assert_ne!(activity.snapshot(), snapshot);
    activity.wait(snapshot);

    let mut output = [0.0; 2];
    assert_eq!(
        reader.read_into(&demand, &mut output),
        Err(SourceAudioError::SourceFailed)
    );
    worker.shutdown();
}

#[kithara::test]
fn multiwindow_forward_read_is_bit_exact() {
    let mut fixture = Fixture::new(4, 2, stereo());
    let range = SourceFrameRange::new(99, 103).expect("valid source range");
    let demand = fixture.reader.request(range, 0, 9).expect("source demand");
    fixture.tap.service().expect("install demand");
    let first = fixture.chunk(stereo(), 99, &[0.25, -0.25, 0.5, -0.5]);
    let second = fixture.chunk(stereo(), 101, &[0.75, -0.75, 1.0, -1.0]);
    fixture.tap.capture(&first, 9).expect("first capture");
    fixture.tap.capture(&second, 9).expect("second capture");
    let mut output = [0.0; 8];

    assert_eq!(
        fixture.reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Ready { frames: 4 })
    );
    assert_eq!(output, [0.25, -0.25, 0.5, -0.5, 0.75, -0.75, 1.0, -1.0]);
}

#[kithara::test]
fn authoritative_capture_waits_for_demand_and_stops_before_the_next_window() {
    let pool = PcmPool::default();
    let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
    let (mut reader, mut tap) = connect_source_audio(
        &pool,
        NonZeroUsize::new(2).expect("non-zero test capacity"),
        NonZeroUsize::new(2).expect("non-zero test frame limit"),
        worker.clone(),
    )
    .expect("source audio connection");
    reader
        .activate_authoritative(mono())
        .expect("authoritative activation");
    tap.service().expect("install activation");
    assert!(!tap.can_step());

    let range = SourceFrameRange::new(0, 2).expect("valid source range");
    let demand = reader.request(range, 0, 11).expect("source demand");
    tap.service().expect("install demand");
    assert!(tap.can_step());
    let next = PcmChunk::new(
        PcmMeta {
            spec: mono(),
            frames: 2,
            frame_offset: 2,
            ..Default::default()
        },
        pool.attach(vec![3.0, 4.0]),
    );
    assert_eq!(
        tap.capture(&next, 11),
        Ok(SourceAudioCaptureOutcome::DemandComplete)
    );
    assert!(!tap.can_step());

    assert!(tap.finish(11, super::SourceAudioTerminal::Eof));
    let mut output = [0.0; 2];
    assert_eq!(
        reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Eof)
    );
    worker.shutdown();
}
