use std::num::NonZeroUsize;

use kithara_bufpool::PoolError;
use kithara_test_utils::{
    bufpool::{pools, pools_with_budget},
    kithara,
};

use super::{DelayError, DelayLine};

const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);
const FIVE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(4);

fn line(capacity: NonZeroUsize) -> DelayLine {
    DelayLine::new(TWO, capacity, &pools()).expect("small line fits")
}

/// Stereo input whose channels never coincide, so a crossed channel shows.
fn input(frame: usize) -> [f32; 2] {
    let base = f32::from(u16::try_from(frame % 97).expect("small index"));
    [base.mul_add(0.01, 0.1), base.mul_add(-0.02, -0.3)]
}

/// One channel of [`input`].
fn sample(frame: usize, channel: usize) -> f32 {
    *input(frame).get(channel).expect("stereo channel")
}

fn delay(frames: usize) -> f64 {
    f64::from(u32::try_from(frames).expect("small delay"))
}

/// Writes one stereo frame.
fn push(line: &mut DelayLine, frame: [f32; 2]) {
    line.write(&[&frame[..1], &frame[1..]], 0..1)
        .expect("planes match");
}

fn read_frame(line: &DelayLine, tap: super::Tap) -> [f32; 2] {
    let mut frame = [0.0; 2];
    for (channel, sample) in frame.iter_mut().enumerate() {
        line.read(channel, tap, std::slice::from_mut(sample))
            .expect("one frame fits every tap");
    }
    frame
}

#[kithara::test]
fn a_delay_reads_the_frame_written_that_many_writes_ago() {
    for frames in 1..=FIVE.get() {
        let mut line = line(FIVE);
        let tap = line.tap(delay(frames)).expect("tap in range");
        for n in 0_usize..23 {
            let expected = n.checked_sub(frames).map_or([0.0; 2], input);
            assert_eq!(
                read_frame(&line, tap),
                expected,
                "delay {frames}, frame {n}"
            );
            push(&mut line, input(n));
        }
    }
}

#[kithara::test]
fn a_fractional_tap_is_the_chord_toward_the_older_frame() {
    let mut line = line(FIVE);
    let tap = line.tap(2.25).expect("tap in range");
    assert_eq!(tap.whole(), 2);
    for n in 0_usize..17 {
        let frame = read_frame(&line, tap);
        if let (Some(newer), Some(older)) = (n.checked_sub(2), n.checked_sub(3)) {
            for ((sample, newer), older) in frame.iter().zip(input(newer)).zip(input(older)) {
                let expected = 0.25_f32.mul_add(older - newer, newer);
                assert_eq!(sample.to_bits(), expected.to_bits(), "frame {n}");
            }
        }
        push(&mut line, input(n));
    }
}

/// `y[n] = x[n] + g·y[n − d]` run in spans of every length up to `d`, against
/// a plain history vector, so spans cross the wrap at every offset.
#[kithara::test]
fn a_feedback_recurrence_in_spans_matches_the_scalar_reference() {
    let gain = 0.75_f32;
    for frames in [1_usize, 3, 5] {
        for span in 1..=frames {
            let mut line = line(FIVE);
            let tap = line.tap(delay(frames)).expect("tap in range");
            let mut history: Vec<[f32; 2]> = Vec::new();
            while history.len() < 64 {
                let [mut left, mut right] = [vec![0.0; span], vec![0.0; span]];
                line.read(0, tap, &mut left).expect("span within delay");
                line.read(1, tap, &mut right).expect("span within delay");
                let [mut feed_left, mut feed_right] = [Vec::new(), Vec::new()];
                for (echo_left, echo_right) in left.iter().zip(&right) {
                    let n = history.len();
                    let [x_left, x_right] = if n == 0 { [1.0, -0.5] } else { [0.0; 2] };
                    let [past_left, past_right] = n.checked_sub(frames).map_or([0.0; 2], |past| {
                        *history.get(past).expect("history covers the past")
                    });
                    let expected = [
                        gain.mul_add(past_left, x_left),
                        gain.mul_add(past_right, x_right),
                    ];
                    let actual = [
                        gain.mul_add(*echo_left, x_left),
                        gain.mul_add(*echo_right, x_right),
                    ];
                    assert_eq!(actual, expected, "d {frames}, span {span}, n {n}");
                    let [actual_left, actual_right] = actual;
                    feed_left.push(actual_left);
                    feed_right.push(actual_right);
                    history.push(expected);
                }
                line.write(&[feed_left, feed_right], 0..span)
                    .expect("planes match");
            }
            let mut level = 1.0_f32;
            for [left, _] in history.iter().step_by(frames).skip(1) {
                level *= gain;
                assert_eq!(*left, level, "d {frames}, span {span}");
            }
        }
    }
}

