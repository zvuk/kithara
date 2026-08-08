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
    let mut backend = build_backend(kind, &options);
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
    assert!(
        backend.source_latency_frames() > 0,
        "{kind}: processed backend must report input latency"
    );
}

#[cfg(feature = "stretch-signalsmith")]
#[test]
fn builds_and_processes_signalsmith_backend() {
    smoke(StretchKind::Signalsmith);
}

#[cfg(feature = "stretch-signalsmith")]
#[test]
fn signalsmith_flush_releases_source_latency() {
    let options = options();
    let mut backend = build_backend(StretchKind::Signalsmith, &options);
    let mut out = Vec::new();
    backend
        .process(&interleaved_stereo(), &mut out)
        .expect("Signalsmith process must succeed");
    assert!(backend.source_latency_frames() > 0);

    backend
        .flush(&mut out)
        .expect("Signalsmith flush must succeed");

    assert_eq!(backend.source_latency_frames(), 0);
}

#[cfg(feature = "stretch-bungee")]
#[test]
fn builds_and_processes_bungee_backend() {
    smoke(StretchKind::Bungee);
}

#[cfg(feature = "stretch-bungee")]
#[test]
fn bungee_noop_flush_preserves_source_latency() {
    let options = options();
    let mut backend = build_backend(StretchKind::Bungee, &options);
    let mut out = Vec::new();
    backend
        .process(&interleaved_stereo(), &mut out)
        .expect("Bungee process must succeed");
    let held = backend.source_latency_frames();
    assert!(held > 0);

    backend.flush(&mut out).expect("Bungee flush must succeed");

    assert_eq!(backend.source_latency_frames(), held);
}
