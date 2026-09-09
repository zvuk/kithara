use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::Range,
};

use kithara::{
    play::effects::{
        AudioEffect, PeakLimiter,
        eq::{EqConfig, EqEffect, GainDb, generate_log_spaced_bands},
    },
    resampler::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerQuality,
        ResamplerSettings, create_resampler, rubato::RubatoBackend,
    },
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
};
use kithara_integration_tests::bufpool_ext::{Pools, pools};
use kithara_test_fixtures::integration_fixtures::{
    dsp_silence, dsp_sweep, dsp_tone_a220, dsp_tone_a440, dsp_tone_ceiling, dsp_tone_ceiling_low,
    dsp_tone_ceiling_unity, dsp_tone_round_low, dsp_tone_round_mid, dsp_tone_round_poison,
    dsp_tone_unity,
};

const HOST_RATE: u16 = 48_000;
const INTERMEDIATE_RATE: u16 = 44_100;

const LIMITER_CEILING: f32 = 0.98;
const LIMITER_RELEASE_MS: f32 = 50.0;

const EQ_GAIN_PATHS: [(&str, GainDb); 3] = [
    ("flat", GainDb::DEFAULT),
    ("boosted", GainDb::MAX),
    ("killed", GainDb::MIN),
];

const SETTLE_FRAMES: usize = 9_600;

const RESAMPLE_CHUNK: usize = 1_024;
const ROUND_TRIP_FRAMES: usize = 12_288;
const ROUND_TRIP_EDGE: usize = 512;
const LAG_TOLERANCE: usize = 1;
const LAG_SEARCH: usize = 8;
const SWEEP_BLOCK: usize = 1_024;

fn test_pools() -> Pools {
    pools()
}

fn host_spec(channels: u16) -> AudioSpec {
    AudioSpec::new(
        channels,
        NonZeroU32::new(u32::from(HOST_RATE)).expect("host rate is non-zero"),
    )
}

fn pcm_chunk(pools: &Pools, spec: AudioSpec, samples: Vec<f32>) -> AudioChunk {
    let meta = AudioChunkInfo {
        spec,
        ..Default::default()
    };
    let mut pooled = pools
        .get_with_len::<f32>(samples.len())
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    pooled.copy_from_slice(&samples);
    AudioChunk::new(meta, pooled)
}

fn eq_with_gain(pools: &Pools, gain_db: GainDb, band_count: usize, channels: u16) -> EqEffect {
    let bands = generate_log_spaced_bands(band_count);
    let config = EqConfig::builder(pools.clone()).build();
    let mut eq = EqEffect::new(&config, bands, u32::from(HOST_RATE), channels)
        .unwrap_or_else(|error| panic!("test EQ: {error}"));
    for band in 0..band_count {
        eq.set_gain(band, gain_db);
    }
    eq
}

fn settle(eq: &mut EqEffect, pools: &Pools, spec: AudioSpec, silence: &[f32]) {
    let samples = silence[..SETTLE_FRAMES * usize::from(spec.channels)].to_vec();
    let _ = eq.process(pcm_chunk(pools, spec, samples));
}

fn process_eq(eq: &mut EqEffect, pools: &Pools, spec: AudioSpec, samples: Vec<f32>) -> Vec<f32> {
    eq.process(pcm_chunk(pools, spec, samples))
        .expect("EqEffect must emit the chunk it was handed")
        .samples
        .to_vec()
}

fn limiter_with_ceiling(ceiling: f32) -> PeakLimiter {
    PeakLimiter::new(
        NonZeroU32::new(u32::from(HOST_RATE)).expect("host rate is non-zero"),
        NonZeroUsize::new(2).expect("stereo is non-zero"),
        ceiling,
        LIMITER_RELEASE_MS,
    )
    .expect("limiter constants are valid")
}

fn limit_stereo(limiter: &mut PeakLimiter, interleaved: &[f32]) -> Vec<f32> {
    let mut left: Vec<f32> = interleaved.iter().step_by(2).copied().collect();
    let mut right: Vec<f32> = interleaved.iter().skip(1).step_by(2).copied().collect();
    {
        let mut planar: [&mut [f32]; 2] = [&mut left, &mut right];
        limiter.process_planar(&mut planar);
    }
    left.iter()
        .zip(right.iter())
        .flat_map(|(l, r)| [*l, *r])
        .collect()
}

