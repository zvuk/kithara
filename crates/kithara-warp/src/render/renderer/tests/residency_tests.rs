use std::num::NonZeroUsize;

use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;

use super::*;
use crate::{
    GridSegment, RegionPlan, Warp,
    test_pools::{TestPools, pools, sample_buffer},
};

fn input(renderer: &WarpRenderer<TestPools>, source: u64, frames: usize) -> AudioChunk {
    AudioChunk::new(
        AudioChunkInfo {
            spec: renderer.spec,
            frame_offset: source,
            frames: u32::try_from(frames).expect("test input frames"),
            ..Default::default()
        },
        sample_buffer(
            &renderer.pools,
            &vec![0.25; frames * usize::from(renderer.spec.channels)],
        ),
    )
}

#[kithara::test]
#[kithara::hang_watchdog]
#[cfg(feature = "stretch-signalsmith")]
fn keylock_reprime_retains_projected_source_history() {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Glide)
        .speed(4.0)
        .region_plan(kithara_platform::sync::Arc::new(
            RegionPlan::new(vec![GridSegment::new(0, u64::MAX, 0.25)]).expect("region"),
        ))
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    for _ in 0..128 {
        hang_tick!();
        quantum(&mut renderer, &mut source);
        hang_reset!();
    }
    renderer.set_backend(StretchKind::Signalsmith);
    renderer.set_keylock(true);
    renderer
        .set_speed(SpeedCurve::Constant(0.05), 2)
        .expect("speed");
    for _ in 0..4 {
        hang_tick!();
        let output = quantum(&mut renderer, &mut source);
        assert_eq!(output.frames(), 128);
        hang_reset!();
    }
}

fn quantum(renderer: &mut WarpRenderer<TestPools>, source: &mut u64) -> AudioChunk {
    renderer
        .prepare_engine_latency(renderer.spec)
        .expect("renderer service");
    let frames = renderer
        .prepare_source_window(
            AudioChunkInfo {
                spec: renderer.spec,
                frame_offset: *source,
                ..Default::default()
            },
            65_536,
            128,
        )
        .expect("source quantum")
        .get();
    let chunk = input(renderer, *source, frames);
    *source += u64::try_from(frames).expect("source frames");
    let output = renderer
        .render_quantum(chunk)
        .continue_value()
        .expect("prepared input")
        .expect("a resident mapping keeps producing audio");
    assert!(output.samples.iter().any(|sample| sample.abs() > 0.01));
    output
}

#[kithara::test]
#[kithara::hang_watchdog]
#[case::bungee_to_signalsmith(StretchKind::Bungee, StretchKind::Signalsmith)]
#[case::glide_to_signalsmith(StretchKind::Glide, StretchKind::Signalsmith)]
#[case::signalsmith_to_bungee(StretchKind::Signalsmith, StretchKind::Bungee)]
#[case::glide_to_bungee(StretchKind::Glide, StretchKind::Bungee)]
fn retiring_keylock_admits_both_engines_source_packets(
    #[case] backend: StretchKind,
    #[case] next: StretchKind,
) {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    for block in 0..96 {
        hang_tick!();
        if block == 8 {
            renderer
                .set_speed(SpeedCurve::Constant(1.25), 1)
                .expect("speed");
        }
        if block == 32 {
            renderer.set_backend(next);
        }
        let output = quantum(&mut renderer, &mut source);
        assert_eq!(output.frames(), 128);
        hang_reset!();
    }
}

#[kithara::test]
#[cfg(feature = "stretch-glide")]
fn complete_unity_chunks_advance_the_next_prepared_mapping() {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Glide)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    quantum(&mut renderer, &mut source);
    for _ in 0..64 {
        renderer.prepare(spec);
        let mut chunk = input(&renderer, source, 4096);
        let span = kithara_signal::SourceSpan::new(source, source + 4096, spec.sample_rate, 4096)
            .expect("producer mapping")
            .with_render_revision(7);
        chunk.meta.source_span = Some(span);
        source += 4096;
        let output = renderer
            .render(chunk)
            .continue_value()
            .expect("complete unity input")
            .expect("unity audio");
        assert!(output.samples.iter().all(|sample| *sample == 0.25));
        assert_eq!(output.meta.source_span, Some(span));
    }
    renderer
        .set_speed(SpeedCurve::Constant(1.25), 1)
        .expect("speed");
    for _ in 0..4 {
        let output = quantum(&mut renderer, &mut source);
        assert_eq!(output.frames(), 128);
    }
}

#[kithara::test]
#[kithara::hang_watchdog]
#[cfg(feature = "stretch-signalsmith")]
fn a_new_source_origin_rebases_the_prepared_unity_mapping() {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Signalsmith)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 2_806_954;
    for _ in 0..8 {
        hang_tick!();
        quantum(&mut renderer, &mut source);
        hang_reset!();
    }
    source += 1_024;
    let meta = AudioChunkInfo {
        spec,
        frame_offset: source,
        ..Default::default()
    };
    match renderer.prepare_source_window(meta, 65_536, 128) {
        Ok(_) => {}
        Err(crate::WarpRenderError::NeedsService) => {
            renderer
                .prepare_engine_latency(spec)
                .expect("source rebase service");
        }
        Err(error) => panic!("source rebase admission: {error}"),
    }
    let output = quantum(&mut renderer, &mut source);
    assert_eq!(output.frames(), 128);
    assert_eq!(
        output.meta.source_span.expect("rebased mapping").start(),
        meta.frame_offset
    );
}

#[kithara::test]
#[kithara::hang_watchdog]
#[cfg(feature = "stretch-glide")]
#[case::evicted_history(0)]
#[case::unresident_future(1_000_000)]
fn lost_source_history_terminates_until_an_explicit_reset(#[case] start: u64) {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Glide)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    quantum(&mut renderer, &mut source);
    for _ in 0..64 {
        hang_tick!();
        let chunk = input(&renderer, source, 4096);
        renderer
            .render(chunk)
            .continue_value()
            .expect("whole input");
        source += 4096;
        hang_reset!();
    }
    renderer
        .set_speed(SpeedCurve::Constant(1.25), 1)
        .expect("speed");
    renderer.prepare_engine_latency(spec).expect("service");
    let meta = AudioChunkInfo {
        spec,
        frame_offset: source,
        ..Default::default()
    };
    let frames = renderer
        .prepare_source_window(meta, 65_536, 128)
        .expect("quantum")
        .get();
    renderer
        .prepared_quantum
        .as_mut()
        .expect("prepared mapping")
        .source_span = Some(
        kithara_signal::SourceSpan::new(start, start + 160, spec.sample_rate, 128)
            .expect("lost mapping"),
    );
    let chunk = input(&renderer, source, frames);
    renderer
        .render_quantum(chunk)
        .continue_value()
        .expect("prepared input");
    assert!(
        renderer.prepare_source_window(meta, 65_536, 128).is_err(),
        "lost history is terminal"
    );
    assert!(
        renderer.drain(128).is_err(),
        "a terminal renderer cannot drain"
    );
    assert!(
        !renderer.accepts_input(),
        "a terminal renderer cannot accept input"
    );
    renderer.reset();
    quantum(&mut renderer, &mut source);
}
