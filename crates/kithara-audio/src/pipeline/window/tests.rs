use std::num::NonZeroU32;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_test_utils::kithara;

use super::{SourceEnd, SourceWindow};

const RATE: u32 = 48_000;

fn chunk(frame_offset: u64, frames: u32) -> PcmChunk {
    let channels = 2_u16;
    let sample_count = usize::try_from(frames)
        .expect("test frame count fits usize")
        .saturating_mul(usize::from(channels));
    let samples = vec![0.25; sample_count];
    PcmChunk::new(
        PcmMeta {
            frame_offset,
            frames,
            spec: PcmSpec::new(
                channels,
                NonZeroU32::new(RATE).expect("test sample rate is non-zero"),
            ),
            ..Default::default()
        },
        PcmPool::default().attach(samples),
    )
}

#[kithara::test]
fn admit_moves_the_chunk_unchanged_and_advances_the_endpoint() {
    let mut window = SourceWindow::default();
    let input = chunk(40, 8);
    let samples = input.samples.as_ptr();
    let meta = input.meta;

    let output = window.admit(input);

    assert_eq!(output.samples.as_ptr(), samples);
    assert_eq!(output.meta, meta);
    assert_eq!(
        window.emitted(0),
        Some(SourceEnd {
            frame: 48,
            rate: RATE,
        })
    );

    let output = window.admit(chunk(48, 5));
    assert_eq!(output.meta.frame_offset, 48);
    assert_eq!(
        window.emitted(0),
        Some(SourceEnd {
            frame: 53,
            rate: RATE,
        })
    );
}

#[kithara::test]
fn emitted_saturates_at_zero() {
    let mut window = SourceWindow::default();
    let _ = window.admit(chunk(0, 8));

    assert_eq!(
        window.emitted(20),
        Some(SourceEnd {
            frame: 0,
            rate: RATE,
        })
    );
}

#[kithara::test]
fn clear_removes_the_endpoint() {
    let mut window = SourceWindow::default();
    let _ = window.admit(chunk(12, 4));

    window.clear();

    assert_eq!(window.emitted(0), None);
}