fn master_chain(
    eq: &mut EqEffect,
    limiter: &mut PeakLimiter,
    pools: &Pools,
    interleaved: Vec<f32>,
) -> Vec<f32> {
    let processed = process_eq(eq, pools, host_spec(2), interleaved);
    limit_stereo(limiter, &processed)
}

fn non_finite_report(label: &str, samples: &[f32]) -> Vec<String> {
    samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| !sample.is_finite())
        .map(|(index, sample)| format!("{label}[{index}] = {sample}"))
        .take(4)
        .collect()
}

#[kithara::test]
#[case::flat(GainDb::DEFAULT)]
#[case::boosted(GainDb::MAX)]
#[case::killed(GainDb::MIN)]
fn eq_maps_silence_to_exact_silence(dsp_silence: Vec<f32>, #[case] gain_db: GainDb) {
    let pools = test_pools();
    let spec = host_spec(2);
    let mut eq = eq_with_gain(&pools, gain_db, 5, spec.channels);

    let output = process_eq(&mut eq, &pools, spec, dsp_silence[..4_096].to_vec());

    for (index, sample) in output.iter().enumerate() {
        assert_eq!(
            *sample,
            0.0,
            "silence in must be silence out at {gain_db} dB, sample {index} = {sample}",
            gain_db = f32::from(gain_db)
        );
    }
}

#[kithara::test]
fn limiter_maps_silence_to_exact_silence(dsp_silence: Vec<f32>) {
    let mut limiter = limiter_with_ceiling(LIMITER_CEILING);

    let output = limit_stereo(&mut limiter, &dsp_silence[..4_096]);

    for (index, sample) in output.iter().enumerate() {
        assert_eq!(*sample, 0.0, "sample {index} = {sample}");
    }
}

#[kithara::test]
fn master_chain_maps_silence_to_exact_silence(dsp_silence: Vec<f32>) {
    let pools = test_pools();
    let mut eq = eq_with_gain(&pools, GainDb::MAX, 5, 2);
    let mut limiter = limiter_with_ceiling(LIMITER_CEILING);

    let output = master_chain(&mut eq, &mut limiter, &pools, dsp_silence[..4_096].to_vec());

    for (index, sample) in output.iter().enumerate() {
        assert_eq!(*sample, 0.0, "sample {index} = {sample}");
    }
}

#[kithara::test]
#[case::mono_three_band(1, 3)]
#[case::stereo_five_band(2, 5)]
#[case::stereo_ten_band(2, 10)]
fn eq_at_zero_db_is_bit_exact_identity(
    dsp_tone_a440: Vec<f32>,
    #[case] channels: u16,
    #[case] band_count: usize,
) {
    let pools = test_pools();
    let spec = host_spec(channels);
    let mut eq = eq_with_gain(&pools, GainDb::default(), band_count, channels);
    let input = dsp_tone_a440[..8_192 * usize::from(channels)].to_vec();

    let output = process_eq(&mut eq, &pools, spec, input.clone());

    assert_eq!(
        output, input,
        "a 0 dB EQ must pass its input through intact"
    );
}

#[kithara::test]
fn eq_returns_to_bit_exact_identity_after_a_gain_round_trip(
    dsp_tone_a440: Vec<f32>,
    dsp_silence: Vec<f32>,
) {
    let pools = test_pools();
    let spec = host_spec(2);
    let mut eq = eq_with_gain(&pools, GainDb::default(), 3, spec.channels);

    eq.set_gain(0, GainDb::MAX);
    settle(&mut eq, &pools, spec, &dsp_silence);
    eq.set_gain(0, GainDb::default());
    settle(&mut eq, &pools, spec, &dsp_silence);
    settle(&mut eq, &pools, spec, &dsp_silence);

    let input = dsp_tone_a440[..8_192].to_vec();
    let output = process_eq(&mut eq, &pools, spec, input.clone());

    assert_eq!(
        output, input,
        "returning every band to 0 dB must restore the bit-exact bypass"
    );
}

#[kithara::test]
#[case::default_ceiling(LIMITER_CEILING, dsp_tone_ceiling())]
#[case::unity_ceiling(1.0, dsp_tone_ceiling_unity())]
#[case::low_ceiling(0.5, dsp_tone_ceiling_low())]
fn limiter_below_ceiling_is_bit_exact_identity(#[case] ceiling: f32, #[case] input: Vec<f32>) {
    let mut limiter = limiter_with_ceiling(ceiling);

    let mut output = Vec::new();
    for block in input.chunks(1_024) {
        output.extend_from_slice(&limit_stereo(&mut limiter, block));
    }

    assert_eq!(
        output, input,
        "material under the ceiling must survive the limiter untouched"
    );
}