#[kithara::test]
fn a_span_longer_than_its_delay_is_refused_untouched() {
    let line = line(FIVE);
    let tap = line.tap(2.5).expect("tap in range");
    let mut output = [7.0; 3];
    assert_eq!(line.read(0, tap, &mut output), Err(DelayError::Shape));
    assert_eq!(line.read(2, tap, &mut output[..1]), Err(DelayError::Shape));
    assert_eq!(output, [7.0; 3]);
}

#[kithara::test]
fn a_write_longer_than_the_capacity_keeps_its_last_frames() {
    let mut line = line(FIVE);
    push(&mut line, [9.0, 9.0]);
    let planes: [Vec<f32>; 2] =
        std::array::from_fn(|channel| (0..13).map(|n| sample(n, channel)).collect());
    line.write(&planes, 1..13).expect("planes cover the range");
    for frames in 1..=FIVE.get() {
        let tap = line.tap(delay(frames)).expect("tap in range");
        let written = 13_usize
            .checked_sub(frames)
            .expect("delay within the write");
        assert_eq!(read_frame(&line, tap), input(written));
    }
}

#[kithara::test]
fn a_mismatched_write_is_refused_and_leaves_the_history() {
    let mut line = line(FIVE);
    for n in 0..FIVE.get() {
        push(&mut line, input(n));
    }
    let loud = [9.0_f32; 3];
    assert_eq!(line.write(&[&loud[..]], 0..1), Err(DelayError::Shape));
    assert_eq!(
        line.write(&[&loud[..], &loud[..1]], 0..2),
        Err(DelayError::Shape)
    );
    assert_eq!(
        line.write(&[&loud[..], &loud[..]], 2..4),
        Err(DelayError::Shape)
    );
    for frames in 1..=FIVE.get() {
        let tap = line.tap(delay(frames)).expect("tap in range");
        let written = FIVE
            .get()
            .checked_sub(frames)
            .expect("delay within the history");
        assert_eq!(read_frame(&line, tap), input(written), "delay {frames}");
    }
}

#[kithara::test]
fn a_write_leaves_samples_outside_its_range_untouched() {
    let mut line = line(FIVE);
    let planes = [[1.0_f32, 2.0, 3.0], [4.0, 5.0, 6.0]];
    line.write(&[&planes[0][..], &planes[1][..]], 1..2)
        .expect("planes cover the range");
    let tap = line.tap(1.0).expect("tap in range");
    assert_eq!(read_frame(&line, tap), [2.0, 5.0]);
    let tap = line.tap(2.0).expect("tap in range");
    assert_eq!(read_frame(&line, tap), [0.0, 0.0]);
}

#[kithara::test]
#[case::zero(0.0)]
#[case::below_one_frame(0.5)]
#[case::past_capacity(5.5)]
#[case::whole_past_capacity(6.0)]
#[case::not_finite(f64::NAN)]
#[case::infinite(f64::INFINITY)]
fn a_tap_outside_the_capacity_is_refused(#[case] frames: f64) {
    assert_eq!(line(FIVE).tap(frames), Err(DelayError::OutOfRange));
}

#[kithara::test]
fn the_line_takes_exactly_channels_times_capacity_samples() {
    assert!(matches!(
        DelayLine::new(TWO, NonZeroUsize::MAX, &pools()),
        Err(PoolError::CapacityOverflow { .. })
    ));
    let bytes = size_of::<f32>()
        .checked_mul(TWO.get())
        .and_then(|plane| plane.checked_mul(FIVE.get()))
        .expect("small line");
    let short = bytes.checked_sub(1).expect("nonzero line");
    assert!(matches!(
        DelayLine::new(TWO, FIVE, &pools_with_budget(short)),
        Err(PoolError::OverallBudgetExceeded { .. })
    ));
    DelayLine::new(TWO, FIVE, &pools_with_budget(bytes))
        .expect("a region of exactly the line's samples holds it");
}

/// The storage of a dropped line goes back to the region with its history;
/// the next line drawn from it still starts silent.
#[kithara::test]
fn a_line_from_reused_storage_starts_silent() {
    let region = pools();
    let mut used = DelayLine::new(TWO, FIVE, &region).expect("small line fits");
    for n in 0..7 {
        push(&mut used, [9.0, -9.0]);
        push(&mut used, input(n));
    }
    drop(used);
    let fresh = DelayLine::new(TWO, FIVE, &region).expect("small line fits");
    for frames in 1..=FIVE.get() {
        let tap = fresh.tap(delay(frames)).expect("tap in range");
        assert_eq!(read_frame(&fresh, tap), [0.0; 2], "delay {frames}");
    }
}

