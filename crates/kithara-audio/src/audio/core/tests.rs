use std::{collections::VecDeque, num::NonZeroU32};

use kithara_decode::TrackMetadata;
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{CancelScope, sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{ActivityWriter, PlayheadState};
use kithara_test_fixtures::unit_fixtures::cursor_half;
use kithara_test_utils::kithara;

use super::{audio::*, context::AudioContext};
use crate::{
    AudioSource, Fetch, ReadOutcome, SeekOutcome, SourceSpan, TrackStep, WaitingReason,
    test_pools::{Pools, pools, sample_buffer},
};

struct StagedSource {
    chunks: VecDeque<AudioChunk>,
    spec: AudioSpec,
}
impl AudioSource for StagedSource {
    type Chunk = AudioChunk;
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, crate::AudioReadError> {
        self.chunks.clear();
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }
    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        self.spec.sample_rate = rate;
    }
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        Some(self.spec.sample_rate)
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.chunks
            .pop_front()
            .map_or(TrackStep::Blocked(WaitingReason::Waiting), |chunk| {
                TrackStep::Produced(Fetch::data(chunk))
            })
    }
}
fn fixture(spec: AudioSpec, chunks: Vec<AudioChunk>) -> Audio<()> {
    let activity = ActivityWriter::new();
    Audio::new(
        Box::new(StagedSource {
            chunks: chunks.into(),
            spec,
        }),
        AudioContext {
            playhead: Arc::new(PlayheadState::new()),
            emit: Arc::new(DeferredBus::new(
                EventBus::default(),
                crate::consts::AUDIO_EVENT_CAPACITY,
            )),
            metadata: TrackMetadata::default(),
            abr: None,
            activity: activity.reader(),
            activity_writer: Some(activity),
            cancel: CancelScope::new(None).token(),
        },
        spec,
    )
}
fn timed_chunk(
    pools: &Pools,
    pcm: &[f32],
    spec: AudioSpec,
    frames: u32,
    start: Duration,
    end: Duration,
) -> AudioChunk {
    let channels = usize::from(spec.channels.max(1));
    let frame_count = usize::try_from(frames).expect("test frame count fits usize");
    let samples = &pcm[..frame_count * channels];
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            timestamp: start,
            end_timestamp: end,
            frames,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}
