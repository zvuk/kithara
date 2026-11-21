#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara_effects::{
    FxParamChange,
    reverb::{Damping, Decay, Reverb, ReverbConfig, ReverbError, Width},
};
use kithara_test_utils::{bufpool::pools, kithara};
use num_traits::ToPrimitive;

const RATE: NonZeroU32 = NonZeroU32::MIN.saturating_add(47_999);

#[kithara::test]
fn reverb_network_is_bit_identical_across_uneven_partitions_and_reset() {
    let mut input = [vec![0.0_f32; 8192], vec![0.0_f32; 8192]];
    input[0][0] = 1.0;
    input[1][7] = -0.5;
    let config = ReverbConfig::builder()
        .width(Width::checked(0.37).unwrap())
        .build();
    let mut whole = Reverb::new(RATE, config, &pools()).unwrap();
    let mut split = Reverb::new(RATE, config, &pools()).unwrap();
    let mut expected = [vec![0.0_f32; 8192], vec![0.0_f32; 8192]];
    let [left, right] = &mut expected;
    whole
        .process([&input[0], &input[1]], [left, right])
        .unwrap();
    assert!(expected.iter().flatten().all(|sample| sample.is_finite()));
    assert!(expected.iter().flatten().any(|sample| sample.abs() > 1e-6));
    let mut actual = [vec![0.0_f32; 8192], vec![0.0_f32; 8192]];
    let mut start = 0;
    for frames in [0, 1, 17, 513, 3, 257].into_iter().cycle() {
        let end = (start + frames).min(8192);
        let [left, right] = &mut actual;
        split
            .process(
                [&input[0][start..end], &input[1][start..end]],
                [&mut left[start..end], &mut right[start..end]],
            )
            .unwrap();
        start = end;
        if start == 8192 {
            break;
        }
    }
    for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
    split.reset();
    let [left, right] = &mut actual;
    split
        .process([&input[0], &input[1]], [left, right])
        .unwrap();
    for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[kithara::test]
fn zero_width_writes_the_same_wet_signal_to_both_channels() {
    let config = ReverbConfig::builder().width(Width::MIN).build();
    let mut reverb = Reverb::new(RATE, config, &pools()).unwrap();
    let mut input = vec![0.0_f32; 8192];
    input[0] = 1.0;
    let silence = vec![0.0_f32; 8192];
    let mut left = vec![0.0_f32; 8192];
    let mut right = left.clone();
    reverb
        .process([&input, &silence], [&mut left, &mut right])
        .unwrap();
    assert!(left.iter().any(|sample| sample.abs() > 1e-6));
    for (left, right) in left.iter().zip(right) {
        assert_eq!(left.to_bits(), right.to_bits());
    }
}

#[kithara::test]
fn exhausted_tail_stays_exactly_silent_and_keeps_its_declared_boundary() {
    let rate = NonZeroU32::new(8000).unwrap();
    let config = ReverbConfig::builder()
        .decay(Decay::MIN)
        .damping(Damping::MAX)
        .width(Width::checked(0.37).unwrap())
        .build();
    let mut reverb = Reverb::new(rate, config, &pools()).unwrap();
    assert!(reverb.tail_exhausted());
    let mut left = [0.0_f32];
    let mut right = left;
    reverb
        .process([&[1.0], &[-0.5]], [&mut left, &mut right])
        .unwrap();
    assert!(!reverb.tail_exhausted());
    let bound = declared_tail_frames(rate, Decay::MIN);
    let silence = vec![0.0_f32; bound - 1];
    let mut left = silence.clone();
    let mut right = silence.clone();
    reverb
        .process([&silence, &silence], [&mut left, &mut right])
        .unwrap();
    assert!(!reverb.tail_exhausted());
    reverb
        .process([&[0.0], &[0.0]], [&mut left[..1], &mut right[..1]])
        .unwrap();
    assert!(
        left[..1]
            .iter()
            .chain(&right[..1])
            .all(|sample| sample.abs() <= 1e-6)
    );
    assert!(reverb.tail_exhausted());
    let silence = vec![0.0_f32; 1024];
    let mut after_left = vec![0.125_f32; 1024];
    let mut after_right = vec![-0.25_f32; 1024];
    reverb
        .process([&silence, &silence], [&mut after_left, &mut after_right])
        .unwrap();
    assert!(
        after_left
            .iter()
            .chain(&after_right)
            .all(|sample| sample.to_bits() == 0)
    );
}

#[kithara::test]
fn parameter_extrema_keep_the_wet_network_finite() {
    let input: Vec<_> = [0.25_f32, -0.125, 0.0, 0.5]
        .into_iter()
        .cycle()
        .take(4096)
        .collect();
    for decay in [Decay::MIN, Decay::MAX] {
        for damping in [Damping::MIN, Damping::MAX] {
            for width in [Width::MIN, Width::MAX] {
                let config = ReverbConfig::builder()
                    .decay(decay)
                    .damping(damping)
                    .width(width)
                    .build();
                let mut reverb = Reverb::new(RATE, config, &pools()).unwrap();
                let mut left = vec![0.0_f32; input.len()];
                let mut right = left.clone();
                reverb
                    .process([&input, &input], [&mut left, &mut right])
                    .unwrap();
                assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
                assert!(!reverb.tail_exhausted());
            }
        }
    }
}

#[kithara::test]
fn shape_refusal_preserves_wet_buffers_and_network_history() {
    let config = ReverbConfig::default();
    let mut reverb = Reverb::new(RATE, config, &pools()).unwrap();
    let mut control = Reverb::new(RATE, config, &pools()).unwrap();
    let input = [1.0_f32; 128];
    let mut warm = [vec![0.0; 128], vec![0.0; 128]];
    let [left, right] = &mut warm;
    reverb.process([&input, &input], [left, right]).unwrap();
    let [left, right] = &mut warm;
    control.process([&input, &input], [left, right]).unwrap();
    let mut left = [0.125_f32; 128];
    let mut right = [-0.25_f32; 127];
    assert!(matches!(
        reverb.process([&input, &input], [&mut left, &mut right]),
        Err(ReverbError::Shape)
    ));
    assert_eq!(left, [0.125; 128]);
    assert_eq!(right, [-0.25; 127]);
    let silence = vec![0.0_f32; 4096];
    let mut actual = [vec![0.0_f32; 4096], vec![0.0_f32; 4096]];
    let mut expected = actual.clone();
    let [left, right] = &mut actual;
    reverb.process([&silence, &silence], [left, right]).unwrap();
    let [left, right] = &mut expected;
    control
        .process([&silence, &silence], [left, right])
        .unwrap();
    for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

// Independent unbounded f64 histories for the declared 44.1 kHz layout.
fn scalar_network(input: [&[f32]; 2], rate: u32, config: ReverbConfig) -> [Vec<f64>; 2] {
    let mut output: [Vec<f64>; 2] = std::array::from_fn(|channel| {
        let spread = if channel == 0 { 0 } else { 13 };
        let comb_lengths = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617].map(|length| {
            ((f64::from(length + spread) * f64::from(rate)) / 44_100.0)
                .round()
                .to_usize()
                .expect("fixture geometry fits usize")
        });
        let diffuser_lengths = [31, 47, 73, 101].map(|length| {
            ((f64::from(length + spread) * f64::from(rate)) / 44_100.0)
                .round()
                .to_usize()
                .expect("fixture geometry fits usize")
        });
        let gains = comb_lengths.map(|length| {
            10.0_f64.powf(
                -3.0 * length.to_f64().expect("fixture delay fits f64")
                    / (f64::from(rate) * f64::from(config.decay())),
            )
        });
        let mut combs: [Vec<f64>; 8] = std::array::from_fn(|_| Vec::new());
        let mut diffusers: [Vec<f64>; 4] = std::array::from_fn(|_| Vec::new());
        let mut memory = [0.0_f64; 8];
        let coefficient = 0.4 * f64::from(config.damping());
        input[channel]
            .iter()
            .enumerate()
            .map(|(frame, input)| {
                let mut sum = 0.0;
                for (((history, state), delay), gain) in combs
                    .iter_mut()
                    .zip(&mut memory)
                    .zip(comb_lengths)
                    .zip(gains)
                {
                    let delayed = frame
                        .checked_sub(delay)
                        .and_then(|past| history.get(past))
                        .copied()
                        .unwrap_or(0.0);
                    sum += delayed;
                    *state = (1.0 - coefficient) * delayed + coefficient * *state;
                    history.push(gain * *state + f64::from(*input) * (1.0 - gain) / 8.0);
                }
                for (history, delay) in diffusers.iter_mut().zip(diffuser_lengths) {
                    let delayed = frame
                        .checked_sub(delay)
                        .and_then(|past| history.get(past))
                        .copied()
                        .unwrap_or(0.0);
                    history.push(sum + 0.5 * delayed);
                    sum = delayed - sum;
                }
                sum
            })
            .collect()
    });
    let width = f64::from(config.width());
    let [left, right] = &mut output;
    for (left, right) in left.iter_mut().zip(right) {
        let mid = (*left + *right) * 0.5;
        *left = width * *left + (1.0 - width) * mid;
        *right = width * *right + (1.0 - width) * mid;
    }
    output
}

#[kithara::test]
fn comb_damping_and_diffusion_match_an_independent_scalar_network() {
    let rate = NonZeroU32::new(8000).unwrap();
    let mut input = [vec![0.0_f32; 8192], vec![0.0_f32; 8192]];
    input[0][0] = 1.0;
    input[1][7] = -0.5;
    for damping in [Damping::MIN, Damping::MAX] {
        for width in [Width::MIN, Width::checked(0.37).unwrap(), Width::MAX] {
            let config = ReverbConfig::builder()
                .decay(Decay::MIN)
                .damping(damping)
                .width(width)
                .build();
            let expected = scalar_network([&input[0], &input[1]], rate.get(), config);
            let mut actual = [vec![0.0_f32; 8192], vec![0.0_f32; 8192]];
            let mut reverb = Reverb::new(rate, config, &pools()).unwrap();
            let [left, right] = &mut actual;
            reverb
                .process([&input[0], &input[1]], [left, right])
                .unwrap();
            for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                for (frame, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                    assert!(
                        (f64::from(*actual) - expected).abs() <= 1e-6,
                        "damping {damping:?}, width {width:?}, channel {channel}, frame {frame}: {actual} != {expected}"
                    );
                }
            }
        }
    }
}

