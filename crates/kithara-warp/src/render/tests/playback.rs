#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use std::num::{NonZero, NonZeroUsize};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_stretch::StretchKind;
use kithara_sync::SpeedCurve;
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::kithara;

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use super::{LaneFeed, WarpRenderer, dominant_bin, drain_tail, expected_bin, samples as concat};
use super::{chunk, renderer, spec};
use crate::{Pitch, WarpConfig};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use crate::{Warp, consts, test_pools::pools};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide_slow(StretchKind::Glide, 0.5, 17)
)]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide_unity(StretchKind::Glide, 1.0, 32)
)]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide_fast(StretchKind::Glide, 2.0, 79)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_slow(StretchKind::Signalsmith, 0.5, 17)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_unity(StretchKind::Signalsmith, 1.0, 32)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_fast(StretchKind::Signalsmith, 2.0, 79)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_slow(StretchKind::Bungee, 0.5, 17)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_unity(StretchKind::Bungee, 1.0, 32)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_fast(StretchKind::Bungee, 2.0, 79)
)]
fn source_span_is_planned_from_the_output_quantum(
    #[case] backend: StretchKind,
    #[case] speed: f32,
    #[case] expected_source_frames: usize,
) {
    let config = WarpConfig::builder()
        .speed(speed)
        .backend(backend)
        .render_quantum_frames(NonZeroUsize::new(32).expect("test quantum is non-zero"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer.prepare(spec());

    let frames = renderer
        .prepare_source_window(
            AudioChunkInfo {
                spec: spec(),
                ..Default::default()
            },
            128,
            usize::MAX,
        )
        .expect("test source span is plannable");

    assert_eq!(frames.get(), expected_source_frames);
    assert_eq!(
        renderer
            .prepared_quantum
            .expect("prepared output quantum")
            .source_span
            .output_frames(),
        32
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn live_activation_primes_from_passthrough_history(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let config = WarpConfig::builder()
        .speed(1.0)
        .pitch(Pitch::HELD)
        .backend(backend)
        .render_quantum_frames(NonZeroUsize::new(32).expect("test quantum is non-zero"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer.prepare(spec());
    let latency = renderer
        .engine
        .as_ref()
        .expect("compiled backend is available")
        .capabilities()
        .latency();
    let cue = latency.first();
    assert!(cue > 0, "activation needs backend history");

    let pools = renderer.pools.clone();
    let source = warp_sine[..(cue) * 2].to_vec();
    let mut feed = LaneFeed::default();
    let mut unity = feed.feed(&mut renderer, chunk(&pools, &source));
    unity.extend(feed.end(&mut renderer));
    assert_eq!(concat(&unity), source, "unity history remains byte-exact");

    let revision = 1;
    renderer
        .set_speed(SpeedCurve::Constant(2.0), revision)
        .expect("valid speed");
    renderer.prepare(spec());
    renderer
        .prepare_engine_latency(spec())
        .expect("activation service");
    let mut meta = AudioChunkInfo {
        frame_offset: u64::try_from(cue).expect("cue fits u64"),
        spec: spec(),
        timestamp: spec()
            .duration_for(u64::try_from(cue).expect("cue fits u64"))
            .expect("cue timestamp fits"),
        ..AudioChunkInfo::default()
    };
    let input_frames = renderer
        .prepare_source_window(meta, 128, usize::MAX)
        .expect("activation quantum is plannable")
        .get();
    assert!(
        input_frames > 128,
        "activation includes lookahead and warmup"
    );
    let active = warp_sine[..(input_frames) * 2].to_vec();
    meta.frames = u32::try_from(input_frames).expect("input frames fit u32");
    meta.end_timestamp = meta
        .timestamp
        .checked_add(
            spec()
                .duration_for(u64::try_from(input_frames).expect("input frames fit u64"))
                .expect("input duration fits"),
        )
        .expect("input end timestamp fits");

    let mut input = chunk(&pools, &active);
    input.meta = meta;
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("primed activation emits immediately");

    assert_eq!(
        output.meta.frame_offset,
        u64::try_from(cue).expect("cue fits u64")
    );
    assert_eq!(output.meta.render_revision, revision);
    assert!(output.frames() > 0);
}

#[kithara::test]
fn a_speed_set_after_planning_replans_the_next_quantum(warp_sine: Vec<f32>) {
    let mut renderer = renderer(&WarpConfig::builder().speed(1.0).build());
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &warp_sine[..128 * 2]);

    renderer
        .prepare_source_window(input.meta, input.frames(), usize::MAX)
        .expect("test source span is plannable");
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 2)
        .expect("valid speed");
    let input = renderer
        .render_quantum(input)
        .break_value()
        .expect("the quantum planned before the speed is dropped");

    let frames = renderer
        .prepare_source_window(input.meta, input.frames(), usize::MAX)
        .expect("test source span is plannable");
    assert_eq!(frames.get(), input.frames());
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("prepared unity quantum renders");
    assert_eq!(output.meta.render_revision, 2);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn keylocked(kind: StretchKind, speed: f32) -> WarpRenderer {
    renderer(
        &WarpConfig::builder()
            .speed(speed)
            .pitch(Pitch::HELD)
            .backend(kind)
            .build(),
    )
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(super) fn vinyl(kind: StretchKind, speed: f32) -> WarpRenderer {
    renderer(&WarpConfig::builder().speed(speed).backend(kind).build())
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::hang_watchdog]
fn render_with_tail(fx: &mut WarpRenderer, input: &[f32]) -> (Vec<f32>, usize) {
    let pools = fx.pools.clone();
    let mut out: Vec<f32> = Vec::new();
    let mut tail_frames = 0;
    let mut source = 0;
    let mut feed = LaneFeed::default();
    let block = 4096 * usize::from(consts::CH);
    let present = |out: &mut Vec<f32>, timeline: Vec<AudioChunk>| {
        for c in timeline {
            assert_eq!(
                c.spec().sample_rate.get(),
                consts::SR,
                "stretch preserves sample rate"
            );
            assert_eq!(c.spec().channels, consts::CH);
            out.extend_from_slice(&c.samples);
        }
    };
    for data in input.chunks(block) {
        hang_tick!();
        let mut input = chunk(&pools, data);
        input.meta.frame_offset = source;
        source += u64::try_from(input.frames()).expect("source frames fit u64");
        present(&mut out, feed.feed(fx, input));
        hang_reset!();
    }
    present(&mut out, feed.end(fx));
    for c in drain_tail(fx) {
        hang_tick!();
        // A non-empty flush chunk carries real audio, so its spec must stay
        // the source spec - never the `AudioChunkInfo::default()` sentinel (0
        // channels) that a `None` `last_input_meta` would otherwise yield.
        assert_eq!(c.spec().channels, consts::CH, "flush preserves channels");
        assert_eq!(
            c.spec().sample_rate.get(),
            consts::SR,
            "flush preserves sample rate"
        );
        tail_frames += c.frames();
        out.extend_from_slice(&c.samples);
        hang_reset!();
    }
    (out, tail_frames)
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(super) fn render(fx: &mut WarpRenderer, input: &[f32]) -> Vec<f32> {
    render_with_tail(fx, input).0
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn run_keylocked_with_tail(
    warp_sine: &[f32],
    kind: StretchKind,
    speed: f32,
    in_frames: usize,
) -> (Vec<f32>, usize) {
    let input = warp_sine[..(in_frames) * 2].to_vec();
    render_with_tail(&mut keylocked(kind, speed), &input)
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn run_vinyl(warp_sine: &[f32], kind: StretchKind, speed: f32, in_frames: usize) -> Vec<f32> {
    let input = warp_sine[..(in_frames) * 2].to_vec();
    render(&mut vinyl(kind, speed), &input)
}

/// Slowed playback -> stretched exact timeline, pitch held.
/// Shared across every compiled-in backend.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn assert_half_speed_contract(
    warp_sine: &[f32],
    kind: StretchKind,
    speed: f32,
    expected_timeline: usize,
) {
    let channels = usize::from(consts::CH);
    let in_frames = usize::try_from(consts::SR).unwrap() * 2; // 2 s
    let (out, tail_frames) = run_keylocked_with_tail(warp_sine, kind, speed, in_frames);
    let out_frames = out.len() / channels;

    assert_eq!(
        out_frames, expected_timeline,
        "{kind:?}: exact timeline at speed {speed}, terminal drain included"
    );
    assert!(tail_frames > 0, "{kind:?}: terminal history is drained");

    // Pitch preserved: dominant bin still at F0 (the load-bearing check -
    // a resampler-in-disguise would shift it).
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= consts::N,
        "{kind:?}: not enough output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(consts::F0);
    assert!(
        peak.abs_diff(want) <= 3,
        "{kind:?}: pitch moved under time-stretch: peak bin {peak}, expected {want}"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn assert_unity_contract(warp_sine: &[f32], kind: StretchKind) {
    let in_frames = usize::try_from(consts::SR).unwrap() * 2;
    let input = warp_sine[..(in_frames) * 2].to_vec();
    let out = render(&mut keylocked(kind, 1.0), &input);
    assert_eq!(out, input, "{kind:?}: unity speed must bypass byte-exact");
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn exact_timeline_and_unity_contracts(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let in_frames = usize::try_from(consts::SR).unwrap() * 2;
    assert_half_speed_contract(&warp_sine, backend, 0.5, in_frames * 2);
    assert_half_speed_contract(&warp_sine, backend, 0.75, in_frames * 4 / 3);
    assert_unity_contract(&warp_sine, backend);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_frontier_excludes_backend_lookahead(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    const SOURCE_START: u64 = 10_000;
    const SOURCE_FRAMES: usize = 16_384;

    let mut renderer = keylocked(backend, 0.5);
    let pools = renderer.pools.clone();
    let source = warp_sine[..(SOURCE_FRAMES) * 2].to_vec();
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
        .first();
    assert!(source_latency > 0, "backend must declare source lookahead");

    let outputs = LaneFeed::default().feed(&mut renderer, input);
    let output = outputs.last().expect("half-speed render emits samples");
    let expected_frame = output.meta.source_span.expect("emitted mapping").end();

    assert_eq!(
        renderer.rendered_source_end(),
        Some((
            expected_frame,
            NonZero::new(consts::SR).expect("test sample rate is non-zero"),
        )),
        "rendered progress is the source end of the emitted PCM"
    );
    assert!(
        expected_frame + u64::try_from(source_latency).expect("latency fits u64") <= admitted,
        "rendered progress excludes source still retained by the backend"
    );
    let output_frames = outputs.iter().map(AudioChunk::frames).sum::<usize>();
    assert_ne!(
        SOURCE_START.saturating_add(u64::try_from(output_frames).expect("output frames fit u64")),
        expected_frame,
        "the oracle must distinguish transformed output frames from the source frontier"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_frontier_reaches_end_only_on_completed_drain(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    const SOURCE_START: u64 = 10_000;

    let mut renderer = keylocked(backend, kithara_sync::MIN_SPEED);
    let source_frames = renderer.source_block_frames.get();
    let pools = renderer.pools.clone();
    let source = warp_sine[..(source_frames) * 2].to_vec();
    let mut input = chunk(&pools, &source);
    input.meta.frame_offset = SOURCE_START;
    let admitted = SOURCE_START
        .checked_add(u64::try_from(source_frames).expect("source frame count fits u64"))
        .expect("source frontier fits u64");
    let source_latency = renderer
        .engine
        .as_ref()
        .expect("compiled backend is available")
        .capabilities()
        .latency()
        .first();
    let mut feed = LaneFeed::default();
    let mut outputs = feed.feed(&mut renderer, input);
    outputs.extend(feed.end(&mut renderer));
    let rendered = outputs
        .last()
        .expect("minimum-speed render emits samples")
        .meta
        .source_span
        .expect("emitted mapping")
        .end();
    assert!(rendered < admitted, "the backend still holds source");
    assert_eq!(
        renderer.rendered_source_end(),
        Some((rendered, spec().sample_rate))
    );

    let mut frontiers = Vec::new();
    loop {
        renderer.prepare(spec());
        let Some(tail) = renderer
            .drain((source_latency / 2).max(1))
            .expect("bounded terminal drain")
        else {
            break;
        };
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
#[kithara::hang_watchdog]
fn output_meta_preserves_decoder_timeline(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let channels = usize::from(consts::CH);
    let mut fx = keylocked(backend, 0.5);
    let pools = fx.pools.clone();
    let cf = 1024usize;
    let block = warp_sine[..(cf) * 2].to_vec();
    let mut feed = LaneFeed::default();
    let mut emitted = Vec::new();
    let mut keep = |outputs: Vec<AudioChunk>| {
        emitted.extend(
            outputs
                .into_iter()
                .map(|output| (output.meta, output.samples.len())),
        );
    };
    for i in 0..40u64 {
        hang_tick!();
        let mut c = chunk(&pools, &block);
        let frame = i * u64::try_from(cf).unwrap();
        c.meta.timestamp = spec().duration_for(frame).expect("test timestamp fits");
        c.meta.end_timestamp = spec()
            .duration_for(frame + u64::try_from(cf).unwrap())
            .expect("test timestamp fits");
        c.meta.frame_offset = frame;
        keep(feed.feed(&mut fx, c));
        hang_reset!();
    }
    keep(feed.end(&mut fx));
    keep(drain_tail(&mut fx));
    let fed_end = spec()
        .duration_for(40 * u64::try_from(cf).unwrap())
        .expect("test end fits");
    assert!(!emitted.is_empty(), "stretch produced no output");
    for (meta, samples) in &emitted {
        assert_eq!(
            meta.spec,
            AudioSpec {
                channels: consts::CH,
                sample_rate: NonZero::new(consts::SR).unwrap()
            },
            "spec (incl. sample rate) preserved verbatim"
        );
        assert_eq!(
            usize::try_from(meta.frames).unwrap(),
            samples / channels,
            "frames recomputed to the actual output count"
        );
        let span = meta.source_span.expect("output source mapping");
        assert_eq!(
            (meta.timestamp, meta.end_timestamp),
            (
                span.position_at(0).expect("span start"),
                span.position_at(span.output_frames()).expect("span end")
            ),
            "timestamps are the source-track time of the rendered span"
        );
        assert!(
            meta.end_timestamp <= fed_end,
            "output time stays on the decoded timeline"
        );
    }
}

/// Key-lock on above unity: speed shortens duration while pitch holds.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn keylocked_double_speed_preserves_pitch(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let channels = usize::from(consts::CH);
    let in_frames = usize::try_from(consts::SR).unwrap() * 2;
    let (out, _tail) = run_keylocked_with_tail(&warp_sine, backend, 2.0, in_frames);
    let out_frames = out.len() / channels;
    assert!(
        out_frames * 10 >= in_frames * 4 && out_frames * 10 <= in_frames * 6,
        "key-locked 2x should roughly halve duration, got {out_frames} from {in_frames}"
    );

    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= consts::N,
        "not enough key-locked output for the pitch window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(consts::F0);
    assert!(
        peak.abs_diff(want) <= 3,
        "key-locked playback rate shifted pitch: peak bin {peak}, expected {want}"
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
/// Key-lock off is vinyl mode: speed changes duration and pitch in the
/// stretch slot, with no resampler-rate handoff.
#[kithara::test]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn vinyl_speed_scales_duration_and_pitch(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let channels = usize::from(consts::CH);
    let in_frames = usize::try_from(consts::SR).unwrap() * 2;
    let out = run_vinyl(&warp_sine, backend, 2.0, in_frames);
    let out_frames = out.len() / channels;
    assert!(
        out_frames * 10 >= in_frames * 4 && out_frames * 10 <= in_frames * 6,
        "vinyl 2x should roughly halve duration, got {out_frames} from {in_frames}"
    );
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= consts::N,
        "not enough vinyl output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(consts::F0 * 2.0);
    assert!(
        peak.abs_diff(want) <= 4,
        "vinyl pitch did not follow speed: peak bin {peak}, expected {want}"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
#[kithara::hang_watchdog]
fn live_speed_change_updates_stretch_duration(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .pitch(Pitch::HELD)
            .backend(backend)
            .build(),
    );
    let pools = fx.pools.clone();
    let block = warp_sine[..(4096) * 2].to_vec();
    let mut feed = LaneFeed::default();
    let unity = feed.feed(&mut fx, chunk(&pools, &block));
    assert_eq!(concat(&unity), block, "unity phase bypasses");

    fx.set_speed(SpeedCurve::Constant(0.5), 1)
        .expect("valid speed");
    let mut stretched: Vec<f32> = Vec::new();
    let mut source = 4096;
    for _ in 0..24 {
        hang_tick!();
        let mut input = chunk(&pools, &block);
        input.meta.frame_offset = source;
        source += u64::try_from(input.frames()).expect("source frames fit u64");
        stretched.extend(concat(&feed.feed(&mut fx, input)));
        hang_reset!();
    }
    stretched.extend(concat(&feed.end(&mut fx)));
    stretched.extend(concat(&drain_tail(&mut fx)));
    assert!(
        stretched.len() > block.len() * 24,
        "half-speed key-lock should lengthen output after a live speed change"
    );
}

/// Flipping key-lock mid-stream switches from vinyl pitch shift to
/// pitch-preserving stretch - no reload.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
#[kithara::hang_watchdog]
fn live_keylock_toggle_switches_pitch_mode(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let mut fx = renderer(&WarpConfig::builder().speed(0.5).backend(backend).build());
    let pools = fx.pools.clone();
    let block = warp_sine[..(4096) * 2].to_vec();

    let mut vinyl_out: Vec<f32> = Vec::new();
    let mut feed = LaneFeed::default();
    let mut source = 0;
    for _ in 0..24 {
        hang_tick!();
        let mut input = chunk(&pools, &block);
        input.meta.frame_offset = source;
        source += u64::try_from(input.frames()).expect("source frames fit u64");
        vinyl_out.extend(concat(&feed.feed(&mut fx, input)));
        hang_reset!();
    }
    let vinyl_mono: Vec<f32> = vinyl_out
        .iter()
        .step_by(usize::from(consts::CH))
        .copied()
        .collect();
    assert!(
        dominant_bin(&vinyl_mono).abs_diff(expected_bin(consts::F0 * 0.5)) <= 4,
        "off: vinyl pitch follows speed"
    );

    fx.set_pitch(Pitch::HELD);
    let mut stretched: Vec<f32> = Vec::new();
    for _ in 0..24 {
        hang_tick!();
        let mut input = chunk(&pools, &block);
        input.meta.frame_offset = source;
        source += u64::try_from(input.frames()).expect("source frames fit u64");
        stretched.extend(concat(&feed.feed(&mut fx, input)));
        hang_reset!();
    }
    stretched.extend(concat(&feed.end(&mut fx)));
    stretched.extend(concat(&drain_tail(&mut fx)));
    let mono: Vec<f32> = stretched
        .iter()
        .step_by(usize::from(consts::CH))
        .copied()
        .collect();
    assert!(
        mono.len() >= consts::N,
        "on: not enough output for the FFT window"
    );
    assert!(
        dominant_bin(&mono).abs_diff(expected_bin(consts::F0)) <= 3,
        "on: pitch preserved after live toggle"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod transpose {
    use kithara_signal::AudioChunk;
    use kithara_stretch::StretchKind;
    use kithara_test_fixtures::unit_fixtures::warp_sine;
    use kithara_test_utils::kithara;

    use super::{
        super::{
            LaneFeed, WarpRenderer, chunk, dominant_bin, expected_bin, f64_of, renderer, samples,
        },
        render,
    };
    use crate::{Pitch, Semitones, WarpConfig, consts};

    fn semitones(value: i8) -> Semitones {
        Semitones::checked(value).expect("fixture shift is in range")
    }

    fn transposed(backend: StretchKind, speed: f32, keylock: bool, transpose: i8) -> WarpConfig {
        let shift = semitones(transpose);
        WarpConfig::builder()
            .speed(speed)
            .pitch(if keylock {
                Pitch::Held { shift }
            } else {
                Pitch::Vinyl { shift }
            })
            .backend(backend)
            .build()
    }

    #[kithara::hang_watchdog]
    fn play(
        fx: &mut WarpRenderer,
        feed: &mut LaneFeed,
        input: &[f32],
        source: &mut u64,
        stop: u64,
    ) -> Vec<AudioChunk> {
        let channels = usize::from(consts::CH);
        let pools = fx.pools.clone();
        let stop = usize::try_from(stop).expect("source frame fits usize");
        let mut chunks = Vec::new();
        let mut start = usize::try_from(*source).expect("source frame fits usize");
        while start < stop {
            hang_tick!();
            let end = (start + 4096).min(stop);
            let mut decoded = chunk(&pools, &input[start * channels..end * channels]);
            decoded.meta.frame_offset = *source;
            chunks.extend(feed.feed(fx, decoded));
            *source = u64::try_from(end).expect("source frame fits u64");
            start = end;
            hang_reset!();
        }
        chunks
    }

    fn assert_peak(out: &[f32], freq: f64, context: &str) {
        let mono: Vec<f32> = out
            .iter()
            .step_by(usize::from(consts::CH))
            .copied()
            .collect();
        assert!(mono.len() >= consts::N, "{context}: not enough output");
        let peak = dominant_bin(&mono);
        let want = expected_bin(freq);
        assert!(
            peak.abs_diff(want) <= 3,
            "{context}: peak bin {peak}, expected {want} ({freq:.1} Hz)"
        );
    }

    fn shift(semitones: i8) -> f64 {
        (f64::from(semitones) / 12.0).exp2()
    }

    fn position(chunk: &AudioChunk, frame: u64) -> f64 {
        let (numerator, denominator) = chunk
            .meta
            .source_span
            .and_then(|span| span.source_ratio_at(frame))
            .expect("mapped output position");
        num_traits::cast::<u128, f64>(numerator).expect("numerator fits f64")
            / num_traits::cast::<u128, f64>(denominator.get()).expect("denominator fits f64")
    }

    fn source_end(chunk: &AudioChunk) -> f64 {
        position(
            chunk,
            u64::try_from(chunk.frames()).expect("frames fit u64"),
        )
    }

    /// A steady pitch sounds at its factor of the source pitch, the speed
    /// included only without keylock, and the timeline keeps the source's
    /// length at the speed. At unity a shift leaves the bypass.
    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_keylock(StretchKind::Signalsmith, true, 1.08, 3)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_vinyl(StretchKind::Signalsmith, false, 1.08, -2)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_keylock_unity(StretchKind::Signalsmith, true, 1.0, 12)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_vinyl_unity(StretchKind::Signalsmith, false, 1.0, 12)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_keylock(StretchKind::Bungee, true, 1.08, 3)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_vinyl(StretchKind::Bungee, false, 1.08, -2)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_keylock_unity(StretchKind::Bungee, true, 1.0, 12)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_vinyl_unity(StretchKind::Bungee, false, 1.0, 12)
    )]
    fn a_steady_pitch_sounds_at_its_factor(
        #[case] backend: StretchKind,
        #[case] keylock: bool,
        #[case] speed: f32,
        #[case] transpose: i8,
        warp_sine: Vec<f32>,
    ) {
        let config = transposed(backend, speed, keylock, transpose);
        let mut fx = renderer(&config);
        let chunks = play(
            &mut fx,
            &mut LaneFeed::default(),
            &warp_sine,
            &mut 0,
            u64::from(consts::SR) * 2,
        );
        let timeline: usize = chunks.iter().map(AudioChunk::frames).sum();
        let covered = source_end(chunks.last().expect("rendered output"))
            - position(chunks.first().expect("rendered output"), 0);
        assert!(timeline > 0);
        assert!(
            (covered - f64_of(timeline) * f64::from(speed)).abs() < 1e-6,
            "{timeline} output frames at {speed} cover {covered} source frames"
        );
        let context = format!("keylock {keylock}, speed {speed}, {transpose:+}");
        assert_peak(
            &samples(&chunks),
            consts::F0 * config.pitch().factor(f64::from(speed)),
            &context,
        );
    }

    /// An octave up at a speed well below the deck's 0.5 floor fits the
    /// projected stages: the shift renders rather than failing on the render path.
    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn an_octave_up_at_a_slow_speed_fits_the_projection(
        #[case] backend: StretchKind,
        warp_sine: Vec<f32>,
    ) {
        let input = &warp_sine[..usize::try_from(consts::SR).unwrap() * 2];
        let out = render(&mut renderer(&transposed(backend, 0.3, true, 12)), input);
        assert_peak(&out, consts::F0 * 2.0, "keylock on, 0.3, +12");
    }

    /// Renders two seconds at `config`, applies `change` live and renders four
    /// more: the pitch moves from `before` to `after` while the source position
    /// runs on without a jump.
    fn assert_live_change(
        warp_sine: &[f32],
        config: &WarpConfig,
        before: f64,
        change: impl FnOnce(&mut WarpRenderer),
        after: f64,
    ) {
        let rate = u64::from(consts::SR);
        let mut fx = renderer(config);
        let mut feed = LaneFeed::default();
        let mut source = 0;
        let first = play(&mut fx, &mut feed, warp_sine, &mut source, rate * 2);
        assert_peak(&samples(&first), before, "before the change");

        change(&mut fx);
        let second = play(&mut fx, &mut feed, warp_sine, &mut source, rate * 6);
        assert_peak(&samples(&second), after, "after the change");

        for (previous, next) in first
            .iter()
            .chain(&second)
            .zip(first.iter().chain(&second).skip(1))
        {
            let (end, start) = (source_end(previous), position(next, 0));
            assert!(
                (end - start).abs() < 1e-6,
                "the position jumps from {end} to {start}"
            );
        }
    }

    /// A live pitch change moves the pitch to the new pitch's factor while the
    /// source position runs on without a jump: a shift with keylock off,
    /// keylock off under a shift, and a shift under keylock.
    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_vinyl_shift(StretchKind::Signalsmith, false, 0, Pitch::Vinyl { shift: semitones(5) })
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_unlock(StretchKind::Signalsmith, true, 3, Pitch::Vinyl { shift: semitones(3) })
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_keylock_shift(StretchKind::Signalsmith, true, 3, Pitch::Held { shift: semitones(4) })
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_vinyl_shift(StretchKind::Bungee, false, 0, Pitch::Vinyl { shift: semitones(5) })
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_unlock(StretchKind::Bungee, true, 3, Pitch::Vinyl { shift: semitones(3) })
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_keylock_shift(StretchKind::Bungee, true, 3, Pitch::Held { shift: semitones(4) })
    )]
    fn a_live_pitch_change_moves_the_pitch_and_keeps_the_position(
        #[case] backend: StretchKind,
        #[case] keylock: bool,
        #[case] transpose: i8,
        #[case] pitch: Pitch,
        warp_sine: Vec<f32>,
    ) {
        let speed = f64::from(1.08_f32);
        let config = transposed(backend, 1.08, keylock, transpose);
        assert_live_change(
            &warp_sine,
            &config,
            consts::F0 * config.pitch().factor(speed),
            |fx| fx.set_pitch(pitch),
            consts::F0 * pitch.factor(speed),
        );
    }

    /// Back to transpose 0 at unity speed the pitch returns to the source's and
    /// the position runs on through the bypass.
    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_keylock(StretchKind::Signalsmith, true)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_vinyl(StretchKind::Signalsmith, false)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_keylock(StretchKind::Bungee, true)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_vinyl(StretchKind::Bungee, false)
    )]
    fn a_live_return_to_no_transpose_at_unity_reenters_the_bypass(
        #[case] backend: StretchKind,
        #[case] keylock: bool,
        warp_sine: Vec<f32>,
    ) {
        assert_live_change(
            &warp_sine,
            &transposed(backend, 1.0, keylock, 3),
            consts::F0 * shift(3),
            |fx| {
                fx.set_pitch(if keylock {
                    Pitch::HELD
                } else {
                    Pitch::default()
                });
            },
            consts::F0,
        );
    }
}
