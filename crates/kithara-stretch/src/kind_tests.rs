use kithara_test_utils::kithara;

use crate::{ElasticConfig, StretchKind, build_engine, test_pools};

#[kithara::test(native, flash(false))]
fn roundtrips_compiled_variants_through_u8() {
    for kind in StretchKind::all().iter().copied() {
        assert_eq!(StretchKind::from(u8::from(kind)), kind);
    }
}

#[kithara::test(native, flash(false))]
fn keeps_stable_discriminants_and_default_decode() {
    #[cfg(feature = "stretch-signalsmith")]
    assert_eq!(u8::from(StretchKind::Signalsmith), 1);

    #[cfg(feature = "stretch-bungee")]
    assert_eq!(u8::from(StretchKind::Bungee), 2);

    let default = StretchKind::all()[0];
    assert_eq!(StretchKind::from(0), default);
    assert_eq!(StretchKind::from(99), default);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn streams_terminal_tail_through_large_caller_buffers(#[case] backend: StretchKind) {
    const CHANNELS: usize = 2;
    const MAX_OUTPUT_FRAMES: usize = 163_840;
    const MAX_SOURCE_FRAMES: usize = 655_360;
    const MAX_DRAIN_STEPS: usize = 1_024;

    let pools = test_pools::pools();
    let config = ElasticConfig::builder()
        .backend(backend)
        .sample_rate(48_000)
        .channels(CHANNELS)
        .max_source_frames(MAX_SOURCE_FRAMES)
        .max_output_frames(MAX_OUTPUT_FRAMES)
        .pools(pools.clone())
        .build()
        .expect("terminal contract fixture is valid");
    let mut engine = build_engine(config).expect("compiled backend prepares");
    let capabilities = engine.capabilities();
    let envelope = capabilities.rate_envelope();
    let request = envelope
        .largest_request_at(
            0.05,
            capabilities.max_source_frames(),
            capabilities.max_output_frames(),
        )
        .expect("the minimum rate has a representable request");
    let mut source = pools.get::<f32>();
    source
        .ensure_len(request.source_frames() * CHANNELS)
        .expect("source buffer fits");
    let mut output = pools.get::<f32>();
    output
        .ensure_len(request.output_frames() * CHANNELS)
        .expect("render output buffer fits");
    engine
        .process(request, &source, &mut output)
        .expect("fixture arms a terminal tail");
    output
        .ensure_len(MAX_OUTPUT_FRAMES * CHANNELS)
        .expect("large terminal output buffer fits");

    let mut complete = false;
    for _ in 0..MAX_DRAIN_STEPS {
        let drained = engine
            .flush(&mut output)
            .expect("large caller buffers must stream the terminal tail");
        assert!(drained.frames() <= MAX_OUTPUT_FRAMES);
        if drained.complete() {
            complete = true;
            break;
        }
        assert!(drained.frames() > 0, "an incomplete drain must progress");
    }
    assert!(complete, "bounded terminal tail must converge");
}