#[kithara::test]
fn retirement_and_reentry_keep_the_same_bits_across_partitions() {
    let rate = NonZeroU32::new(8000).unwrap();
    let config = ReverbConfig::builder()
        .decay(Decay::MIN)
        .width(Width::checked(0.37).unwrap())
        .build();
    let mut input = [vec![0.0_f32; 6000], vec![0.0_f32; 6000]];
    input[0][0] = 1.0;
    input[1][4000] = -0.5;
    let mut whole = Reverb::new(rate, config, &pools()).unwrap();
    let mut split = Reverb::new(rate, config, &pools()).unwrap();
    let mut expected = [vec![0.0_f32; 6000], vec![0.0_f32; 6000]];
    let mut actual = expected.clone();
    let [left, right] = &mut expected;
    whole
        .process([&input[0], &input[1]], [left, right])
        .unwrap();
    let mut start = 0;
    for count in [0, 1, 37, 257, 3].into_iter().cycle() {
        let end = (start + count).min(6000);
        let [left, right] = &mut actual;
        split
            .process(
                [&input[0][start..end], &input[1][start..end]],
                [&mut left[start..end], &mut right[start..end]],
            )
            .unwrap();
        start = end;
        if start == 6000 {
            break;
        }
    }
    for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
    assert_eq!(whole.tail_exhausted(), split.tail_exhausted());
}

