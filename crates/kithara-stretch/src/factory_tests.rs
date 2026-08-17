#[cfg(feature = "stretch-bungee")]
use kithara_bufpool::ByteBudget;
use kithara_bufpool::PcmPool;

use super::build_backend;
use crate::{StretchKind, StretchOptions};

const CHANNELS: usize = 2;
const INPUT_FRAMES: usize = 4096;

fn options() -> StretchOptions {
    StretchOptions::builder()
        .sample_rate(44_100)
        .channels(CHANNELS)
        .max_input_frames(INPUT_FRAMES)
        .pool(PcmPool::default())
        .build()
}

fn interleaved_stereo() -> Vec<f32> {
    let mut input = Vec::with_capacity(INPUT_FRAMES * CHANNELS);
    for frame in 0..INPUT_FRAMES {
        let left = if frame % 2 == 0 { 0.25 } else { -0.25 };
        input.push(left);
        input.push(-left);
    }
    input
}

fn smoke(kind: StretchKind) {
    let options = options();
    let mut backend = build_backend(kind, &options).expect("backend construction must succeed");
    if let Err(error) = backend.set_ratio(1.0) {
        panic!("{kind}: set_ratio(1.0) failed: {error}");
    }

    let max_output_samples = backend.max_output_samples(INPUT_FRAMES);
    let input = interleaved_stereo();
    let mut out = Vec::with_capacity(max_output_samples);
    if let Err(error) = backend.process(&input, &mut out) {
        panic!("{kind}: process failed: {error}");
    }

    assert!(!out.is_empty(), "{kind}: process emitted no samples");
    assert_eq!(
        out.len() % CHANNELS,
        0,
        "{kind}: output must stay channel-aligned"
    );
    assert!(
        out.len() <= max_output_samples,
        "{kind}: emitted {} samples above max_output_samples {}",
        out.len(),
        max_output_samples
    );
}

#[cfg(feature = "stretch-signalsmith")]
#[test]
fn builds_and_processes_signalsmith_backend() {
    smoke(StretchKind::Signalsmith);
}

#[cfg(feature = "stretch-bungee")]
#[test]
fn builds_and_processes_bungee_backend() {
    smoke(StretchKind::Bungee);
}

#[cfg(feature = "stretch-bungee")]
#[test]
fn bungee_construction_fails_closed_when_scratch_budget_is_exhausted() {
    let options = StretchOptions::builder()
        .sample_rate(44_100)
        .channels(CHANNELS)
        .max_input_frames(INPUT_FRAMES)
        .pool(PcmPool::with_byte_budget(0, 0, ByteBudget(0)))
        .build();

    let Err(error) = build_backend(StretchKind::Bungee, &options) else {
        panic!("Bungee cannot install a backend without its fixed scratch");
    };
    assert!(matches!(error, crate::StretchBackendError::Construction(_)));
}
