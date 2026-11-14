use kithara::signal::sanitize_sample;

const BLOCK: u32 = 1 << 16;
const BLOCK_LEN: usize = 1 << 16;
const BLOCKS: u32 = 1 << 16;

#[kithara::test(native, timeout(::std::time::Duration::from_millis(1908)))]
#[case(0)]
#[case(1)]
#[case(2)]
#[case(3)]
#[case(4)]
#[case(5)]
#[case(6)]
#[case(7)]
#[case(8)]
#[case(9)]
#[case(10)]
#[case(11)]
#[case(12)]
#[case(13)]
#[case(14)]
#[case(15)]
fn platform_sanitize_matches_signal_on_every_bit_pattern(#[case] first: u32) {
    check_blocks(first);
}

fn check_blocks(first: u32) {
    let mut samples = vec![0.0_f32; BLOCK_LEN];
    let mut expected = vec![0_u32; BLOCK_LEN];
    for block in (first..BLOCKS).step_by(16) {
        let base = block << 16;
        let patterns = (0..BLOCK).map(|offset| base | offset);
        for ((sample, expected), bits) in
            samples.iter_mut().zip(&mut expected).zip(patterns.clone())
        {
            *sample = f32::from_bits(bits);
            *expected = sanitize_sample(*sample).to_bits();
        }
        kithara::dsp::sanitize(&mut samples);
        let diff = samples
            .iter()
            .zip(&expected)
            .fold(0, |diff, (sample, expected)| {
                diff | (sample.to_bits() ^ expected)
            });
        if diff != 0 {
            let (bits, sample, expected) = patterns
                .zip(&samples)
                .zip(&expected)
                .map(|((bits, sample), expected)| (bits, sample.to_bits(), *expected))
                .find(|(_, sample, expected)| sample != expected)
                .expect("a nonzero difference has a mismatching sample");
            panic!("pattern {bits:#010x}: sanitized {sample:#010x}, expected {expected:#010x}");
        }
    }
}
