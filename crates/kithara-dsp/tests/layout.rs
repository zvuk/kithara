#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara_test_utils::kithara;

const FRAMES: [usize; 9] = [0, 1, 3, 4, 5, 8, 16, 17, 1023];
const STARTS: [usize; 2] = [0, 3];
const MAX_CHANNELS: usize = 8;
/// Past eight planes, where a stack array of plane references would spill.
const MAX_CHANNEL_MAJOR: usize = 12;
/// Samples after the last whole plane, which a channel-major layout ignores.
const LEFTOVERS: [usize; 2] = [0, 1];
const TAIL: usize = 3;
const UNWRITTEN: f32 = -1.0;

fn count(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("channel counts are non-zero")
}

/// Scrambled bit patterns, so `NaN` payloads, infinities, subnormals and −0.0
/// all pass through the kernels.
fn sample(channel: usize, frame: usize) -> f32 {
    let key = u32::try_from(channel << 16 | frame).expect("fixture index fits u32");
    f32::from_bits(key.wrapping_mul(0x9E37_79B9).rotate_left(11))
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn stride(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("strides are non-zero")
}

/// The channel sum in channel order times `1 / N`: the mean every analysis
/// path took before `downmix`.
fn channel_mean(frame: &[f32]) -> f32 {
    let channels = u16::try_from(frame.len()).expect("fixture channel counts fit u16");
    frame.iter().sum::<f32>() * (1.0 / f32::from(channels))
}

#[kithara::test]
fn deinterleave_variable_matches_fast_interleave() {
    for inputs in 1..=MAX_CHANNELS {
        for outputs in 1..=MAX_CHANNELS {
            for frames in FRAMES {
                for start in STARTS {
                    let interleaved: Vec<f32> = (0..frames * inputs + TAIL)
                        .map(|slot| sample(0, slot))
                        .collect();
                    let mut expected = vec![vec![UNWRITTEN; start + frames + TAIL]; outputs];
                    let mut actual = expected.clone();
                    let range = start..start + frames;
                    fast_interleave::deinterleave_variable(
                        &interleaved,
                        count(inputs),
                        &mut expected,
                        range.clone(),
                    );
                    kithara_dsp::deinterleave_variable(
                        &interleaved,
                        count(inputs),
                        &mut actual,
                        range,
                    );
                    for (channel, (got, want)) in actual.iter().zip(&expected).enumerate() {
                        assert_eq!(
                            bits(got),
                            bits(want),
                            "{inputs} into {outputs} channels, {frames} frames from {start}, \
                             channel {channel}"
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn interleave_channel_major_matches_fast_interleave_over_its_planes() {
    for inputs in 1..=MAX_CHANNEL_MAJOR {
        for outputs in 1..=MAX_CHANNEL_MAJOR {
            for frames in FRAMES {
                for start in STARTS {
                    for leftover in LEFTOVERS {
                        let plane = start + frames + TAIL;
                        let planar: Vec<f32> = (0..inputs * plane + leftover)
                            .map(|slot| sample(slot / plane, slot % plane))
                            .collect();
                        let planes: Vec<&[f32]> = planar.chunks_exact(plane).collect();
                        let mut expected = vec![UNWRITTEN; frames * outputs + TAIL];
                        let mut actual = expected.clone();
                        let range = start..start + frames;
                        fast_interleave::interleave_variable(
                            &planes,
                            range.clone(),
                            &mut expected,
                            count(outputs),
                        );
                        kithara_dsp::interleave_channel_major(
                            &planar,
                            stride(plane),
                            range,
                            &mut actual,
                            count(outputs),
                        );
                        assert_eq!(
                            bits(&actual),
                            bits(&expected),
                            "{inputs} into {outputs} channels, {frames} frames from {start}, \
                             {leftover} leftover"
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn deinterleave_channel_major_matches_fast_interleave_over_its_planes() {
    for inputs in 1..=MAX_CHANNEL_MAJOR {
        for outputs in 1..=MAX_CHANNEL_MAJOR {
            for frames in FRAMES {
                for start in STARTS {
                    for leftover in LEFTOVERS {
                        let plane = start + frames + TAIL;
                        let interleaved: Vec<f32> = (0..frames * inputs + TAIL)
                            .map(|slot| sample(0, slot))
                            .collect();
                        let mut expected = vec![UNWRITTEN; outputs * plane + leftover];
                        let mut actual = expected.clone();
                        let range = start..start + frames;
                        let mut planes: Vec<&mut [f32]> =
                            expected.chunks_exact_mut(plane).collect();
                        fast_interleave::deinterleave_variable(
                            &interleaved,
                            count(inputs),
                            &mut planes,
                            range.clone(),
                        );
                        kithara_dsp::deinterleave_channel_major(
                            &interleaved,
                            count(inputs),
                            &mut actual,
                            stride(plane),
                            range,
                        );
                        assert_eq!(
                            bits(&actual),
                            bits(&expected),
                            "{inputs} into {outputs} channels, {frames} frames from {start}, \
                             {leftover} leftover"
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn a_short_plane_bounds_the_channel_major_layout() {
    const U: f32 = UNWRITTEN;
    let planar = [1.0_f32, 2.0, 3.0, -1.0, -2.0, -3.0, 9.0];

    let mut mono = [U; 5];
    kithara_dsp::interleave_channel_major(&planar[..3], stride(3), 0..5, &mut mono, count(1));
    assert_eq!(bits(&mono), bits(&[1.0, 2.0, 3.0, U, U]));

    let mut stereo = [U; 8];
    kithara_dsp::interleave_channel_major(&planar, stride(3), 1..5, &mut stereo, count(2));
    assert_eq!(bits(&stereo), bits(&[2.0, -2.0, 3.0, -3.0, U, U, U, U]));

    let mut three = [U; 6];
    kithara_dsp::interleave_channel_major(&planar, stride(3), 0..2, &mut three, count(3));
    assert_eq!(bits(&three), bits(&[1.0, -1.0, U, 2.0, -2.0, U]));

    let mut planar = [U; 7];
    kithara_dsp::deinterleave_channel_major(
        &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0],
        count(2),
        &mut planar,
        stride(3),
        1..5,
    );
    assert_eq!(bits(&planar), bits(&[U, 1.0, 2.0, U, -1.0, -2.0, U]));
}

#[kithara::test]
fn a_short_plane_bounds_the_variable_split_instead_of_panicking() {
    const U: f32 = UNWRITTEN;
    let mut mono = [[U; 3]];
    kithara_dsp::deinterleave_variable(&[1.0, 2.0], count(1), &mut mono, 0..3);
    assert_eq!(bits(&mono[0]), bits(&[1.0, 2.0, U]));

    let mut stereo = [[U; 4]; 2];
    kithara_dsp::deinterleave_variable(&[1.0, -1.0, 2.0, -2.0, 3.0], count(2), &mut stereo, 0..4);
    assert_eq!(bits(&stereo[0]), bits(&[1.0, 2.0, U, U]));
    assert_eq!(bits(&stereo[1]), bits(&[-1.0, -2.0, U, U]));

    let mut three = [[U; 3]; 2];
    kithara_dsp::deinterleave_variable(
        &[1.0, -1.0, 9.0, 2.0, -2.0, 9.0, 3.0, -3.0, 9.0],
        count(3),
        &mut three,
        1..5,
    );
    assert_eq!(bits(&three[0]), bits(&[U, 1.0, 2.0]));
    assert_eq!(bits(&three[1]), bits(&[U, -1.0, -2.0]));
}

#[kithara::test]
fn downmix_is_the_channel_mean_of_every_whole_frame() {
    for channels in 1..=MAX_CHANNELS {
        for frames in FRAMES {
            let interleaved: Vec<f32> = (0..frames * channels + channels - 1)
                .map(|slot| sample(0, slot))
                .collect();
            let mut mono = vec![UNWRITTEN; frames + TAIL];
            assert_eq!(
                kithara_dsp::downmix(&interleaved, count(channels), &mut mono),
                frames,
                "{channels} channels, {frames} frames"
            );
            for (frame, (got, samples)) in mono
                .iter()
                .zip(interleaved.chunks_exact(channels))
                .enumerate()
            {
                let want = channel_mean(samples);
                assert!(
                    got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()),
                    "{channels} channels, frame {frame} of {frames}: {got} against {want}"
                );
            }
            assert_eq!(
                bits(&mono[frames..]),
                bits(&[UNWRITTEN; TAIL]),
                "{channels} channels wrote past {frames} frames"
            );
        }
    }
}

#[kithara::test]
fn a_short_mono_bounds_the_downmix() {
    const U: f32 = UNWRITTEN;
    let mut mono = [U; 2];
    assert_eq!(
        kithara_dsp::downmix(&[1.0, 3.0, 5.0, 7.0, 9.0, 11.0], count(2), &mut mono),
        2
    );
    assert_eq!(bits(&mono), bits(&[2.0, 6.0]));

    let mut mono = [U; 3];
    assert_eq!(
        kithara_dsp::downmix(&[1.0, 2.0, 3.0, 4.0], count(3), &mut mono),
        1
    );
    assert_eq!(bits(&mono), bits(&[2.0, U, U]));
}
