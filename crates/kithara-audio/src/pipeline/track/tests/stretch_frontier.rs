use std::{num::NonZeroU32, sync::atomic::Ordering};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmMeta, PcmSpec, duration_for_frames};
use kithara_stretch::{StretchKind, StretchOptions, build_backend};
use kithara_test_utils::kithara;

use super::rebuild::{
    Consts, RouteFixture, RouteParams, next_test_chunk, route_source_with_effects,
};
use crate::{
    pipeline::{
        rebuild::RecreateNext,
        track::fsm::{CurrentFsm, TrackStep},
    },
    renderer::AudioWorkerSource,
    tempo::streaming::{StretchControls, TimeStretchProcessor},
    traits::AudioEffect,
};

fn signalsmith_effect(speed: f32) -> (TimeStretchProcessor, u64) {
    let pool = PcmPool::default();
    let spec = PcmSpec::new(
        Consts::CHANNELS,
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("test sample rate is non-zero"),
    );
    let options = StretchOptions::builder()
        .sample_rate(Consts::SAMPLE_RATE)
        .channels(usize::from(Consts::CHANNELS))
        .pool(pool.clone())
        .build();
    let latency =
        u64::try_from(build_backend(StretchKind::Signalsmith, &options).source_latency_frames())
            .unwrap_or(u64::MAX);
    let controls = StretchControls::new(speed);
    controls.set_backend(StretchKind::Signalsmith);
    controls.set_keylock(true);
    (TimeStretchProcessor::new(controls, spec, pool), latency)
}

#[kithara::test(tokio)]
async fn route_change_at_non_unity_uses_the_emitted_source_endpoint() {
    let (effect, source_latency) = signalsmith_effect(0.5);
    assert!(source_latency > 0, "Signalsmith must report input latency");
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_source_with_effects(
        RouteParams {
            initial_host_rate: Consts::SAMPLE_RATE,
            segmented: false,
        },
        vec![Box::new(effect)],
    )
    .await;
    let mut route_recreated = false;
    let mut last = None;

    for _ in 0..20 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        assert_eq!(chunk.meta.spec.sample_rate.get(), Consts::SAMPLE_RATE);
        last = Some(chunk);
    }

    let last = last.expect("non-unity stretch must produce PCM");
    let admitted = last
        .meta
        .frame_offset
        .saturating_add(u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX));
    assert_eq!(
        last.meta.end_timestamp,
        duration_for_frames(Consts::SAMPLE_RATE, admitted)
    );
    let expected = admitted.saturating_sub(source_latency.min(admitted));
    let epoch = source.seek_engine.epoch();
    assert_eq!(
        source.resume.decode_head(epoch),
        Some((expected, Consts::SAMPLE_RATE)),
        "the produced frontier must subtract source still held by Signalsmith"
    );
    assert_ne!(
        last.meta
            .frame_offset
            .saturating_add(u64::from(last.meta.frames)),
        expected,
        "the fixture must distinguish transformed output frames from the source endpoint"
    );

    host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    let CurrentFsm::RecreatingDecoder(handle) = &source.state else {
        panic!("expected route-change recreate");
    };
    let RecreateNext::ApplySeek(request) = &handle.data().next else {
        panic!("route change must apply a seek");
    };
    assert_eq!(
        request.seek.target,
        duration_for_frames(Consts::SAMPLE_RATE, expected),
        "route recreation must seek to the exact emitted source frame"
    );
}

async fn capture_unity_route(effects: Vec<Box<dyn AudioEffect>>) -> Vec<(PcmMeta, Vec<u32>)> {
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_source_with_effects(
        RouteParams {
            initial_host_rate: Consts::SAMPLE_RATE,
            segmented: false,
        },
        effects,
    )
    .await;
    let mut route_recreated = false;
    let mut chunks = Vec::new();
    for _ in 0..8 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        chunks.push((
            chunk.meta,
            chunk
                .samples
                .iter()
                .map(|sample| sample.to_bits())
                .collect(),
        ));
    }
    host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);
    for _ in 0..8 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        chunks.push((
            chunk.meta,
            chunk
                .samples
                .iter()
                .map(|sample| sample.to_bits())
                .collect(),
        ));
    }
    assert!(route_recreated, "fixture must exercise route recreation");
    chunks
}

#[kithara::test(tokio)]
async fn unity_stretch_route_is_bit_identical_to_the_effect_free_path() {
    let baseline = capture_unity_route(Vec::new()).await;
    let (effect, _) = signalsmith_effect(1.0);
    let unity = capture_unity_route(vec![Box::new(effect)]).await;

    assert_eq!(unity, baseline);
}