#[kithara::test]
fn unrepresentable_rate_is_refused_without_clamping() {
    assert!(
        matches!(Reverb::new(NonZeroU32::MIN, ReverbConfig::default(), &pools()),
        Err(ReverbError::Geometry(rate)) if rate == NonZeroU32::MIN)
    );
}

#[kithara::test]
fn maximum_decay_undamped_boundary_step_is_below_retirement_threshold() {
    let mut maximum = 0.0_f32;
    for rate in [8000, 48000] {
        let rate = NonZeroU32::new(rate).unwrap();
        let config = ReverbConfig::builder()
            .decay(Decay::MAX)
            .damping(Damping::MIN)
            .width(Width::MAX)
            .build();
        let full_scale = [1.0_f32; 512];
        let silence = [0.0_f32; 512];
        let bound = declared_tail_frames(rate, Decay::MAX);
        let dc_frames = (4.0 * f64::from(Decay::MAX) * f64::from(rate.get()))
            .ceil()
            .to_usize()
            .expect("fixture geometry fits usize");
        for excitation in [1, dc_frames] {
            let mut reverb = Reverb::new(rate, config, &pools()).unwrap();
            let mut left = [0.0_f32; 512];
            let mut right = left;
            let mut remaining = excitation;
            while remaining > 0 {
                let count = remaining.min(512);
                reverb
                    .process(
                        [&full_scale[..count], &full_scale[..count]],
                        [&mut left[..count], &mut right[..count]],
                    )
                    .unwrap();
                remaining -= count;
            }
            let mut remaining = bound - 1;
            while remaining > 0 {
                let count = remaining.min(512);
                reverb
                    .process(
                        [&silence[..count], &silence[..count]],
                        [&mut left[..count], &mut right[..count]],
                    )
                    .unwrap();
                remaining -= count;
            }
            assert!(!reverb.tail_exhausted());
            reverb
                .process(
                    [&silence[..1], &silence[..1]],
                    [&mut left[..1], &mut right[..1]],
                )
                .unwrap();
            assert!(reverb.tail_exhausted());
            let peak = left[0].abs().max(right[0].abs());
            eprintln!(
                "boundary rate={} excitation_frames={excitation} decay={:?} damping={:?} width={:?} peak={peak:.12e}",
                rate.get(),
                Decay::MAX,
                Damping::MIN,
                Width::MAX
            );
            maximum = maximum.max(peak);
        }
    }
    assert!(
        maximum <= 1e-6,
        "maximum pre-retirement step {maximum:.12e}"
    );
}

