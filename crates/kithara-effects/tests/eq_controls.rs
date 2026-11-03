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
fn rejected_eq_retune_preserves_both_channels_format_and_targets() {
    use kithara_dsp::filter::FilterError;
    use kithara_effects::eq::generate_log_spaced_bands;

    let region = pools();
    let config = EqConfig::builder(region.clone()).build();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).unwrap());
    let make = || {
        let mut effect = EqEffect::new(&config, generate_log_spaced_bands(10), 44_100, 2).unwrap();
        effect.set_gain(0, GainDb::from(-12.0));
        effect.set_gain(9, GainDb::from(6.0));
        effect
    };
    let mut actual = make();
    let mut expected = make();
    let chunk = || {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames: 257,
                ..Default::default()
            },
            sample_buffer(&region, &[0.25, -0.125].repeat(257)),
        )
    };
    actual.process(chunk()).unwrap();
    expected.process(chunk()).unwrap();
    let refused = AudioSpec::new(1, NonZeroU32::new(12_000).unwrap());
    assert!(matches!(
        assert_no_alloc(|| actual.service_deferred(refused)),
        Err(EffectPrepareError::Filter(FilterError::Parameters))
    ));
    assert!(actual.accepts_spec(spec));
    assert!(!actual.accepts_spec(refused));
    assert_eq!(actual.bands(), expected.bands());
    for _ in 0..32 {
        let actual = actual.process(chunk()).unwrap();
        let expected = expected.process(chunk()).unwrap();
        assert_eq!(actual.meta, expected.meta);
        assert!(
            actual
                .samples
                .iter()
                .zip(expected.samples.iter())
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }
}

#[kithara::test(native)]
fn stereo_eq_preparation_and_commit_preserve_pcm_without_allocation() {
    use kithara_effects::eq::{EqLayout, StereoEq, generate_log_spaced_bands};

    let config = EqConfig::builder(pools()).build();
    let initial = NonZeroU32::new(44_100).unwrap();
    let rate = NonZeroU32::new(48_000).unwrap();
    let layout = |count| {
        Box::new(EqLayout::new(&config, &generate_log_spaced_bands(count), initial).unwrap())
    };
    let make = || {
        let mut eq = StereoEq::with_layout(&config, initial, layout(3)).unwrap();
        eq.set_gain(0, GainDb::from(-12.0));
        assert!(eq.take_layout(layout(3)).unwrap().is_none());
        eq.process(&mut [0.25], &mut [-0.125]);
        assert!(eq.take_layout(layout(10)).unwrap().is_none());
        eq.set_gain(2, GainDb::from(6.0));
        eq
    };
    let mut actual = make();
    let mut reference = make();
    assert_no_alloc(|| actual.prepare_sample_rate(rate)).unwrap();
    let (mut left, mut right) = ([0.25; 13], [-0.125; 13]);
    let (mut expected_left, mut expected_right) = (left, right);
    actual.process(&mut left, &mut right);
    reference.process(&mut expected_left, &mut expected_right);
    assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
    assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
    assert_eq!(
        assert_no_alloc(|| actual.apply_prepared_sample_rate()),
        Some(rate)
    );
    assert_no_alloc(|| reference.update_sample_rate(rate)).unwrap();
    for _ in 0..32 {
        assert_eq!(
            assert_no_alloc(|| actual.apply_prepared_sample_rate()),
            None
        );
        let (mut left, mut right) = ([0.25; 257], [-0.125; 257]);
        let (mut expected_left, mut expected_right) = (left, right);
        actual.process(&mut left, &mut right);
        reference.process(&mut expected_left, &mut expected_right);
        assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
        assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
    }
    actual.prepare_sample_rate(initial).unwrap();
    let displaced = actual.take_layout(layout(3)).unwrap();
    assert_eq!(actual.apply_prepared_sample_rate(), None);
    drop(displaced);
}

#[kithara::test(native)]
fn prepared_eq_steps_and_reset_preserve_targets_without_allocation() {
    let mut effect = EqEffect::new(
        &EqConfig::builder(pools()).build(),
        kithara_effects::eq::generate_log_spaced_bands(3),
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
