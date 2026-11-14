#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_stretch::StretchKind;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use {
    crate::{WarpConfig, consts},
    kithara_sync::SpeedCurve,
};

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use super::flush_serviced;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use super::{LaneFeed, chunk, renderer, samples, spec};
use super::{WarpRenderer, f64_of};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use crate::Warp;

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn assert_identity_quantum(
    renderer: &mut WarpRenderer,
    source: &mut u64,
    signal: impl Fn(u64) -> f32,
) {
    let frontier = renderer.rendered_source_end();
    renderer.prepare(spec());
    assert_eq!(
        renderer
            .prepare_engine_latency(spec())
            .expect("identity latency")
            .get(),
        0
    );
    let samples: Vec<_> = (0..128)
        .flat_map(|offset| [signal(*source + offset); 2])
        .collect();
    let mut input = chunk(&renderer.pools, &samples);
    input.meta.frame_offset = *source;
    let pointer = input.samples.as_ptr();
    assert_eq!(
        renderer
            .prepare_source_window(input.meta, 128, 128)
            .expect("identity quantum")
            .get(),
        128,
        "converged identity consumes the exact source quantum"
    );
    assert_eq!(
        renderer.rendered_source_end(),
        frontier,
        "preparation does not publish unrendered unity"
    );
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared identity")
        .expect("unity PCM");
    assert_eq!(output.frames(), 128);
    assert_eq!(
        &*output.samples, samples,
        "settled keylocked unity is source PCM bit-exact"
    );
    assert_eq!(
        output.samples.as_ptr(),
        pointer,
        "identity returns the input buffer without copying"
    );
    assert!(!renderer.active, "no engine processes identity samples");
    assert!(
        renderer.applied_pitch.is_nan(),
        "identity does not invoke the prepared engine"
    );
    assert_eq!(
        output.meta.source_span.expect("identity mapping").start(),
        *source
    );
    *source += 128;
    assert_eq!(
        renderer.rendered_source_end(),
        Some((*source, spec().sample_rate))
    );
}