fn declared_tail_frames(rate: NonZeroU32, decay: Decay) -> usize {
    let rate = f64::from(rate.get());
    let longest = (1630.0 * rate / 44_100.0)
        .round()
        .to_usize()
        .expect("fixture geometry fits usize");
    let diffusion: usize = [31, 47, 73, 101]
        .into_iter()
        .map(|length| {
            (f64::from(length + 13) * rate / 44_100.0)
                .round()
                .to_usize()
                .expect("fixture geometry fits usize")
        })
        .sum();
    let rounds = (1e-6_f64.ln() / 0.5_f64.ln())
        .ceil()
        .to_usize()
        .expect("fixture geometry fits usize")
        + 1;
    (2.0 * f64::from(decay) * rate)
        .ceil()
        .to_usize()
        .expect("fixture geometry fits usize")
        + longest
        + diffusion * rounds
}

fn advance_reverb(reverb: &mut Reverb, mut frames: usize, sample: f32) -> [f32; 2] {
    let input = [sample; 512];
    let mut left = [0.0; 512];
    let mut right = left;
    let mut last = [0.0; 2];
    while frames > 0 {
        let count = frames.min(input.len());
        let input = input.get(..count).expect("bounded fixture span");
        reverb
            .process(
                [input, input],
                [
                    left.get_mut(..count).expect("left span"),
                    right.get_mut(..count).expect("right span"),
                ],
            )
            .expect("equal fixture spans");
        let index = count.wrapping_sub(1);
        last = [
            *left.get(index).expect("nonempty span"),
            *right.get(index).expect("nonempty span"),
        ];
        frames = frames.wrapping_sub(count);
    }
    last
}

