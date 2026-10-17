use std::num::NonZeroU32;

use kithara_signal::AudioChunk;
use kithara_stretch::StretchKind;
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{WarpRenderer, chunk, spec};
use crate::{Warp, WarpConfig, consts, test_pools::pools};

const CUE_BEAT: f64 = 4.0;

#[kithara::hang_watchdog]
fn entered_renderer(
    host_rate: NonZeroU32,
    backend: StretchKind,
    keylock: bool,
) -> (WarpRenderer, u64, u64) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(keylock)
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer.prepare(spec());
    let cue = (f64::from(consts::SR) * 60.0 / 120.0 * CUE_BEAT)
        .to_u64()
        .expect("cue frame");
    let history = if keylock {
        renderer
            .engine
            .as_ref()
            .expect("prepared engine")
            .capabilities()
            .latency()
            .first()
    } else {
        0
    };
    let entry = cue
        .checked_sub(u64::try_from(history).expect("history frames"))
        .expect("cue leaves exact history");
    if history > 0 {
        let input = source_span(&renderer, entry, history);
        let mut position = entry;
        while position < cue {
            hang_tick!();
            let offset = usize::try_from(position - entry).expect("history offset");
            renderer.prepare(spec());
            let mut meta = input.meta;
            meta.frame_offset = position;
            let frames = renderer
                .prepare_source_window(meta, history - offset, usize::MAX)
                .expect("history quantum")
                .get();
            let source = source_span(&renderer, position, frames);
            let _ = renderer
                .render_quantum(source)
                .continue_value()
                .expect("admit exact history");
            position += u64::try_from(frames).expect("history quantum frames");
            if frames > 0 {
                hang_reset!();
            }
        }
        assert_eq!(
            renderer.rendered_source_end(),
            Some((cue, spec().sample_rate))
        );
    }
    let speed = (100.0 / 120.0 * f64::from(consts::SR) / f64::from(host_rate.get()))
        .to_f32()
        .expect("entry speed");
    renderer
        .set_speed(SpeedCurve::Constant(speed), 1)
        .expect("entry trajectory");
    (renderer, entry, cue)
}

fn source_span(renderer: &WarpRenderer, start: u64, frames: usize) -> AudioChunk {
    let samples: Vec<f32> = (start..)
        .take(frames)
        .flat_map(|frame| {
            let value = f32::from(u16::try_from(frame % 97).expect("signal modulus"));
            [value / 97.0, -value / 97.0]
        })
        .collect();
    let mut input = chunk(&renderer.pools, &samples);
    input.meta.frame_offset = start;
    input
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_keylocked(consts::SR, StretchKind::Signalsmith, true)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_keylocked_host_rate_differs(48_000, StretchKind::Signalsmith, true)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_varispeed(consts::SR, StretchKind::Signalsmith, false)
)]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide(consts::SR, StretchKind::Glide, false)
)]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide_host_rate_differs(48_000, StretchKind::Glide, false)
)]
#[kithara::hang_watchdog]
fn an_entered_plan_presents_its_activation_source_after_exact_history(
    #[case] host_rate: u32,
    #[case] backend: StretchKind,
    #[case] keylock: bool,
) {
    let host_rate = NonZeroU32::new(host_rate).expect("fixture host rate");
    let (mut renderer, entry, cue) = entered_renderer(host_rate, backend, keylock);
    assert_eq!(
        entry < cue,
        keylock,
        "only a keylocked engine needs history before the activation"
    );
    let mut position = cue;
    let output = loop {
        hang_tick!();
        assert!(
            position < cue + 16 * 1024,
            "entered PCM must appear within the engine's warm-up"
        );
        renderer.prepare(spec());
        let at = source_span(&renderer, position, 1024);
        let frames = renderer
            .prepare_source_window(at.meta, at.frames(), usize::MAX)
            .expect("the entered source continues")
            .get();
        let mut input = source_span(&renderer, position, frames);
        input.meta.frames = u32::try_from(frames).expect("span fits u32");
        position += u64::try_from(frames).expect("span fits u64");
        if frames > 0 {
            hang_reset!();
        }
        if let Some(output) = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape")
        {
            break output;
        }
    };
    assert_eq!(output.meta.frame_offset, cue);
    assert_eq!(
        output.meta.render_revision, 1,
        "entered PCM carries the effective trajectory"
    );
    assert_eq!(
        output
            .meta
            .source_span
            .expect("entered source mapping")
            .render_revision(),
        1
    );
    assert!(output.frames() > 0);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith, true)
)]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide, false))]
fn an_entered_plan_refuses_a_landing_after_its_activation_source(
    #[case] backend: StretchKind,
    #[case] keylock: bool,
) {
    let (mut renderer, _, cue) = entered_renderer(spec().sample_rate, backend, keylock);
    renderer.prepare(spec());
    let expected = source_span(&renderer, cue, 256);
    let frames = renderer
        .prepare_source_window(expected.meta, expected.frames(), usize::MAX)
        .expect("entry quantum")
        .get();
    let late = source_span(&renderer, cue + 1, frames);
    let refused = renderer.render_quantum(late);
    let refused = refused
        .break_value()
        .expect("a landing after the cue can never present its first frame");
    assert_eq!(refused.meta.frame_offset, cue + 1);
    assert_eq!(refused.frames(), frames);
}
