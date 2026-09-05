use std::{
    collections::HashSet,
    num::{NonZero, NonZeroU32, NonZeroUsize},
};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunkInfo, AudioSpec};
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::{
    Consts, StretchControls, WarpRenderer, chunk, dominant_bin, expected_bin, flush_serviced,
    render_serviced, renderer, sine, spec,
};
use crate::{Warp, WarpConfig, test_pools::pools};

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_slow(StretchKind::Signalsmith, 0.5, 15)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_unity(StretchKind::Signalsmith, 1.0, 128)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_fast(StretchKind::Signalsmith, 2.0, 63)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_slow(StretchKind::Bungee, 0.5, 15)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_unity(StretchKind::Bungee, 1.0, 128)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_fast(StretchKind::Bungee, 2.0, 63)
)]
fn source_span_is_planned_from_the_output_quantum(
    #[case] backend: StretchKind,
    #[case] speed: f32,
    #[case] expected_source_frames: usize,
) {
    let controls = StretchControls::new(speed);
    controls.set_backend(backend);
    let config = WarpConfig::builder()
        .stretch(controls)
        .render_quantum_frames(NonZeroUsize::new(32).expect("test quantum is non-zero"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer.prepare(spec());

    let frames = renderer
        .prepare_quantum(AudioChunkInfo::default(), 128)
        .expect("test source span is plannable");

    assert_eq!(frames.get(), expected_source_frames);
}

#[kithara::test]
fn rendered_quantum_keeps_the_rate_revision_selected_during_planning() {
    let controls = StretchControls::new(1.0);
    let expected_revision = controls.set_speed(1.0);
    let mut renderer = renderer(Arc::clone(&controls));
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &sine(128));

    let frames = renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("test source span is plannable");
    assert_eq!(frames.get(), input.frames());
    controls.set_speed(0.5);

    let output = renderer
        .render_quantum(input)
        .expect("prepared unity quantum renders");
    assert_eq!(output.meta.render_revision, expected_revision);
}

fn keylocked(kind: StretchKind, speed: f32) -> WarpRenderer {
    let controls = StretchControls::new(speed);
    controls.set_keylock(true);
    controls.set_backend(kind);
    renderer(controls)
}

fn vinyl(kind: StretchKind, speed: f32) -> WarpRenderer {
    let controls = StretchControls::new(speed);
    controls.set_keylock(false);
    controls.set_backend(kind);
    renderer(controls)
}

fn render_with_tail(fx: &mut WarpRenderer, input: &[f32]) -> (Vec<f32>, usize) {
    let pools = fx.pools.clone();
    let mut out: Vec<f32> = Vec::new();
    let mut tail_frames = 0;
    let block = 4096 * usize::from(Consts::CH);
    for data in input.chunks(block) {
        if let Some(c) = render_serviced(fx, chunk(&pools, data)) {
            assert_eq!(
                c.spec().sample_rate.get(),
                Consts::SR,
                "stretch preserves sample rate"
            );
            assert_eq!(c.spec().channels, Consts::CH);
            out.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = flush_serviced(fx) {
        // A non-empty flush chunk carries real audio, so its spec must stay
        // the source spec - never the `AudioChunkInfo::default()` sentinel (0
        // channels) that a `None` `last_input_meta` would otherwise yield.
        assert_eq!(c.spec().channels, Consts::CH, "flush preserves channels");
        assert_eq!(
            c.spec().sample_rate.get(),
            Consts::SR,
            "flush preserves sample rate"
        );
        tail_frames += c.frames();
        out.extend_from_slice(&c.samples);
    }
    (out, tail_frames)
}

fn render(fx: &mut WarpRenderer, input: &[f32]) -> Vec<f32> {
    render_with_tail(fx, input).0
}

fn run_keylocked_with_tail(kind: StretchKind, speed: f32, in_frames: usize) -> (Vec<f32>, usize) {
    let input = sine(in_frames);
    render_with_tail(&mut keylocked(kind, speed), &input)
}

fn run_vinyl(kind: StretchKind, speed: f32, in_frames: usize) -> Vec<f32> {
    let input = sine(in_frames);
    render(&mut vinyl(kind, speed), &input)
}

/// Half playback speed -> stretch 2.0 -> ~double duration, pitch held.
/// Shared across every compiled-in backend.
fn assert_half_speed_contract(kind: StretchKind) {
    let channels = usize::from(Consts::CH);
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2; // 2 s
    let (out, tail_frames) = run_keylocked_with_tail(kind, 0.5, in_frames);
    let out_frames = out.len() / channels;
    let timeline_frames = out_frames - tail_frames;
    let expected_timeline = in_frames * 2;

    assert_eq!(
        timeline_frames, expected_timeline,
        "{kind:?}: exact half-speed timeline"
    );
    assert!(tail_frames > 0, "{kind:?}: terminal history is drained");

    // Pitch is still measured over the complete emitted stream, including
    // the latency fill and its matching terminal drain.
    assert!(
        out_frames >= expected_timeline,
        "{kind:?}: terminal drain cannot shorten the exact timeline"
    );

    // Pitch preserved: dominant bin still at F0 (the load-bearing check -
    // a resampler-in-disguise would shift it).
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= Consts::N,
        "{kind:?}: not enough output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(Consts::F0);
    assert!(
        peak.abs_diff(want) <= 3,
        "{kind:?}: pitch moved under time-stretch: peak bin {peak}, expected {want}"
    );
}

fn assert_unity_contract(kind: StretchKind) {
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2;
    let input = sine(in_frames);
    let out = render(&mut keylocked(kind, 1.0), &input);
    assert_eq!(out, input, "{kind:?}: unity speed must bypass byte-exact");
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn half_speed_and_unity_contracts(#[case] backend: StretchKind) {
    assert_half_speed_contract(backend);
    assert_unity_contract(backend);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_frontier_excludes_backend_lookahead(#[case] backend: StretchKind) {
    const SOURCE_START: u64 = 10_000;
    const SOURCE_FRAMES: usize = 4096;

    let mut renderer = keylocked(backend, 0.5);
    let pools = renderer.pools.clone();
    let source = sine(SOURCE_FRAMES);
    let mut input = chunk(&pools, &source);
    input.meta.frame_offset = SOURCE_START;
    input.meta.timestamp = spec()
        .duration_for(SOURCE_START)
        .expect("test source timestamp fits");
    let admitted = SOURCE_START
        .checked_add(u64::try_from(SOURCE_FRAMES).expect("source frame count fits u64"))
        .expect("source frontier fits u64");
    input.meta.end_timestamp = spec()
        .duration_for(admitted)
        .expect("test source end timestamp fits");
    let source_latency = renderer
        .engine
        .as_ref()
        .expect("compiled backend is available")
        .capabilities()
        .latency()
        .source_frames();
    assert!(source_latency > 0, "backend must declare source lookahead");

    let output = render_serviced(&mut renderer, input).expect("half-speed render emits samples");
    let held =
        u64::try_from(source_latency.min(SOURCE_FRAMES)).expect("backend source latency fits u64");
    let expected_frame = admitted.saturating_sub(held);

    assert_eq!(
        renderer.rendered_source_end(),
        Some((
            expected_frame,
            NonZeroU32::new(Consts::SR).expect("test sample rate is non-zero"),
        )),
        "rendered progress excludes source still retained by the backend"
    );
    assert_ne!(
        output
            .meta
            .frame_offset
            .saturating_add(u64::from(output.meta.frames)),
        expected_frame,
        "the oracle must distinguish transformed output frames from the source frontier"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_frontier_reaches_end_only_on_completed_drain(#[case] backend: StretchKind) {
    const SOURCE_START: u64 = 10_000;
    const SOURCE_FRAMES: usize = WarpRenderer::MAX_SOURCE_FRAMES;

    let mut renderer = keylocked(backend, StretchControls::MIN_SPEED);
    let pools = renderer.pools.clone();
    let source = sine(SOURCE_FRAMES);
    let mut input = chunk(&pools, &source);
    input.meta.frame_offset = SOURCE_START;
    let admitted = SOURCE_START
        .checked_add(u64::try_from(SOURCE_FRAMES).expect("source frame count fits u64"))
        .expect("source frontier fits u64");
    let source_latency = renderer
        .engine
        .as_ref()
        .expect("compiled backend is available")
        .capabilities()
        .latency()
        .source_frames();
    let held =
        u64::try_from(source_latency.min(SOURCE_FRAMES)).expect("backend source latency fits u64");

    render_serviced(&mut renderer, input).expect("minimum-speed render emits samples");
    assert_eq!(
        renderer.rendered_source_end(),
        Some((admitted - held, spec().sample_rate))
    );

    let mut frontiers = Vec::new();
    while let Some(tail) = flush_serviced(&mut renderer) {
        assert!(tail.frames() > 0, "terminal chunk carries real samples");
        frontiers.push(renderer.rendered_source_end());
        assert!(frontiers.len() < 64, "terminal drain must converge");
    }

    assert!(
        frontiers.len() > 1,
        "fixture must exercise a multi-chunk terminal drain"
    );
    assert!(
        frontiers[..frontiers.len() - 1]
            .iter()
            .all(|frontier| *frontier != Some((admitted, spec().sample_rate))),
        "an incomplete tail chunk cannot publish the full source frontier"
    );
    assert_eq!(
        frontiers.last().copied().flatten(),
        Some((admitted, spec().sample_rate)),
        "the completed tail releases the held source frontier"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn output_meta_preserves_decoder_timeline(#[case] backend: StretchKind) {
    let channels = usize::from(Consts::CH);
    let mut fx = keylocked(backend, 0.5);
    let pools = fx.pools.clone();
    let cf = 1024usize;
    let block = sine(cf);
    let mut fed_ends = HashSet::new();
    let mut emitted = Vec::new();
    for i in 0..40u64 {
        let mut c = chunk(&pools, &block);
        let end = Duration::from_millis(i * 100 + 100);
        c.meta.timestamp = Duration::from_millis(i * 100);
        c.meta.end_timestamp = end;
        c.meta.frame_offset = i * u64::try_from(cf).unwrap();
        fed_ends.insert(end);
        if let Some(o) = render_serviced(&mut fx, c) {
            emitted.push(o);
        }
    }
    while let Some(o) = flush_serviced(&mut fx) {
        emitted.push(o);
    }
    assert!(!emitted.is_empty(), "stretch produced no output");
    for o in &emitted {
        assert_eq!(
            o.spec(),
            AudioSpec {
                channels: Consts::CH,
                sample_rate: NonZero::new(Consts::SR).unwrap()
            },
            "spec (incl. sample rate) preserved verbatim"
        );
        assert_eq!(
            usize::try_from(o.meta.frames).unwrap(),
            o.samples.len() / channels,
            "frames recomputed to the actual output count"
        );
        assert!(
            fed_ends.contains(&o.meta.end_timestamp),
            "end_timestamp carried verbatim from an input chunk (source-track time)"
        );
    }
}

/// Key-lock off is vinyl mode: speed changes duration and pitch in the
/// stretch slot, with no resampler-rate handoff.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn vinyl_speed_scales_duration_and_pitch(#[case] backend: StretchKind) {
    let channels = usize::from(Consts::CH);
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2;
    let out = run_vinyl(backend, 2.0, in_frames);
    let out_frames = out.len() / channels;
    assert!(
        out_frames * 10 >= in_frames * 4 && out_frames * 10 <= in_frames * 6,
        "vinyl 2x should roughly halve duration, got {out_frames} from {in_frames}"
    );
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= Consts::N,
        "not enough vinyl output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(Consts::F0 * 2.0);
    assert!(
        peak.abs_diff(want) <= 4,
        "vinyl pitch did not follow speed: peak bin {peak}, expected {want}"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn live_speed_change_updates_stretch_duration(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let mut fx = renderer(Arc::clone(&controls));
    let pools = fx.pools.clone();
    let block = sine(4096);
    let unity = render_serviced(&mut fx, chunk(&pools, &block)).expect("unity bypass emits");
    assert_eq!(&unity.samples[..], &block[..], "unity phase bypasses");

    controls.set_speed(0.5);
    let mut stretched: Vec<f32> = Vec::new();
    for _ in 0..24 {
        if let Some(c) = render_serviced(&mut fx, chunk(&pools, &block)) {
            stretched.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = flush_serviced(&mut fx) {
        stretched.extend_from_slice(&c.samples);
    }
    assert!(
        stretched.len() > block.len() * 24,
        "half-speed key-lock should lengthen output after a live speed change"
    );
}

/// Flipping key-lock mid-stream switches from vinyl pitch shift to
/// pitch-preserving stretch - no reload.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn live_keylock_toggle_switches_pitch_mode(#[case] backend: StretchKind) {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(false);
    controls.set_backend(backend);
    let mut fx = renderer(Arc::clone(&controls));
    let pools = fx.pools.clone();
    let block = sine(4096);

    let mut vinyl_out: Vec<f32> = Vec::new();
    for _ in 0..24 {
        if let Some(c) = render_serviced(&mut fx, chunk(&pools, &block)) {
            vinyl_out.extend_from_slice(&c.samples);
        }
    }
    let vinyl_mono: Vec<f32> = vinyl_out
        .iter()
        .step_by(usize::from(Consts::CH))
        .copied()
        .collect();
    assert!(
        dominant_bin(&vinyl_mono).abs_diff(expected_bin(Consts::F0 * 0.5)) <= 4,
        "off: vinyl pitch follows speed"
    );

    controls.set_keylock(true);
    let mut stretched: Vec<f32> = Vec::new();
    for _ in 0..24 {
        if let Some(c) = render_serviced(&mut fx, chunk(&pools, &block)) {
            stretched.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = flush_serviced(&mut fx) {
        stretched.extend_from_slice(&c.samples);
    }
    let mono: Vec<f32> = stretched
        .iter()
        .step_by(usize::from(Consts::CH))
        .copied()
        .collect();
    assert!(
        mono.len() >= Consts::N,
        "on: not enough output for the FFT window"
    );
    assert!(
        dominant_bin(&mono).abs_diff(expected_bin(Consts::F0)) <= 3,
        "on: pitch preserved after live toggle"
    );
}
