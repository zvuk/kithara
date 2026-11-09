use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara_bufpool::HasPool;
use kithara_signal::FrameCount;
use kithara_stretch::{
    ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticEngine, ElasticError, ElasticRequest,
};
use kithara_sync::SpeedCurve;
use num_traits::ToPrimitive;

use super::*;
use crate::{
    Warp, WarpRenderError,
    test_pools::{pools, sample_buffer},
};

fn prepared_renderer(backend: StretchKind) -> WarpRenderer<crate::test_pools::TestPools> {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("fixture rate is non-zero"),
    );
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(true)
        .backend(backend)
        .build();
    Warp::new((), &config).renderer(spec, pools())
}

fn active_renderer(backend: StretchKind) -> WarpRenderer<crate::test_pools::TestPools> {
    let mut renderer = prepared_renderer(backend);
    let frames = renderer.source_block_frames.get().min(4096);
    let samples = vec![0.25; frames * usize::from(renderer.spec.channels)];
    let input = AudioChunk::new(
        AudioChunkInfo {
            spec: renderer.spec,
            frames: u32::try_from(frames).expect("fixture frames fit"),
            ..AudioChunkInfo::default()
        },
        sample_buffer(&renderer.pools, &samples),
    );
    drop(
        renderer
            .render(input)
            .continue_value()
            .expect("fixture fits one source span"),
    );
    renderer.prepare(renderer.spec);
    assert!(renderer.active);
    renderer
}

struct TrackedEngine {
    inner: Box<dyn ElasticEngine>,
    drops: Arc<AtomicUsize>,
}

