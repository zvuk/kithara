use kithara_test_utils::kithara;

use super::Fmp4Layout;
use crate::{
    consts,
    fixture::{CountingSource, fragmented_mp4},
};

#[kithara::test]
fn layout_walks_headers_without_reading_the_payload() {
    let (bytes, first_moof) = fragmented_mp4();
    let source = CountingSource::new(bytes);
    let total = source.total();

    let layout = Fmp4Layout::read(&source, total).expect("fragmented mp4 layout");

    let delivered = source.delivered();
    assert!(
        delivered < consts::WALK_BUDGET_BYTES,
        "layout walk pulled {delivered} bytes from a {total}-byte file; \
         the mdat payload must be seeked over, not read"
    );
    assert_eq!(layout.init_range(), 0..first_moof);
    assert_eq!(layout.timescale(), consts::TIMESCALE);
    assert_eq!(
        u32::try_from(layout.fragments().len()).expect("fragment count fits u32"),
        consts::FRAGMENTS
    );
}

#[kithara::test]
fn fragments_carry_contiguous_ticks_and_byte_ranges() {
    let (bytes, first_moof) = fragmented_mp4();
    let source = CountingSource::new(bytes);
    let total = source.total();

    let layout = Fmp4Layout::read(&source, total).expect("fragmented mp4 layout");

    let fragment_ticks = u64::from(consts::SAMPLES_PER_FRAGMENT) * u64::from(consts::SAMPLE_TICKS);
    let mut expected_start = first_moof;
    for (idx, fragment) in layout.fragments().iter().enumerate() {
        let index = u64::try_from(idx).expect("fragment index fits u64");
        assert_eq!(fragment.decode_ticks, index * fragment_ticks);
        assert_eq!(fragment.duration_ticks, fragment_ticks);
        assert_eq!(fragment.byte_range.start, expected_start);
        expected_start = fragment.byte_range.end;
    }
    assert_eq!(expected_start, total, "fragments must cover the whole tail");
}
