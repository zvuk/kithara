use std::num::NonZeroU32;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_test_utils::kithara;

use super::{
    super::output::{OutputBuffers, RecycleOutcome},
    assert_no_alloc,
};

fn recycle_spec(channels: u16, sample_rate: u32) -> PcmSpec {
    PcmSpec::new(
        channels,
        NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
    )
}

fn prepared_buffers(spec: PcmSpec) -> OutputBuffers {
    let mut buffers = OutputBuffers::new(PcmPool::default());
    assert!(
        buffers
            .prepare(spec)
            .expect("presentation reserve must prepare")
    );
    buffers
}

fn rejected_chunk(spec: PcmSpec, samples: usize) -> PcmChunk {
    let pool = PcmPool::new(1, 0);
    pool.pre_warm(1, |buffer| buffer.resize(samples.max(1), 0.0));
    PcmChunk::new(
        PcmMeta {
            spec,
            frames: 1,
            ..Default::default()
        },
        pool.attach(vec![0.0; samples]),
    )
}

#[kithara::test]
fn full_recycle_failure_retains_pcm_without_allocator_calls() {
    let spec = recycle_spec(1, 48_000);
    let mut buffers = prepared_buffers(spec);
    let chunk = rejected_chunk(spec, super::PRESENTATION_FRAMES);

    let result = assert_no_alloc(|| buffers.recycle(chunk));

    assert!(matches!(result, RecycleOutcome::Rejected(_)));
    drop(result);
}

#[kithara::test]
fn stale_recycle_failure_retains_pcm_without_allocator_calls() {
    let spec = recycle_spec(1, 48_000);
    let mut buffers = prepared_buffers(spec);
    let held = buffers
        .take(spec)
        .expect("prepared reserve must be valid")
        .expect("prepared reserve must have one buffer");
    let stale = rejected_chunk(recycle_spec(2, 44_100), super::PRESENTATION_FRAMES * 2);

    let result = assert_no_alloc(|| buffers.recycle(stale));

    assert!(matches!(result, RecycleOutcome::Rejected(_)));
    drop(result);
    drop(held);
}

#[kithara::test]
fn undersized_recycle_failure_retains_pcm_without_allocator_calls() {
    let spec = recycle_spec(1, 48_000);
    let mut buffers = prepared_buffers(spec);
    let held = buffers
        .take(spec)
        .expect("prepared reserve must be valid")
        .expect("prepared reserve must have one buffer");
    let short = rejected_chunk(spec, 1);

    let result = assert_no_alloc(|| buffers.recycle(short));

    assert!(matches!(result, RecycleOutcome::Rejected(_)));
    drop(result);
    drop(held);
}