impl Drop for TrackedEngine {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl ElasticEngine for TrackedEngine {
    fn prepare<S: HasPool<f32>>(_: ElasticConfig<S>) -> Result<Self, ElasticError> {
        Err(ElasticError::EnginePreparation(
            "tracking wraps a prepared engine",
        ))
    }

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> ElasticCapabilities;
            fn flush(&mut self, output: &mut [f32]) -> Result<ElasticDrain, ElasticError>;
            fn prime(&mut self, request: ElasticRequest, source_history: &[f32], source_lookahead: &[f32], source: &[f32], discarded_output: &mut [f32]) -> Result<(), ElasticError>;
            fn process(&mut self, request: ElasticRequest, source: &[f32], output: &mut [f32]) -> Result<(), ElasticError>;
            fn reset(&mut self) -> Result<(), ElasticError>;
            fn set_pitch(&mut self, scale: f64) -> Result<(), ElasticError>;
        }
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn tempo_retargets_keep_prepared_engines_and_the_source_frontier(#[case] backend: StretchKind) {
    let mut renderer = prepared_renderer(backend);
    let mut source = 0;
    let drops = Arc::new(AtomicUsize::new(0));
    renderer.engine = renderer.engine.take().map(|inner| {
        Box::new(TrackedEngine {
            inner,
            drops: drops.clone(),
        }) as Box<dyn ElasticEngine>
    });
    if let Some(mut projection) = renderer.projection.take() {
        projection.engines = projection.engines.map(|inner| {
            Box::new(TrackedEngine {
                inner,
                drops: drops.clone(),
            }) as Box<dyn ElasticEngine>
        });
        renderer.projection = Some(projection);
    }
    for (revision, speed) in [0.5, 0.75, 0.05, 0.9, 0.05, 0.6].into_iter().enumerate() {
        if revision > 0 {
            assert!(renderer.active && renderer.mapped_render);
            let frontier = renderer.rendered_source_end();
            renderer
                .set_speed(
                    SpeedCurve::Constant(speed),
                    u64::try_from(revision).expect("revision"),
                )
                .expect("retarget");
            renderer
                .prepare_engine_latency(renderer.spec)
                .expect("retarget service");
            assert_eq!(renderer.rendered_source_end(), frontier);
            assert_eq!(
                drops.load(Ordering::Relaxed),
                0,
                "tempo changes must retain prepared DSP engines"
            );
        }
        renderer
            .prepare_engine_latency(renderer.spec)
            .expect("service");
        let meta = AudioChunkInfo {
            spec: renderer.spec,
            frame_offset: source,
            ..Default::default()
        };
        let frames = renderer
            .prepare_source_window(meta, 65_536, 128)
            .expect("mapped source window")
            .get();
        let samples = vec![0.25; frames * usize::from(renderer.spec.channels)];
        let output = renderer
            .render_quantum(AudioChunk::new(
                AudioChunkInfo {
                    frames: u32::try_from(frames).expect("frame count"),
                    ..meta
                },
                sample_buffer(&renderer.pools, &samples),
            ))
            .continue_value()
            .expect("prepared quantum")
            .expect("audible output");
        assert_eq!(output.frames(), 128);
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
        source += u64::try_from(frames).expect("source frames");
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn prepared_engine_latency_is_visible_before_activation(#[case] backend: StretchKind) {
    let renderer = prepared_renderer(backend);
    let expected = renderer
        .engine
        .as_ref()
        .expect("fixture engine is prepared")
        .capabilities()
        .latency()
        .second();

    assert!(!renderer.active);
    assert!(renderer.rendered_source_end.is_none());
    assert!(expected > 0);
    assert_eq!(renderer.engine_latency(), FrameCount::new(expected));
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn latency_receipt_retires_engine_without_advancing_output(#[case] backend: StretchKind) {
    let mut renderer = active_renderer(backend);
    let frontier = renderer.rendered_source_end();
    let source_meta = renderer.last_input_meta;
    renderer.set_keylock(false);

    assert!(renderer.transition_pending());
    let latency = renderer
        .prepare_engine_latency(renderer.spec)
        .expect("outgoing tail retires into the crossfade");

    assert_eq!(latency, FrameCount::new(0));
    assert!(!renderer.transition_pending());
    assert_eq!(renderer.rendered_source_end(), frontier);
    assert_eq!(renderer.last_input_meta, source_meta);
    assert!(
        !renderer
            .residency
            .as_ref()
            .expect("crossfade residency is retained")
            .replacement
            .is_empty()
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn latency_receipt_waits_for_queued_unity(#[case] backend: StretchKind) {
    let mut renderer = active_renderer(backend);
    renderer.pending_unity_meta = Some(AudioChunkInfo::default());
    renderer.set_keylock(false);

    assert!(matches!(
        renderer.prepare_engine_latency(renderer.spec),
        Err(WarpRenderError::NeedsService)
    ));
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn latency_receipt_uses_the_prepared_replacement(#[case] backend: StretchKind) {
    let mut renderer = prepared_renderer(backend);
    assert!(renderer.engine_latency().get() > 0);
    renderer.set_keylock(false);

    let latency = renderer
        .prepare_engine_latency(renderer.spec)
        .expect("inactive backend replacement is prepared");

    assert!(!renderer.active);
    assert!(!renderer.transition_pending());
    assert_eq!(latency, FrameCount::new(0));
    assert_eq!(renderer.engine_latency(), latency);
}

#[cfg(feature = "stretch-identity")]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn backend_latency_preparation_preserves_crossfade_into_identity(#[case] backend: StretchKind) {
    for identical in [false, true] {
        let mut renderer = prepared_renderer(backend);
        let source_frames = renderer.source_block_frames.get().min(4096);
        let source: Vec<f32> = (0..source_frames)
            .flat_map(|frame| {
                let sample = (frame.to_f32().expect("fixture frame fits") * 0.137).sin() * 0.3;
                [sample; 2]
            })
            .collect();
        let input = AudioChunk::new(
            AudioChunkInfo {
                spec: renderer.spec,
                frames: u32::try_from(source_frames).expect("fixture frame count"),
                ..AudioChunkInfo::default()
            },
            sample_buffer(&renderer.pools, &source),
        );
        drop(
            renderer
                .render(input)
                .continue_value()
                .expect("fixture source span"),
        );
        renderer.prepare(renderer.spec);
        assert!(renderer.active);
        let frontier = renderer.rendered_source_end();
        renderer.set_backend(StretchKind::Identity);
        assert_eq!(
            renderer
                .prepare_engine_latency(renderer.spec)
                .expect("Identity is prepared at the switch moment"),
            FrameCount::new(0)
        );
        assert_eq!(renderer.rendered_source_end(), frontier);
        assert!(!renderer.transition_pending());
        assert!(!renderer.requires_staging());
        let replacement = renderer
            .residency
            .as_ref()
            .expect("Identity retains the outgoing crossfade")
            .replacement
            .to_vec();
        assert!(!replacement.is_empty());
        assert!(
            replacement
                .windows(2)
                .any(|pair| pair[0].to_bits() != pair[1].to_bits()),
            "the retained engine tail must vary"
        );
        let channels = usize::from(renderer.spec.channels);
        let total_frames = replacement.len() / channels;
        let frames = total_frames + 64;
        let mut samples = vec![-0.25; frames * channels];
        if identical {
            samples[..replacement.len()].copy_from_slice(&replacement);
        }
        let input = AudioChunk::new(
            AudioChunkInfo {
                spec: renderer.spec,
                frame_offset: 4096,
                frames: u32::try_from(frames).expect("fixture frames fit"),
                ..AudioChunkInfo::default()
            },
            sample_buffer(&renderer.pools, &samples),
        );
        let output = renderer
            .render(input)
            .continue_value()
            .expect("Identity accepts one source span")
            .expect("Identity emits the crossfaded source");
        assert_eq!(output.frames(), frames);
        for (index, (&got, &input)) in output.samples.iter().zip(&samples).enumerate() {
            if identical || index >= replacement.len() {
                assert_eq!(
                    got.to_bits(),
                    input.to_bits(),
                    "identity={identical}, sample={index}"
                );
            } else {
                let gain = (index / channels + 1).to_f32().expect("frame fits")
                    / total_frames.to_f32().expect("join fits");
                let old = replacement[index] * (1.0 - gain);
                let new = input * gain;
                let expected = replacement[index].mul_add(1.0 - gain, new);
                let larger = old.abs().max(new.abs());
                let ulp = larger.next_up() - larger;
                assert!(
                    (f64::from(got) - f64::from(expected)).abs() <= f64::from(ulp),
                    "linear law: sample={index}, got={got}, expected={expected}"
                );
            }
        }
    }
}
