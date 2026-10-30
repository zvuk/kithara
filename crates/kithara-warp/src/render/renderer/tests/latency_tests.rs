use kithara_signal::FrameCount;
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