#[kithara::test]
fn master_chain_at_unity_is_bit_exact_identity(dsp_tone_unity: Vec<f32>) {
    let pools = test_pools();
    let mut eq = eq_with_gain(&pools, GainDb::default(), 5, 2);
    let mut limiter = limiter_with_ceiling(LIMITER_CEILING);

    let input = dsp_tone_unity;

    let output = master_chain(&mut eq, &mut limiter, &pools, input.clone());

    assert_eq!(
        output, input,
        "a flat EQ into a limiter with headroom must not touch the signal"
    );
}

#[kithara::test]
#[case::tiny(1e-30)]
#[case::subnormal(f32::MIN_POSITIVE / 2.0)]
#[case::huge(1e30)]
#[case::nan(f32::NAN)]
#[case::infinity(f32::INFINITY)]
#[case::neg_infinity(f32::NEG_INFINITY)]
fn eq_output_stays_finite_on_pathological_input(
    dsp_tone_a440: Vec<f32>,
    dsp_silence: Vec<f32>,
    #[case] poison: f32,
) {
    let pools = test_pools();
    let spec = host_spec(1);
    let mut violations = Vec::new();

    for (path, gain_db) in EQ_GAIN_PATHS {
        let mut eq = eq_with_gain(&pools, gain_db, 3, spec.channels);
        settle(&mut eq, &pools, spec, &dsp_silence);

        let mut input = dsp_tone_a440[..1_024].to_vec();
        input[512] = poison;
        let output = process_eq(&mut eq, &pools, spec, input);
        violations.extend(non_finite_report(path, &output));

        let clean = dsp_tone_a440[..1_024].to_vec();
        let recovered = process_eq(&mut eq, &pools, spec, clean);
        violations.extend(non_finite_report(&format!("{path}/recovered"), &recovered));
    }

    assert!(
        violations.is_empty(),
        "{poison} reached the EQ output: {violations:?}"
    );
}

#[kithara::test]
#[case::tiny(1e-30)]
#[case::huge(1e30)]
#[case::nan(f32::NAN)]
#[case::infinity(f32::INFINITY)]
#[case::neg_infinity(f32::NEG_INFINITY)]
fn limiter_output_stays_finite_on_pathological_input(dsp_tone_a220: Vec<f32>, #[case] poison: f32) {
    let mut limiter = limiter_with_ceiling(LIMITER_CEILING);

    let mut input = dsp_tone_a220[..1_024].to_vec();
    input[512] = poison;
    let output = limit_stereo(&mut limiter, &input);

    let clean = dsp_tone_a220[..1_024].to_vec();
    let recovered = limit_stereo(&mut limiter, &clean);

    let mut violations = non_finite_report("limited", &output);
    violations.extend(non_finite_report("recovered", &recovered));
    assert!(
        violations.is_empty(),
        "{poison} reached the limiter output: {violations:?}"
    );
}

fn build_stage(pools: &Pools, source_rate: u16, target_rate: u16) -> impl Resampler {
    let settings = ResamplerSettings::builder()
        .channels(NonZeroUsize::MIN)
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(u32::from(source_rate))
                .expect("source rate is non-zero"),
            target_sample_rate: NonZeroU32::new(u32::from(target_rate))
                .expect("target rate is non-zero"),
        })
        .quality(ResamplerQuality::High)
        .options(
            ResamplerOptions::builder()
                .chunk_size(RESAMPLE_CHUNK)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    create_resampler(&config).expect("rubato stage must build")
}

fn run_stage(stage: &mut dyn Resampler, input: &[f32]) -> Vec<f32> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor + stage.input_frames_next() <= input.len() {
        let take = stage.input_frames_next();
        let mut buffer = vec![0.0f32; stage.output_frames_next()];
        let produced = {
            let input_refs = [&input[cursor..cursor + take]];
            let mut output_refs = [&mut buffer[..]];
            stage
                .process_into_buffer(&input_refs, &mut output_refs)
                .expect("rubato stage must accept its own block size")
        };
        cursor += produced.input_frames;
        buffer.truncate(produced.output_frames);
        out.append(&mut buffer);
    }
    out
}