#[kithara::test]
fn reads_preserve_consecutive_rendered_source_spans_and_revisions(cursor_half: Vec<f32>) {
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("test rate");
    let spec = AudioSpec::new(1, rate);
    let mut first = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        3,
        Duration::ZERO,
        Duration::from_millis(3),
    );
    first.meta.frame_offset = 100;
    first.meta.render_revision = 7;
    let mut second = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        2,
        Duration::from_millis(3),
        Duration::from_millis(5),
    );
    second.meta.frame_offset = 1_000;
    second.meta.render_revision = 7;
    let mut changed = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        2,
        Duration::from_millis(5),
        Duration::from_millis(7),
    );
    changed.meta.frame_offset = 2_000;
    changed.meta.render_revision = 8;
    first.meta.source_span =
        SourceSpan::new(100, 106, rate, 3).map(|span| span.with_render_revision(7));
    second.meta.source_span =
        SourceSpan::new(106, 110, rate, 2).map(|span| span.with_render_revision(7));
    changed.meta.source_span =
        SourceSpan::new(110, 114, rate, 2).map(|span| span.with_render_revision(8));
    let mut audio = fixture(spec, vec![first, second, changed]);
    audio.preload().expect("prime first chunk");
    let first_output_meta = audio.current_chunk.as_ref().map(|chunk| chunk.meta);
    let mut output = [0.0; 8];
    let first_read = audio.read(&mut output).expect("first read succeeds");
    assert_eq!(
        first_output_meta.map(|meta| meta.timestamp),
        Some(Duration::ZERO)
    );
    let ReadOutcome::Frames {
        count, source_span, ..
    } = first_read
    else {
        panic!("expected first rendered frames");
    };
    assert_eq!(count.get(), 5);
    assert_eq!(
        source_span,
        SourceSpan::new(100, 110, rate, 5).map(|span| span.with_render_revision(7))
    );

    let second_read = audio
        .read(&mut output[..1])
        .expect("partial changed-revision read succeeds");
    let ReadOutcome::Frames { source_span, .. } = second_read else {
        panic!("expected partial changed-revision frames");
    };
    assert_eq!(
        source_span,
        SourceSpan::new(110, 112, rate, 1).map(|span| span.with_render_revision(8))
    );

    let final_read = audio
        .read(&mut output)
        .expect("final changed-revision read succeeds");
    let ReadOutcome::Frames { source_span, .. } = final_read else {
        panic!("expected final changed-revision frames");
    };
    assert_eq!(
        source_span,
        SourceSpan::new(112, 114, rate, 1).map(|span| span.with_render_revision(8))
    );
}
#[kithara::test]
fn planar_read_crosses_chunks_but_preserves_mapping_boundaries() {
    let config = kithara_bufpool::PoolConfig::builder()
        .max_buffers(32)
        .max_retained_capacity(1)
        .build();
    let pools = crate::test_pools::pools_with(1024 * 1024, config, config);
    let rate = NonZeroU32::new(48_000).expect("test rate");
    let spec = AudioSpec::new(2, rate);
    let mut chunks = Vec::new();
    let first_map = std::num::NonZeroU64::new(1);
    let next_map = std::num::NonZeroU64::new(2);
    for (offset, pcm, mapping) in [
        (0, [0.0, 10.0, 1.0, 11.0], first_map),
        (2, [2.0, 12.0, 3.0, 13.0], first_map),
        (4, [4.0, 14.0, 5.0, 15.0], next_map),
    ] {
        let mut chunk = timed_chunk(
            &pools,
            &pcm,
            spec,
            2,
            Duration::ZERO,
            Duration::from_millis(1),
        );
        chunk.meta.frame_offset = offset;
        chunk.meta.mapping_revision = mapping;
        chunk.meta.source_span = SourceSpan::new(offset, offset + 2, rate, 2)
            .map(|span| span.with_mapping_revision(mapping));
        chunks.push(chunk);
    }
    let first_bytes = chunks[0].samples.capacity() * size_of::<f32>();
    let second_bytes = chunks[1].samples.capacity() * size_of::<f32>();
    let mut audio = fixture(spec, chunks);
    let allocated_before = pools.stats().allocated_bytes;
    let mut left = [-1.0; 5];
    let mut right = [-1.0; 5];
    let read = audio
        .read_planar(&mut [&mut left, &mut right])
        .expect("planar read succeeds");
    let ReadOutcome::Frames {
        count, source_span, ..
    } = read
    else {
        panic!("expected planar frames");
    };
    assert_eq!(count.get(), 4);
    assert_eq!(left, [0.0, 1.0, 2.0, 3.0, -1.0]);
    assert_eq!(right, [10.0, 11.0, 12.0, 13.0, -1.0]);
    assert_eq!(
        source_span,
        SourceSpan::new(0, 4, rate, 4).map(|span| span.with_mapping_revision(first_map))
    );
    assert!(pools.stats().allocated_bytes <= allocated_before - first_bytes);
    assert!(pools.stats().allocated_bytes <= allocated_before - first_bytes - second_bytes);
    assert_eq!(
        pools.stats().allocated_bytes,
        allocated_before - first_bytes - second_bytes
    );
    assert_eq!(
        audio
            .current_chunk
            .as_ref()
            .expect("next map remains resident")
            .meta
            .mapping_revision,
        next_map
    );
    assert_eq!(audio.cursor.consumed_frames(), 0);
}
#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(hang_timeout_secs(1))]
fn preload_returns_when_the_producer_has_delivered_nothing() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
    let mut audio = fixture(spec, Vec::new());
    audio
        .preload()
        .expect("preload primes whatever the producer delivered");
    assert!(audio.is_preloaded());
}

#[kithara::test]
fn partial_reads_keep_resumed_source_frames_and_rational_mapping(cursor_half: Vec<f32>) {
    let pools = pools();
    let output_rate = NonZeroU32::new(48_000).expect("output rate");
    let source_rate = NonZeroU32::new(44_100).expect("source rate");
    let spec = AudioSpec::new(2, output_rate);
    let span = SourceSpan::try_from((
        128 * u128::from(output_rate.get()),
        u128::from(source_rate.get()),
        output_rate.into(),
        source_rate,
        5,
    ))
    .expect("exact resumed mapping")
    .with_mapping_revision(std::num::NonZeroU64::new(4))
    .with_render_revision(7);
    let mut chunk = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        5,
        span.position_at(0).expect("mapped start"),
        span.position_at(5).expect("mapped end"),
    );
    chunk.meta.frame_offset = 139;
    chunk.meta.source_span = Some(span);
    chunk.meta.mapping_revision = span.mapping_revision();
    chunk.meta.render_revision = span.render_revision();
    let mut audio = fixture(spec, vec![chunk]);
    let read = audio.read(&mut [0.0; 4]).expect("partial mapped read");
    let ReadOutcome::Frames {
        count, source_span, ..
    } = read
    else {
        panic!("expected partial mapped frames");
    };
    assert_eq!(count.get(), 4);
    assert_eq!(source_span, span.for_output_range(0..2));
    let current = audio.current_chunk.as_ref().expect("partially read chunk");
    assert_eq!(
        source_end(&current.meta, audio.cursor.consumed_frames())
            .expect("rendered source boundary"),
        crate::SourceEnd::new(129, source_rate).with_mapping_revision(span.mapping_revision()),
    );
    let crate::ChunkOutcome::Chunk(remaining) = audio.next_chunk().expect("remaining mapped PCM")
    else {
        panic!("expected remaining mapped chunk");
    };
    assert_eq!(remaining.meta.frames, 3);
    assert_eq!(remaining.meta.source_span, span.for_output_range(2..5));
    assert_eq!(Some(remaining.meta.timestamp), span.position_at(2));
    assert_eq!(Some(remaining.meta.end_timestamp), span.position_at(5));
}
