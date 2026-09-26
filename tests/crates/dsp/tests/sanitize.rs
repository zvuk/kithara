use std::{num::NonZeroUsize, thread};

use kithara_dsp::{Backend, Platform};
use kithara_signal::sanitize_sample;
use kithara_test_utils::kithara;

const BLOCK: u32 = 1 << 16;
const BLOCK_LEN: usize = 1 << 16;
const BLOCKS: u32 = 1 << 16;

#[kithara::test(native, flash(false))]
fn platform_sanitize_matches_signal_on_every_bit_pattern() {
    let workers = thread::available_parallelism().map_or(1, NonZeroUsize::get);
    thread::scope(|scope| {
        for first in 0..workers {
            scope.spawn(move || check_blocks(first, workers));
        }
    });
}

fn check_blocks(first: usize, step: usize) {
    let backend = Platform::default();
    let mut samples = vec![0.0_f32; BLOCK_LEN];
    for block in (0..BLOCKS).skip(first).step_by(step) {
        let base = block << 16;
        let patterns = base..=base | (BLOCK - 1);
        for (sample, bits) in samples.iter_mut().zip(patterns.clone()) {
            *sample = f32::from_bits(bits);
        }
        backend.sanitize(&mut samples);
        for (sample, bits) in samples.iter().zip(patterns) {
            let expected = sanitize_sample(f32::from_bits(bits));
            assert_eq!(sample.to_bits(), expected.to_bits(), "pattern {bits:#010x}");
        }
    }
}