fn round_trip(pools: &Pools, input: &[f32]) -> (Vec<f32>, usize) {
    let mut down = build_stage(pools, HOST_RATE, INTERMEDIATE_RATE);
    let mut up = build_stage(pools, INTERMEDIATE_RATE, HOST_RATE);
    let intermediate = run_stage(&mut down, input);
    let output = run_stage(&mut up, &intermediate);

    let host = usize::from(HOST_RATE);
    let intermediate_rate = usize::from(INTERMEDIATE_RATE);
    let rescaled = (down.output_delay() * host + intermediate_rate / 2) / intermediate_rate;
    (output, up.output_delay() + rescaled)
}

fn lag_error(input: &[f32], output: &[f32], lag: usize, window: Range<usize>) -> f32 {
    window
        .map(|frame| {
            let diff = output[lag + frame] - input[frame];
            diff * diff
        })
        .sum()
}

fn best_lag(input: &[f32], output: &[f32], predicted: usize, window: Range<usize>) -> usize {
    let error = |lag: &usize| lag_error(input, output, *lag, window.clone());
    (predicted.saturating_sub(LAG_SEARCH)..=predicted + LAG_SEARCH)
        .min_by(|left, right| error(left).total_cmp(&error(right)))
        .expect("the lag search range is non-empty")
}

fn rms(block: &[f32]) -> f32 {
    let power: f32 = block.iter().map(|sample| sample * sample).sum();
    (power / f32::from(u16::try_from(block.len()).expect("block length fits u16"))).sqrt()
}

fn shape_tolerance(freq_hz: f32) -> f32 {
    core::f32::consts::PI * freq_hz / f32::from(HOST_RATE) + 0.005
}

#[kithara::test]
#[case::low(200.0, dsp_tone_round_low())]
#[case::mid(1_000.0, dsp_tone_round_mid())]
fn resample_round_trip_preserves_wave_shape(#[case] freq_hz: f32, #[case] input: Vec<f32>) {
    let pools = test_pools();

    let (output, predicted) = round_trip(&pools, &input);

    let window = ROUND_TRIP_EDGE..ROUND_TRIP_FRAMES - ROUND_TRIP_EDGE;
    assert!(
        output.len() >= predicted + LAG_SEARCH + window.end,
        "the detour produced {} frames, too few to compare at delay {predicted}",
        output.len()
    );

    // A stage that misreports its latency by more than the rounding envelope desyncs the deck clock
    // from what the listener hears.
    let aligned = best_lag(&input, &output, predicted, window.clone());
    assert!(
        aligned.abs_diff(predicted) <= LAG_TOLERANCE,
        "the measured group delay {aligned} is more than {LAG_TOLERANCE} frame \
         off the reported {predicted}"
    );

    let tolerance = shape_tolerance(freq_hz);
    let worst = window
        .map(|frame| (output[aligned + frame] - input[frame]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst <= tolerance,
        "{freq_hz} Hz lost its shape in the resample detour: worst deviation \
         {worst} over {tolerance}"
    );
}

#[kithara::test]
fn resample_round_trip_keeps_sweep_band_energy(dsp_sweep: Vec<f32>) {
    const TOLERANCE: f32 = 0.03;

    let pools = test_pools();
    let input = dsp_sweep;

    let (output, predicted) = round_trip(&pools, &input);

    let last_block = ROUND_TRIP_FRAMES - ROUND_TRIP_EDGE - SWEEP_BLOCK;
    assert!(
        output.len() >= predicted + last_block + SWEEP_BLOCK,
        "the detour produced {} frames, too few to compare at delay {predicted}",
        output.len()
    );

    let mut start = ROUND_TRIP_EDGE;
    while start <= last_block {
        let source = rms(&input[start..start + SWEEP_BLOCK]);
        let detoured = rms(&output[predicted + start..predicted + start + SWEEP_BLOCK]);
        assert!(
            (detoured - source).abs() <= source * TOLERANCE,
            "the block at frame {start} lost energy in the detour: {source} in, \
             {detoured} out"
        );
        start += SWEEP_BLOCK;
    }
}

#[kithara::test]
#[case::tiny(1e-30)]
#[case::huge(1e30)]
fn resample_round_trip_stays_finite_on_pathological_input(
    dsp_tone_round_poison: Vec<f32>,
    #[case] poison: f32,
) {
    assert_round_trip_stays_finite(poison, dsp_tone_round_poison);
}

fn assert_round_trip_stays_finite(poison: f32, mut input: Vec<f32>) {
    let pools = test_pools();
    input[4_096] = poison;

    let (output, _) = round_trip(&pools, &input);

    let violations = non_finite_report("detoured", &output);
    assert!(
        violations.is_empty(),
        "{poison} reached the resampler output: {violations:?}"
    );
}
