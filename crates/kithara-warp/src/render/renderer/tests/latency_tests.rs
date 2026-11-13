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
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::*;
use crate::{
    Warp, WarpRenderError,
    render::tests::fixtures::LaneFeed,
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
    let frames = 16_384;
    let samples = vec![0.25; frames * usize::from(renderer.spec.channels)];
    let input = AudioChunk::new(
        AudioChunkInfo {
            spec: renderer.spec,
            frames: u32::try_from(frames).expect("fixture frames fit"),
            ..AudioChunkInfo::default()
        },
        sample_buffer(&renderer.pools, &samples),
    );
    assert!(!LaneFeed::default().feed(&mut renderer, input).is_empty());
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

/// Switching keylock off at unity retires the engine into a crossfade that
/// fades its retained tail into the passthrough source by the linear law and
/// then passes the source through bit-identical.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
#[kithara::hang_watchdog]
fn keylock_off_at_unity_crossfades_the_retained_tail_by_the_linear_law(
    #[case] backend: StretchKind,
) {
    let mut renderer = active_renderer(backend);
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity speed");
    renderer.set_keylock(false);
    assert_eq!(
        renderer
            .prepare_engine_latency(renderer.spec)
            .expect("the engine retires at the switch moment"),
        FrameCount::new(0)
    );
    assert!(!renderer.transition_pending());
    let resident = renderer
        .residency
        .as_ref()
        .expect("the retired engine leaves its tail");
    let decoded_end = resident.end.expect("the engine consumed decoded source");
    let replacement = resident.replacement.to_vec();
    assert!(
        replacement
            .windows(2)
            .any(|pair| pair[0].to_bits() != pair[1].to_bits()),
        "the retained engine tail must vary"
    );
    let channels = usize::from(renderer.spec.channels);
    let fade_frames = replacement.len() / channels;
    // The fixture fed a constant 0.25 before `decoded_end`; a varying signal
    // follows so a misplaced frame cannot pass.
    let source = |frame: u64| {
        if frame < decoded_end {
            0.25
        } else {
            (frame.to_f32().expect("fixture frame fits") * 0.137).sin() * 0.3
        }
    };
    let mut output = Vec::new();
    let mut first_source = None;
    let mut fed = decoded_end;
    while output.len() < (fade_frames + 64) * channels {
        hang_tick!();
        let meta = AudioChunkInfo {
            spec: renderer.spec,
            frame_offset: fed,
            ..AudioChunkInfo::default()
        };
        let window = renderer
            .prepare_source_window(meta, 4096, usize::MAX)
            .expect("decoded source continues into the unity window")
            .get();
        let samples: Vec<f32> = (fed..fed + u64::try_from(window).expect("window fits u64"))
            .flat_map(|frame| [source(frame); 2])
            .collect();
        let quantum = renderer
            .render_quantum(AudioChunk::new(
                AudioChunkInfo {
                    frames: u32::try_from(window).expect("window fits u32"),
                    ..meta
                },
                sample_buffer(&renderer.pools, &samples),
            ))
            .continue_value()
            .expect("prepared quantum")
            .expect("unity emits the crossfaded source");
        let span = quantum
            .meta
            .source_span
            .expect("unity publishes its mapping");
        if first_source.is_none() {
            first_source = Some(span.start());
        }
        assert_eq!(
            span.start(),
            first_source.expect("first quantum")
                + u64::try_from(output.len() / channels).expect("frames fit u64"),
            "unity quanta are source-contiguous"
        );
        output.extend_from_slice(&quantum.samples);
        fed += u64::try_from(window).expect("window fits u64");
        drop(quantum);
        renderer.prepare(renderer.spec);
        hang_reset!();
    }
    let first_source = first_source.expect("at least one quantum");
    for (index, &got) in output.iter().enumerate() {
        let frame = index / channels;
        let input = source(first_source + u64::try_from(frame).expect("frame fits u64"));
        if index >= replacement.len() {
            assert_eq!(got.to_bits(), input.to_bits(), "passthrough sample={index}");
            continue;
        }
        let gain =
            (frame + 1).to_f32().expect("frame fits") / fade_frames.to_f32().expect("fade fits");
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