#[kithara::test]
fn a_tap_reaching_past_a_shorter_line_is_refused_untouched() {
    let long = line(NonZeroUsize::new(64).expect("nonzero"));
    let mut short = line(FIVE);
    push(&mut short, [1.0, -1.0]);
    let mut output = [7.0; 40];
    for frames in [64.0, 6.0, 5.5] {
        let tap = long.tap(frames).expect("tap in range");
        assert_eq!(short.read(0, tap, &mut output), Err(DelayError::OutOfRange));
        assert_eq!(
            short.read(0, tap, &mut output[..1]),
            Err(DelayError::OutOfRange)
        );
    }
    assert_eq!(output, [7.0; 40]);
    let within = long.tap(4.5).expect("tap in range");
    short
        .read(0, within, &mut output[..4])
        .expect("tap within the short line");
}

/// Fractional taps read in spans of every length up to their whole delay,
/// including one whose older neighbour is the oldest frame, against the
/// chord of a plain history vector across several wraps.
#[kithara::test]
#[case::short(1.5, 0.5)]
#[case::quarter(2.25, 0.25)]
#[case::at_capacity(4.5, 0.5)]
fn fractional_spans_match_the_scalar_chord_across_wraps(
    #[case] frames: f64,
    #[case] fraction: f32,
) {
    for span in 1..=4_usize {
        let mut line = line(FIVE);
        let tap = line.tap(frames).expect("tap in range");
        if span > tap.whole() {
            continue;
        }
        let mut written = 0_usize;
        while written < 31 {
            for channel in 0..2 {
                let mut output = vec![0.0; span];
                line.read(channel, tap, &mut output)
                    .expect("span within delay");
                for (offset, actual) in output.iter().enumerate() {
                    let n = written.wrapping_add(offset);
                    let at = |back: usize| {
                        n.checked_sub(back)
                            .map_or(0.0, |past| sample(past, channel))
                    };
                    let newer = at(tap.whole());
                    let older = at(tap.whole().wrapping_add(1));
                    let expected = fraction.mul_add(older - newer, newer);
                    assert_eq!(
                        actual.to_bits(),
                        expected.to_bits(),
                        "{frames}, span {span}, n {n}"
                    );
                }
            }
            let block = written..written.wrapping_add(span);
            let planes: [Vec<f32>; 2] =
                std::array::from_fn(|channel| block.clone().map(|n| sample(n, channel)).collect());
            line.write(&planes, 0..span).expect("planes cover the span");
            written = block.end;
        }
    }
}

#[kithara::test]
fn reset_returns_to_silence_and_the_first_cursor() {
    let mut used = line(FIVE);
    for n in 0..7 {
        push(&mut used, input(n));
    }
    used.reset();
    let mut fresh = line(FIVE);
    let tap = fresh.tap(2.0).expect("tap in range");
    for n in 0..11 {
        assert_eq!(read_frame(&used, tap), read_frame(&fresh, tap), "frame {n}");
        push(&mut used, input(n));
        push(&mut fresh, input(n));
    }
}

/// A pure delay of `lookahead` frames written a block at a time and read back
/// after the write at `block + lookahead`, so zero lookahead takes the same
/// path; blocks of irregular length cross the wrap at varying offsets.
#[kithara::test]
#[case::no_lookahead(0)]
#[case::three_frames(3)]
fn a_block_read_after_its_write_is_delayed_by_the_lookahead(#[case] lookahead: usize) {
    let capacity = NonZeroUsize::new(7)
        .and_then(|block| block.checked_add(lookahead))
        .expect("capacity fits the longest block and the lookahead");
    let mut line = line(capacity);
    let mut written = 0_usize;
    for block in [1_usize, 7, 2, 5, 7, 3, 6, 4, 7, 1] {
        let frames = written..written.wrapping_add(block);
        let planes: [Vec<f32>; 2] =
            std::array::from_fn(|channel| frames.clone().map(|n| sample(n, channel)).collect());
        line.write(&planes, 0..block)
            .expect("planes cover the block");
        let tap = line
            .tap(delay(block.wrapping_add(lookahead)))
            .expect("tap in range");
        for channel in 0..2 {
            let mut output = vec![0.0; block];
            line.read(channel, tap, &mut output)
                .expect("span within delay");
            let expected: Vec<f32> = frames
                .clone()
                .map(|n| {
                    n.checked_sub(lookahead)
                        .map_or(0.0, |past| sample(past, channel))
                })
                .collect();
            assert_eq!(output, expected, "lookahead {lookahead}, frame {written}");
        }
        written = frames.end;
    }
}

#[kithara::test]
fn debug_names_the_shape_without_the_history() {
    let text = format!("{:?}", line(FIVE));
    assert_eq!(
        text,
        "DelayLine { channels: 2, capacity: 5, cursor: 0, .. }"
    );
}
