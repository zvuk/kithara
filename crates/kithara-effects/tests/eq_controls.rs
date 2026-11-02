#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use kithara_effects::{
    AudioEffect, EffectChange, EffectConfig, EffectPrepareError, EffectSetup, EffectTarget, GainDb,
    eq::{EqBandConfig, EqBandConfigChange, EqConfig, EqEffect},
};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::{
    bufpool::{pools, sample_buffer},
    kithara,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[kithara::test(native)]
fn configured_eq_services_format_changes_and_preserves_targets() {
    use kithara_effects::eq::generate_log_spaced_bands;

    let region = pools();
    let initial = AudioSpec::new(2, NonZeroU32::new(44_100).unwrap());
    let mut bands = generate_log_spaced_bands(3);
    for (band, gain) in bands.iter_mut().zip([-12.0, 6.0, -3.0]) {
        band.set_gain_db(GainDb::from(gain));
    }
    let mut effect = EffectSetup::Config(EffectConfig::Equalizer(bands.clone()))
        .prepare(initial, &region)
        .unwrap();
    for (channels, rate) in [(2, 48_000), (1, 48_000), (4, 44_100)] {
        let spec = AudioSpec::new(channels, NonZeroU32::new(rate).unwrap());
        effect.reset();
        effect.service_deferred(spec).unwrap();
        assert!(effect.accepts_spec(spec));
        let mut reference = EqEffect::new(
            &EqConfig::builder(region.clone()).build(),
            bands.clone(),
            rate,
            channels,
        )
        .unwrap();
        for offset in [0, 13, 26] {
            let samples: Vec<_> = (offset..offset + 13)
                .flat_map(|frame| {
                    (0..channels).map(move |channel| {
                        if (frame + usize::from(channel)) % 7 == 0 {
                            0.5
                        } else {
                            -0.25
                        }
                    })
                })
                .collect();
            let make = || {
                AudioChunk::new(
                    AudioChunkInfo {
                        spec,
                        frames: 13,
                        ..Default::default()
                    },
                    sample_buffer(&region, &samples),
                )
            };
            effect.service_deferred(spec).unwrap();
            let actual = effect.process(make()).unwrap();
            let expected = reference.process(make()).unwrap();
            assert_eq!(
                &*actual.samples, &*expected.samples,
                "{channels}ch/{rate}, block {offset}"
            );
        }
    }
}

#[kithara::test(native)]
fn prepared_eq_format_refusal_precedes_audio_production() {
    let region = pools();
    let effect = EqEffect::new(
        &EqConfig::builder(region.clone()).build(),
        vec![EqBandConfig::default()],
        44_100,
        2,
    )
    .unwrap();
    let refused = AudioSpec::new(0, NonZeroU32::new(44_100).unwrap());
    assert!(matches!(
        EffectSetup::Prepared(Box::new(effect)).prepare(refused, &region),
        Err(EffectPrepareError::Shape)
    ));
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).unwrap());
    let effect = EffectSetup::Config(EffectConfig::Equalizer(vec![EqBandConfig::default()]))
        .prepare(spec, &region)
        .unwrap();
    assert!(effect.accepts_spec(spec));
}

#[kithara::test(native)]
fn prepared_eq_steps_and_reset_preserve_targets_without_allocation() {
    let mut effect = EqEffect::new(
        &EqConfig::builder(pools()).build(),
        vec![EqBandConfig::default(); 3],
        44_100,
        2,
    )
    .unwrap();
    for band in 0..3 {
        assert_no_alloc(|| {
            effect.configure(
                EffectTarget::EqBand { effect: 0, band },
                EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MIN)),
            )
        })
        .unwrap();
    }
    assert_no_alloc(|| effect.reset());
    for band in 0..3 {
        assert_eq!(effect.target_gain(band), Some(GainDb::MIN));
    }
    assert!(
        assert_no_alloc(|| effect.configure(
            EffectTarget::EqBand { effect: 0, band: 9 },
            EffectChange::EqBand(EqBandConfigChange::GainDb(GainDb::MAX)),
        ))
        .is_err()
    );
    assert_eq!(effect.target_gain(0), Some(GainDb::MIN));
}