#[kithara::test]
#[cfg(feature = "stretch-signalsmith")]
fn manual_ramp_to_the_rate_limit_keeps_quantized_requests_bounded() {
    let config = WarpConfig::builder()
        .speed(2.0)
        .keylock(true)
        .backend(StretchKind::Signalsmith)
        .backends(
            kithara_stretch::ElasticBackendConfig::builder()
                .signalsmith(
                    kithara_stretch::SignalsmithConfig::builder()
                        .block_frames(std::num::NonZeroUsize::new(224).expect("non-zero block"))
                        .interval_frames(
                            std::num::NonZeroUsize::new(32).expect("non-zero interval"),
                        )
                        .build()
                        .expect("valid Signalsmith geometry"),
                )
                .build(),
        )
        .render_quantum_frames(std::num::NonZeroUsize::new(32).expect("non-zero quantum"))
        .build();
    let mut fx = Warp::new((), &config).renderer(spec(), crate::test_pools::pools());
    let pools = fx.pools.clone();
    fx.prepare(spec());
    fx.set_speed(
        SpeedCurve::Ramp {
            to: 4.0,
            frames: std::num::NonZeroU64::new(882).expect("ramp duration"),
        },
        1,
    )
    .expect("valid speed");
    let mut source_frame = 0_u64;
    let mut output_frames = 0;
    for _ in 0..400 {
        fx.prepare(spec());
        fx.prepare_engine_latency(spec()).expect("engine is primed");
        let meta = kithara_signal::AudioChunkInfo {
            spec: spec(),
            frame_offset: source_frame,
            ..Default::default()
        };
        let frames = fx
            .prepare_source_window(meta, 128, usize::MAX)
            .expect("a continuous ramp keeps accepting source quanta")
            .get();
        let mut input = chunk(&pools, &vec![0.25; frames * usize::from(consts::CH)]);
        input.meta.frame_offset = source_frame;
        source_frame += u64::try_from(frames).expect("quantum fits u64");
        let output = fx
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape")
            .expect("a complete ramp quantum emits PCM");
        output_frames += output.frames();
        assert!(output.frames() <= 32, "the output quantum stays bounded");
    }
    assert!(
        output_frames > 10_000,
        "rendering continues past the former stall"
    );
    assert_eq!(fx.trajectory.speed().expect("settled speed"), 4.0);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test(timeout(Short))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn live_unity_transition_drains_active_backend_tail(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(true)
        .backend(backend)
        .build();
    let signal = |frame: u64| {
        let frame = usize::try_from(frame).expect("test source frame fits usize");
        warp_sine[frame * 2 % warp_sine.len()]
    };
    let render = || {
        let mut fx = renderer(&config);
        let mut twin = renderer(&config);
        let mut source = 0;
        let mut twin_source = 0;
        for _ in 0..32 {
            let output = super::exact::mapped_signal(&mut fx, &mut source, 128, signal);
            assert_eq!(output.frames(), 128);
            super::exact::mapped_signal(&mut twin, &mut twin_source, 128, signal);
        }
        let half_frame = super::exact::mapped_signal(&mut fx, &mut source, 1, signal);
        assert_eq!(half_frame.frames(), 1);
        super::exact::mapped_signal(&mut twin, &mut twin_source, 1, signal);
        let tail = super::exact::supported_tail(&mut twin);
        let before = fx
            .trajectory
            .span(0, spec().sample_rate, 1)
            .expect("old phase")
            .source_ratio_at(0)
            .expect("source position");
        assert_eq!(
            before.0 % before.1.get() * 2,
            before.1.get(),
            "the fixture exercises an exact half-frame tie"
        );
        let snapped = before.0 / before.1.get() + 1;
        assert!(
            before.0 < u128::from(source) * before.1.get(),
            "the active engine retains decoded lookahead"
        );
        fx.set_speed(SpeedCurve::Constant(1.0), 1)
            .expect("unity command");
        fx.prepare_engine_latency(spec())
            .expect("frame-aligned reinitialisation");
        let mut samples = Vec::new();
        let mut quanta = Vec::new();
        for index in 0..=tail.len().div_ceil(128) {
            let output = super::exact::mapped_signal(&mut fx, &mut source, 128, signal);
            assert_eq!(
                output.frames(),
                128,
                "reinitialisation does not starve output"
            );
            let span = output.meta.source_span.expect("unity mapping");
            if index == 0 {
                assert_eq!(
                    span.source_ratio_at(0),
                    Some((snapped, std::num::NonZeroU128::MIN)),
                    "R6 identity starts at the nearest frame, ties forward; only the crossfade hides this half-frame snap"
                );
                assert!(
                    (snapped * before.1.get()).abs_diff(before.0) * 2 <= before.1.get(),
                    "R6 identity snap is at most half a source frame; the retiring tail remains exact"
                );
            }
            assert_eq!(span.end() - span.start(), 128);
            assert_eq!(
                fx.rendered_source_end(),
                Some((span.end(), spec().sample_rate))
            );
            assert!(
                span.end() <= source,
                "the frontier never advances beyond emitted source"
            );
            let identity: Vec<f32> = (span.start()..span.end())
                .flat_map(|frame| [signal(frame); 2])
                .collect();
            quanta.push(output.frames());
            samples.extend_from_slice(&output.samples);
            fx.prepare(spec());
            if output.samples[..] == identity[..] {
                super::exact::assert_fades_from(samples.as_chunks::<2>().0, &tail);
                assert!(!fx.transition_pending());
                assert!(samples.iter().any(|sample| sample.abs() > f32::EPSILON));
                assert!(samples.iter().all(|sample| sample.is_finite()));
                let peak = samples
                    .iter()
                    .map(|sample| sample.abs())
                    .fold(0.0_f32, f32::max);
                let energy = samples
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>()
                    / f64_of(samples.len());
                assert!(
                    energy > 0.0 && energy <= 1.0,
                    "live tail energy stays finite and normalized: {energy}"
                );
                assert!(peak <= 1.0, "crossfaded tail remains normalized: {peak}");
                for _ in 0..3 {
                    assert_identity_quantum(&mut fx, &mut source, signal);
                }
                assert!(
                    flush_serviced(&mut fx).is_none(),
                    "completed unity has no tail to flush"
                );
                return (samples, quanta);
            }
        }
        panic!("active-to-unity crossfade must converge");
    };
    let first_run = render();
    let second_run = render();
    assert_eq!(
        second_run.1, first_run.1,
        "repeated live runs have deterministic per-quantum progression"
    );
    assert_eq!(second_run.0.len(), first_run.0.len());
    assert_eq!(
        second_run.0, first_run.0,
        "source-aligned reinitialisation is deterministic"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn a_fractional_identity_phase_snaps_to_its_nearest_frame_without_starving(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let config = WarpConfig::builder()
        .speed(1.0 / 1.6)
        .keylock(true)
        .backend(backend)
        .build();
    let signal = |frame: u64| {
        let frame = usize::try_from(frame).expect("test source frame fits usize");
        warp_sine[frame * 2 % warp_sine.len()]
    };
    let mut fx = renderer(&config);
    let mut twin = renderer(&config);
    let mut source = 0;
    let first = super::exact::mapped_signal(&mut fx, &mut source, 2, signal);
    assert_eq!(first.frames(), 2);
    super::exact::mapped_signal(&mut twin, &mut 0, 2, signal);
    let tail = super::exact::supported_tail(&mut twin);
    fx.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity command");
    fx.prepare_engine_latency(spec())
        .expect("identity transition");
    for index in 0..=tail.len().div_ceil(128) {
        let unity = super::exact::mapped_signal(&mut fx, &mut source, 128, signal);
        assert_eq!(unity.frames(), 128, "the identity transition adds no frame");
        let span = unity.meta.source_span.expect("unity mapping");
        if index == 0 {
            assert_eq!(
                span.source_ratio_at(0),
                Some((1, std::num::NonZeroU128::MIN)),
                "R6 rounds the 1.25-frame identity phase to frame 1 under the tail crossfade"
            );
        }
        let identity: Vec<f32> = (span.start()..span.end())
            .flat_map(|frame| [signal(frame); 2])
            .collect();
        fx.prepare(spec());
        if unity.samples[..] == identity[..] {
            assert!(!fx.transition_pending());
            return;
        }
    }
    panic!("the complete unity transition must converge");
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn keylocked_unity_reenters_the_engine_without_a_source_jump_or_starvation(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .build(),
    );
    let mut source = 0;
    for _ in 0..32 {
        assert_identity_quantum(&mut fx, &mut source, |frame| {
            let frame = usize::try_from(frame).expect("test source frame fits usize");
            warp_sine[frame * 2 % warp_sine.len()]
        });
    }
    let before = fx
        .trajectory
        .span(0, spec().sample_rate, 1)
        .expect("identity phase")
        .source_ratio_at(0)
        .expect("exact identity endpoint");
    let frontier = fx.rendered_source_end();
    fx.set_speed(SpeedCurve::Constant(0.5), 1)
        .expect("leave unity");
    assert!(
        fx.prepare_engine_latency(spec())
            .expect("engine re-entry")
            .get()
            > 0
    );
    assert_eq!(
        fx.rendered_source_end(),
        frontier,
        "re-entry preparation never jumps source"
    );
    let mut expected = Some(before);
    for _ in 0..16 {
        let output = super::exact::mapped_signal(&mut fx, &mut source, 128, |frame| {
            let frame = usize::try_from(frame).expect("test source frame fits usize");
            warp_sine[frame * 2 % warp_sine.len()]
        });
        assert_eq!(
            output.frames(),
            128,
            "engine re-entry does not starve a quantum"
        );
        let span = output.meta.source_span.expect("re-entry mapping");
        assert_eq!(
            span.source_ratio_at(0),
            expected,
            "non-unity re-entry keeps the exact source phase"
        );
        expected = span.source_ratio_at(span.output_frames());
        assert_eq!(
            fx.rendered_source_end(),
            Some((span.end(), spec().sample_rate))
        );
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
        assert!(
            output
                .samples
                .iter()
                .any(|sample| sample.abs() > f32::EPSILON)
        );
        assert!(fx.active, "non-unity invokes the engine again");
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn reset_discards_pending_span_before_new_timeline(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0 / 0.75)
            .keylock(true)
            .backend(backend)
            .build(),
    );
    let pools = fx.pools.clone();
    const HELD: usize = 65_536;
    let source = warp_sine[..(HELD + 1) * 2].to_vec();
    let held = LaneFeed::default().feed(&mut fx, chunk(&pools, &source[..HELD * 2]));
    assert!(
        !held.is_empty() && fx.active,
        "the engine holds a running span"
    );
    drop(held);

    fx.reset();
    fx.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("valid speed");
    fx.prepare(spec());
    let mut landed = chunk(&pools, &source[HELD * 2..]);
    landed.meta.frame_offset = 100;
    let landed_at = spec().duration_for(100).expect("landed timestamp fits");
    landed.meta.timestamp = landed_at;
    let expected = landed.samples.to_vec();
    let outputs = LaneFeed::default().feed(&mut fx, landed);
    let [output] = outputs.as_slice() else {
        panic!("post-seek unity passes through as one quantum");
    };
    assert_eq!(output.meta.frame_offset, 100);
    assert_eq!(output.meta.timestamp, landed_at);
    assert_eq!(&output.samples[..], &expected);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn a_deferred_reset_renders_like_a_fresh_renderer(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    const BLOCK_FRAMES: usize = 4096;
    const SEEK_FRAME: usize = 40_960;
    let config = WarpConfig::builder()
        .speed(0.75)
        .keylock(true)
        .backend(backend)
        .build();
    let channels = usize::from(consts::CH);
    let render_at = |fx: &mut WarpRenderer, feed: &mut LaneFeed, frame: usize| {
        let start = frame * channels;
        let mut input = chunk(
            &fx.pools,
            &warp_sine[start..start + BLOCK_FRAMES * channels],
        );
        input.meta.frame_offset = u64::try_from(frame).expect("source frame fits u64");
        samples(&feed.feed(fx, input))
    };

    let mut seeked = renderer(&config);
    let mut feed = LaneFeed::default();
    for frame in (0..4).map(|block| block * BLOCK_FRAMES) {
        render_at(&mut seeked, &mut feed, frame);
    }
    seeked.reset();
    seeked.prepare(spec());
    let mut feed = LaneFeed::default();
    let mut after_reset = render_at(&mut seeked, &mut feed, SEEK_FRAME);
    after_reset.extend(samples(&feed.end(&mut seeked)));
    let mut fresh_renderer = renderer(&config);
    let mut feed = LaneFeed::default();
    let mut fresh = render_at(&mut fresh_renderer, &mut feed, SEEK_FRAME);
    fresh.extend(samples(&feed.end(&mut fresh_renderer)));

    assert!(!fresh.is_empty(), "the first block after the seek renders");
    assert_eq!(after_reset.len(), fresh.len());
    let mismatch = after_reset
        .iter()
        .zip(fresh.iter())
        .position(|(actual, expected)| actual.to_bits() != expected.to_bits());
    assert_eq!(
        mismatch, None,
        "the first block after a reset carries no earlier stream history"
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn moving_target_renderer() -> WarpRenderer {
    renderer(&WarpConfig::builder().speed(1.0).build())
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn moving_target_advance(blocks: usize, frames: usize) -> f64 {
    let mut fx = moving_target_renderer();
    fx.set_speed(
        SpeedCurve::Ramp {
            to: 1.2,
            frames: std::num::NonZeroU64::new(4_096).expect("ramp duration"),
        },
        1,
    )
    .expect("ramp");
    let mut total = 0.0;
    for _ in 0..blocks {
        let span = fx
            .trajectory
            .span(0, spec().sample_rate, frames)
            .expect("curve span");
        let first = span.source_ratio_at(0).expect("start");
        let last = span.source_ratio_at(span.output_frames()).expect("end");
        total += last.0.to_f64().expect("end numerator fits f64")
            / last.1.get().to_f64().expect("end denominator fits f64")
            - first.0.to_f64().expect("start numerator fits f64")
                / first.1.get().to_f64().expect("start denominator fits f64");
        fx.trajectory.advance(span).expect("advance");
    }
    total
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native, flash(false))]
fn a_moving_target_advances_the_same_whatever_the_partitioning() {
    let one_block = moving_target_advance(1, 2_048);
    let many_blocks = moving_target_advance(16, 128);

    assert!(
        (one_block - many_blocks).abs() < 1e-3,
        "the same 2048 frames of a moving target must consume the same source \
         whether rendered as one block or sixteen: {one_block} vs {many_blocks}",
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native, flash(false))]
fn a_settled_target_keeps_its_exact_multiplier() {
    let fx = moving_target_renderer();

    assert_eq!(
        fx.trajectory.speed().expect("constant speed"),
        1.0,
        "a target that never moves must not be perturbed by averaging",
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_prepared_smoothed_quantum_keeps_the_identity_of_its_request() {
    let mut fx = renderer(&WarpConfig::builder().speed(1.0).build());
    fx.set_speed(
        SpeedCurve::Ramp {
            to: 1.25,
            frames: std::num::NonZeroU64::new(512).expect("ramp duration"),
        },
        1,
    )
    .expect("valid speed");
    let target = fx.rate;
    let expected_speed = fx.trajectory.speed().expect("ramp origin");
    let meta = kithara_signal::AudioChunkInfo {
        spec: spec(),
        frames: 128,
        ..Default::default()
    };
    fx.prepare_source_window(meta, 128, usize::MAX)
        .expect("manual span is plannable");
    let prepared = fx.prepared_quantum.expect("quantum was prepared");

    assert_eq!(prepared.rate.revision(), target.revision());
    assert_eq!(prepared.rate.speed(), 1.0);
    assert_eq!(prepared.speed, expected_speed);
    assert!(prepared.speed < 1.25);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
struct UnityQuantum {
    output: kithara_signal::AudioChunk,
    input: Vec<f32>,
    input_ptr: *const f32,
    was_active: bool,
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn varispeed_quantum(
    fx: &mut WarpRenderer,
    source: &mut u64,
    signal: impl Fn(u64) -> f32,
) -> UnityQuantum {
    fx.prepare(spec());
    fx.prepare_engine_latency(spec()).expect("renderer service");
    let meta = kithara_signal::AudioChunkInfo {
        spec: spec(),
        frame_offset: *source,
        timestamp: spec().duration_for(*source).expect("source timestamp"),
        ..Default::default()
    };
    let frames = fx
        .prepare_source_window(meta, 4096, usize::MAX)
        .expect("source window")
        .get();
    let input: Vec<f32> = (0..frames)
        .flat_map(|offset| [signal(*source + offset as u64); 2])
        .collect();
    let mut chunk = chunk(&fx.pools, &input);
    chunk.meta = kithara_signal::AudioChunkInfo {
        frames: u32::try_from(frames).expect("window fits u32"),
        ..meta
    };
    let input_ptr = chunk.samples.as_ptr();
    let was_active = fx.active;
    *source += u64::try_from(frames).expect("window fits u64");
    let output = fx
        .render_quantum(chunk)
        .continue_value()
        .expect("prepared quantum")
        .expect("quantum PCM");
    UnityQuantum {
        output,
        input,
        input_ptr,
        was_active,
    }
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn hand_over_to_unity(
    fx: &mut WarpRenderer,
    source: &mut u64,
    signal: impl Fn(u64) -> f32 + Copy,
) -> Vec<UnityQuantum> {
    for _ in 0..16 {
        drop(varispeed_quantum(fx, source, signal));
    }
    assert!(fx.active, "the varispeed run drives the engine");
    fx.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity command");
    let mut quanta = Vec::new();
    for _ in 0..4 {
        let quantum = varispeed_quantum(fx, source, signal);
        let handed_over = !fx.active;
        quanta.push(quantum);
        if handed_over {
            return quanta;
        }
    }
    panic!("an active varispeed run must hand over to unity");
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn an_active_varispeed_run_hands_its_queued_unity_quantum_over_without_a_copy(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(false)
        .backend(backend)
        .build();
    let signal = |frame: u64| {
        let frame = usize::try_from(frame).expect("test source frame fits usize");
        warp_sine[frame * 2 % warp_sine.len()]
    };
    let mut fx = renderer(&config);
    let mut source = 0;
    let quanta = hand_over_to_unity(&mut fx, &mut source, signal);
    let unity = quanta.last().expect("hand-over quantum");
    assert!(unity.was_active, "unity is queued behind the active engine");
    assert_eq!(
        &unity.output.samples[..],
        &unity.input[..],
        "unity samples follow the drained varispeed tail byte-for-byte"
    );
    assert_eq!(
        unity.output.samples.as_ptr(),
        unity.input_ptr,
        "queued unity samples return without copying"
    );
    assert!(!fx.transition_pending());
    assert_eq!(
        fx.rendered_source_end(),
        Some((source, spec().sample_rate)),
        "the source frontier advances only after tail and unity samples are emitted"
    );
    assert!(flush_serviced(&mut fx).is_none());
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_is_committed_before_live_unity_passthrough(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.5)
            .keylock(false)
            .backend(backend)
            .build(),
    );
    let signal = |frame: u64| {
        let frame = usize::try_from(frame).expect("test source frame fits usize");
        warp_sine[frame * 2 % warp_sine.len()]
    };
    let mut source = 0;
    for _ in 0..16 {
        drop(varispeed_quantum(&mut fx, &mut source, signal));
    }
    fx.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity command");
    let mut committed = fx.rendered_source_end().expect("active frontier").0;
    let mut handed_over = false;
    for _ in 0..4 {
        let quantum = varispeed_quantum(&mut fx, &mut source, signal);
        let span = quantum.output.meta.source_span.expect("published mapping");
        assert_eq!(
            span.start(),
            committed,
            "each quantum starts at the source the previous one committed"
        );
        assert_eq!(
            quantum.output.meta.timestamp,
            span.position_at(0).expect("span start")
        );
        committed = span.end();
        if quantum.was_active && !fx.active {
            assert_eq!(&quantum.output.samples[..], &quantum.input[..]);
            handed_over = true;
            break;
        }
    }
    assert!(handed_over, "the active run hands over to unity");
    let passthrough = varispeed_quantum(&mut fx, &mut source, signal);
    assert_eq!(passthrough.output.samples.as_ptr(), passthrough.input_ptr);
    assert_eq!(&passthrough.output.samples[..], &passthrough.input[..]);
    assert_eq!(
        passthrough
            .output
            .meta
            .source_span
            .expect("unity mapping")
            .start(),
        committed
    );
    assert!(flush_serviced(&mut fx).is_none());
}
