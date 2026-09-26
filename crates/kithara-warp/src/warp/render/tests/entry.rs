use std::num::{NonZero, NonZeroU32};

use kithara_signal::AudioChunk;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_stretch::StretchKind;

use super::*;
use crate::{WarpPlan, WarpRenderError, consts, test_grids};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn entered_plan(host_rate: NonZeroU32) -> WarpPlan {
    let source = test_grids::asset_grid(120.0, spec().sample_rate);
    let target = test_grids::session_grid(100.0, host_rate);
    let output = test_grids::beat_frames(100.0, host_rate) * consts::CUE_BEAT;
    test_grids::plan_over_at(
        source,
        target,
        consts::CUE_BEAT,
        consts::CUE_BEAT,
        SessionFrame::new(num_traits::cast(output).expect("fixture output frame")),
    )
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn entered_renderer(plan: WarpPlan, backend: StretchKind, keylock: bool) -> WarpRenderer {
    let controls = StretchControls::new(1.0);
    controls.set_backend(backend);
    controls.set_keylock(keylock);
    let config = WarpConfig::builder().stretch(controls).build();
    let entered = config.entering(Arc::new(plan));
    let mut renderer = Warp::new((), &entered).renderer(spec(), pools());
    renderer.prepare(spec());
    renderer
}

fn source_span(renderer: &WarpRenderer, start: u64, frames: usize) -> AudioChunk {
    let samples: Vec<f32> = (0..frames)
        .flat_map(|frame| {
            let value = f32::from(u16::try_from((start as usize + frame) % 97).unwrap_or(0));
            [value / 97.0, -value / 97.0]
        })
        .collect();
    let mut input = chunk(&renderer.pools, &samples);
    input.meta.frame_offset = start;
    input
}

/// Admits the exact engine history an entered plan names before its cue.
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn admit_history(renderer: &mut WarpRenderer, entry: u64, cue: u64) {
    let history = usize::try_from(cue - entry).expect("history fits usize");
    if history == 0 {
        return;
    }
    let landing = source_span(renderer, entry, history + 256);
    let preroll = renderer.prepare_quantum(landing.meta, landing.frames());
    let Err(WarpRenderError::Preroll { frames }) = preroll else {
        panic!("audio before the activation is history, got {preroll:?}");
    };
    assert_eq!(frames.get(), history);
    let (head, _) = landing.samples.split_at(history * usize::from(consts::CH));
    let mut head_meta = landing.meta;
    head_meta.frames = u32::try_from(history).expect("history fits u32");
    renderer
        .admit_preroll(head_meta, head)
        .expect("history continues from the entry source");
}

#[kithara::test]
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
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
fn an_entered_plan_presents_its_activation_source_after_exact_history(
    #[case] host_rate: u32,
    #[case] backend: StretchKind,
    #[case] keylock: bool,
) {
    let host_rate = NonZeroU32::new(host_rate).expect("fixture host rate");
    let plan = entered_plan(host_rate);
    let activation = plan.activation();
    let revision = activation.revision();
    let cue = activation.source();
    let mut renderer = entered_renderer(plan, backend, keylock);

    let entry = renderer
        .entry_source()
        .expect("an entered renderer names its first source frame");
    assert_eq!(
        entry < cue,
        keylock,
        "only a keylocked engine needs history before the activation"
    );
    admit_history(&mut renderer, entry, cue);

    let mut position = cue;
    let output = loop {
        assert!(
            position < cue + 16 * 1024,
            "entered PCM must appear within the engine's warm-up"
        );
        renderer.prepare(spec());
        let at = source_span(&renderer, position, 1024);
        let frames = renderer
            .prepare_quantum(at.meta, at.frames())
            .expect("the entered source continues")
            .get();
        let mut input = source_span(&renderer, position, frames);
        input.meta.frames = u32::try_from(frames).expect("span fits u32");
        position += u64::try_from(frames).expect("span fits u64");
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
        output.meta.mapping_revision.map(NonZero::get),
        Some(u64::from(revision)),
        "entered PCM carries the plan's map"
    );
    assert!(output.frames() > 0);
}

#[kithara::test]
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn a_one_frame_decoder_chunk_keeps_the_slowed_projection_presenting(#[case] backend: StretchKind) {
    let plan = entered_plan(spec().sample_rate);
    let cue = plan.activation().source();
    let mut renderer = entered_renderer(plan, backend, true);
    let entry = renderer
        .entry_source()
        .expect("an entered renderer names its first source frame");
    admit_history(&mut renderer, entry, cue);

    let mut position = cue;
    let mut audible = None;
    for chunk_frames in consts::ALTERNATING_CHUNKS
        .iter()
        .cycle()
        .take(consts::ALTERNATING_CHUNKS.len() * consts::CHUNK_PAIRS)
    {
        let mut remaining = *chunk_frames;
        while remaining > 0 {
            renderer.prepare(spec());
            let at = source_span(&renderer, position, remaining);
            let frames = renderer
                .prepare_quantum(at.meta, remaining)
                .expect("the entered source continues")
                .get();
            let mut input = source_span(&renderer, position, frames);
            input.meta.frames = u32::try_from(frames).expect("span fits u32");
            if let Some(output) = renderer
                .render_quantum(input)
                .continue_value()
                .expect("prepared source shape")
            {
                audible = Some(output.meta.frame_offset);
            }
            position += u64::try_from(frames).expect("span fits u64");
            remaining -= frames;
        }
    }
    let audible = audible.expect("the slowed projection presents PCM");
    assert!(
        audible + consts::LAG_FRAMES >= position,
        "the audible source stalled at {audible} while {position} was decoded"
    );
}

#[kithara::test]
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith, true)
)]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide, false))]
fn an_entered_plan_refuses_a_landing_after_its_activation_source(
    #[case] backend: StretchKind,
    #[case] keylock: bool,
) {
    let plan = entered_plan(spec().sample_rate);
    let cue = plan.activation().source();
    let mut renderer = entered_renderer(plan, backend, keylock);
    let late = source_span(&renderer, cue + 1, 256);
    let refused = renderer.prepare_quantum(late.meta, late.frames());
    assert!(
        matches!(
            refused,
            Err(WarpRenderError::Engine(
                kithara_stretch::ElasticError::DiscontinuousSource { .. }
            ))
        ),
        "a landing after the cue can never present its first frame, got {refused:?}"
    );
}

#[kithara::test]
fn a_renderer_without_an_entered_plan_names_no_entry_source() {
    let (mut renderer, _) = projection::planned_renderer(StretchControls::new(1.0));
    renderer.prepare(spec());
    assert_eq!(renderer.entry_source(), None);
    let input = source_span(&renderer, 0, 16);
    assert!(matches!(
        renderer.admit_preroll(input.meta, &input.samples),
        Err(WarpRenderError::UnsupportedProjection)
    ));
}