#[kithara::test]
#[case(Decay::MAX, Decay::MIN)]
#[case(Decay::MIN, Decay::MAX)]
fn decay_change_rearms_active_history_from_the_command_frame(
    #[case] from: Decay,
    #[case] to: Decay,
) {
    let rate = NonZeroU32::new(8000).unwrap();
    let config = ReverbConfig::builder().decay(from).build();
    let mut actual = Reverb::new(rate, config, &pools()).unwrap();
    let mut unchanged = Reverb::new(rate, config, &pools()).unwrap();
    let warm = (4.0 * f64::from(from) * f64::from(rate.get()))
        .ceil()
        .to_usize()
        .unwrap();
    let age = (0.9 * f64::from(from) * f64::from(rate.get()))
        .ceil()
        .to_usize()
        .unwrap();
    for reverb in [&mut actual, &mut unchanged] {
        advance_reverb(reverb, warm, 1.0);
        advance_reverb(reverb, age, 0.0);
        assert!(!reverb.tail_exhausted());
    }
    actual
        .set(FxParamChange {
            index: 0,
            value: f64::from(to),
        })
        .unwrap();
    actual.process([&[], &[]], [&mut [], &mut []]).unwrap();
    let first = advance_reverb(&mut actual, 1, 0.0);
    let reference = advance_reverb(&mut unchanged, 1, 0.0);
    assert!(
        reference.iter().any(|sample| sample.abs() > 1e-6),
        "the fixture has an audible tail at the command"
    );
    assert_eq!(
        first.map(f32::to_bits),
        reference.map(f32::to_bits),
        "the command preserves the stored signal"
    );
    let bound = declared_tail_frames(rate, to);
    let checkpoint = if to > from {
        declared_tail_frames(rate, from).min(bound - 1)
    } else {
        0
    };
    let at_checkpoint = advance_reverb(&mut actual, checkpoint, 0.0);
    if to > from {
        assert!(
            at_checkpoint.iter().any(|sample| sample.abs() > 1e-6),
            "the new feedback retains signal beyond the old bound"
        );
    }
    assert!(
        !actual.tail_exhausted(),
        "lengthening cannot retire at the old bound"
    );
    advance_reverb(&mut actual, bound - 1 - checkpoint, 0.0);
    assert!(
        !actual.tail_exhausted(),
        "the command frame is the new bound origin"
    );
    let last = advance_reverb(&mut actual, 1, 0.0);
    assert!(actual.tail_exhausted());
    assert!(last.iter().all(|sample| sample.abs() <= 1e-6));
    assert_eq!(advance_reverb(&mut actual, 512, 0.0), [0.0; 2]);
}

#[kithara::test]
fn decay_change_does_not_revive_an_exhausted_tail() {
    let rate = NonZeroU32::new(8000).unwrap();
    let mut reverb = Reverb::new(
        rate,
        ReverbConfig::builder().decay(Decay::MIN).build(),
        &pools(),
    )
    .unwrap();
    advance_reverb(&mut reverb, 512, 1.0);
    advance_reverb(&mut reverb, declared_tail_frames(rate, Decay::MIN), 0.0);
    assert!(reverb.tail_exhausted());
    reverb
        .set(FxParamChange {
            index: 0,
            value: f64::from(Decay::MAX),
        })
        .unwrap();
    assert!(reverb.tail_exhausted());
    assert_eq!(advance_reverb(&mut reverb, 512, 0.0), [0.0; 2]);
    assert!(reverb.tail_exhausted());
}

#[kithara::test]
fn identical_decay_keeps_the_original_retirement_frame() {
    let rate = NonZeroU32::new(8000).unwrap();
    let config = ReverbConfig::builder().decay(Decay::MIN).build();
    let mut actual = Reverb::new(rate, config, &pools()).unwrap();
    let mut unchanged = Reverb::new(rate, config, &pools()).unwrap();
    for reverb in [&mut actual, &mut unchanged] {
        advance_reverb(reverb, 512, 1.0);
    }
    actual
        .set(FxParamChange {
            index: 0,
            value: f64::from(Decay::MIN),
        })
        .unwrap();
    let bound = declared_tail_frames(rate, Decay::MIN);
    for reverb in [&mut actual, &mut unchanged] {
        advance_reverb(reverb, bound - 1, 0.0);
        assert!(!reverb.tail_exhausted());
    }
    assert_eq!(
        advance_reverb(&mut actual, 1, 0.0).map(f32::to_bits),
        advance_reverb(&mut unchanged, 1, 0.0).map(f32::to_bits)
    );
    assert!(actual.tail_exhausted());
    assert!(unchanged.tail_exhausted());
}

