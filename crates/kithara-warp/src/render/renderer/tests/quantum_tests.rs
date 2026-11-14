use std::num::NonZeroUsize;

use kithara_test_utils::kithara;

use super::*;
use crate::{
    Pitch, Warp,
    test_pools::{TestPools, pools},
};

/// The lane packet the fixtures render: the render quantum, below the source block.
const PACKET: usize = 512;

fn unity_renderer(backend: StretchKind) -> WarpRenderer<TestPools> {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(1.0)
        .render_quantum_frames(NonZeroUsize::new(PACKET).expect("quantum"))
        .build();
    let renderer = Warp::new((), &config).renderer(spec, pools());
    assert!(
        renderer.source_block_frames.get() >= PACKET,
        "the fixture packet is the render quantum"
    );
    renderer
}

fn chunk_meta(renderer: &WarpRenderer<TestPools>, frames: usize) -> AudioChunkInfo {
    AudioChunkInfo {
        spec: renderer.spec,
        frames: u32::try_from(frames).expect("fixture frames"),
        ..AudioChunkInfo::default()
    }
}

fn prepared_frames(backend: StretchKind, chunk: usize, output_limit: usize) -> usize {
    let mut renderer = unity_renderer(backend);
    let meta = chunk_meta(&renderer, chunk);
    renderer
        .prepare_source_window(meta, chunk, output_limit)
        .expect("unity quantum");
    renderer
        .prepared_quantum
        .expect("prepared unity quantum")
        .active_frames
}

#[kithara::test]
#[case::passthrough(StretchKind::Identity, false)]
#[case::rate(StretchKind::Glide, true)]
fn a_unity_quantum_is_one_lane_packet_whatever_the_chunk(
    #[case] backend: StretchKind,
    #[case] staging: bool,
) {
    assert_eq!(unity_renderer(backend).requires_staging(), staging);
    for chunk in [363, 661, PACKET, 4_096] {
        assert_eq!(
            prepared_frames(backend, chunk, usize::MAX),
            PACKET,
            "a {chunk}-frame decoder chunk must not size the packet"
        );
    }
}

#[kithara::test]
#[case::passthrough(StretchKind::Identity)]
#[case::rate(StretchKind::Glide)]
fn a_unity_quantum_ends_at_the_output_limit(#[case] backend: StretchKind) {
    assert_eq!(prepared_frames(backend, 661, 100), 100);
}

#[kithara::test]
#[case::unbounded(None, 163_840)]
#[case::one_frame(Some(1), 20)]
#[case::lane_packet(Some(128), 128)]
fn prepared_output_storage_follows_the_quantum_without_narrowing_rates(
    #[case] quantum: Option<usize>,
    #[case] expected: usize,
) {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Glide)
        .maybe_render_quantum_frames(quantum.and_then(NonZeroUsize::new))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    for rate in [consts::SR, consts::SR + 1] {
        renderer.prepare(AudioSpec {
            sample_rate: NonZeroU32::new(rate).expect("prepared sample rate"),
            ..spec
        });
        let capabilities = renderer
            .engine
            .as_ref()
            .expect("prepared engine")
            .capabilities();
        assert_eq!(capabilities.max_output_frames(), expected);
        assert_eq!(
            renderer
                .scratch
                .as_ref()
                .expect("prepared output scratch")
                .capacity(),
            expected * usize::from(spec.channels)
        );
        let envelope = capabilities.rate_envelope();
        assert_eq!(envelope.min_source_frames_per_output(), 0.05);
        assert_eq!(envelope.max_source_frames_per_output(), 4.0);
    }
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn keylocked_output_preparation_retains_native_priming_capacity(#[case] backend: StretchKind) {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(backend)
        .pitch(Pitch::HELD)
        .render_quantum_frames(NonZeroUsize::MIN)
        .build();
    let renderer = Warp::new((), &config).renderer(spec, pools());
    let engine = renderer.engine.as_ref().expect("prepared native engine");
    let projection = renderer
        .projection
        .as_ref()
        .expect("prepared native projection");
    for engine in std::iter::once(engine).chain(projection.engines.iter()) {
        let capabilities = engine.capabilities();
        assert_eq!(capabilities.max_output_frames(), 163_840);
        assert!(capabilities.max_output_frames() >= capabilities.latency().second());
    }
}