#[kithara::test]
fn indexed_parameters_match_prepared_values_and_refuse_without_changing_history() {
    let config = ReverbConfig::builder()
        .decay(Decay::MAX)
        .damping(Damping::MAX)
        .width(Width::MIN)
        .build();
    let mut actual = Reverb::new(RATE, ReverbConfig::default(), &pools()).unwrap();
    let mut expected = Reverb::new(RATE, config, &pools()).unwrap();
    for (index, value) in [
        f64::from(Decay::MAX),
        f64::from(Damping::MAX),
        f64::from(Width::MIN),
    ]
    .into_iter()
    .enumerate()
    {
        actual.set(FxParamChange { index, value }).unwrap();
    }
    let input = [0.25_f32; 8192];
    let mut output = [vec![0.0_f32; 8192], vec![0.0_f32; 8192]];
    let mut reference = output.clone();
    let [left, right] = &mut output;
    actual
        .process([&input, &[-0.125_f32; 8192]], [left, right])
        .unwrap();
    let [left, right] = &mut reference;
    expected
        .process([&input, &[-0.125_f32; 8192]], [left, right])
        .unwrap();
    assert_eq!(
        output
            .iter()
            .flatten()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>(),
        reference
            .iter()
            .flatten()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>()
    );
    for (index, value) in [
        (3, 0.0),
        (usize::MAX, 0.0),
        (0, f64::from(Decay::MIN).next_down()),
        (1, f64::from(Damping::MAX).next_up()),
        (2, f64::NAN),
        (0, f64::INFINITY),
    ] {
        assert!(actual.set(FxParamChange { index, value }).is_err());
    }
    let silence = [0.0_f32; 8192];
    let [left, right] = &mut output;
    actual.process([&silence, &silence], [left, right]).unwrap();
    let [left, right] = &mut reference;
    expected
        .process([&silence, &silence], [left, right])
        .unwrap();
    assert_eq!(
        output
            .iter()
            .flatten()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>(),
        reference
            .iter()
            .flatten()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>()
    );
}

#[kithara::test]
fn parameter_commands_keep_partition_bits_at_their_frames() {
    let input: Vec<f32> = [0.25, -0.125, 0.0, 0.5]
        .into_iter()
        .cycle()
        .take(4096)
        .collect();
    let commands = [
        (
            257,
            FxParamChange {
                index: 0,
                value: f64::from(Decay::MAX),
            },
        ),
        (
            769,
            FxParamChange {
                index: 1,
                value: f64::from(Damping::MAX),
            },
        ),
        (
            1541,
            FxParamChange {
                index: 2,
                value: f64::from(Width::MIN),
            },
        ),
        (
            2049,
            FxParamChange {
                index: 0,
                value: f64::from(Decay::MIN),
            },
        ),
    ];
    let render = |partition: &[usize]| {
        let mut reverb = Reverb::new(RATE, ReverbConfig::default(), &pools()).unwrap();
        let mut output = [vec![0.0_f32; input.len()], vec![0.0_f32; input.len()]];
        let mut start = 0;
        let mut command = 0;
        for count in partition.iter().copied().cycle() {
            if let Some((frame, value)) = commands.get(command).filter(|(frame, _)| *frame == start)
            {
                assert_eq!(*frame, start);
                reverb.set(*value).unwrap();
                command += 1;
            }
            let boundary = commands
                .get(command)
                .map_or(input.len(), |(frame, _)| *frame);
            let end = start.saturating_add(count).min(boundary).min(input.len());
            let [left, right] = &mut output;
            reverb
                .process(
                    [&input[start..end], &input[start..end]],
                    [&mut left[start..end], &mut right[start..end]],
                )
                .unwrap();
            start = end;
            if start == input.len() {
                break;
            }
        }
        assert_eq!(command, commands.len());
        output
    };
    let whole = render(&[4096]);
    let partitioned = render(&[0, 1, 31, 257, 3, 513]);
    assert_eq!(
        whole
            .iter()
            .flatten()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>(),
        partitioned
            .iter()
            .flatten()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>()
    );
}
