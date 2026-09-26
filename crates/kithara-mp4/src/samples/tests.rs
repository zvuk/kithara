use kithara_test_utils::kithara;

use super::read_samples;
use crate::{
    consts,
    fixture::{CountingSource, mdat_bytes, media_segment},
};

/// A media segment carries no `moov`, so this is the walk a whole-file
/// parser cannot do: `moof` then `mdat`, addressed by `trun`.
#[kithara::test]
fn samples_of_a_media_segment_tile_the_mdat_payload() {
    let source = CountingSource::new(media_segment(0));
    let total = source.total();

    let samples = read_samples(&source, total, consts::TRACK_ID).expect("media segment samples");

    assert_eq!(
        u32::try_from(samples.len()).expect("sample count fits u32"),
        consts::SAMPLES_PER_FRAGMENT
    );

    // The first sample starts where the `mdat` payload does: everything
    // before it is the `moof` plus the `mdat` header.
    let payload_start = total - u64::try_from(mdat_bytes()).expect("test mdat fits u64");
    let mut expected_start = payload_start;
    for (idx, sample) in samples.iter().enumerate() {
        let index = u64::try_from(idx).expect("sample index fits u64");
        assert_eq!(sample.byte_range.start, expected_start);
        assert_eq!(
            sample.byte_range.end - sample.byte_range.start,
            u64::from(consts::SAMPLE_BYTES)
        );
        assert_eq!(sample.decode_ticks, index * u64::from(consts::SAMPLE_TICKS));
        assert_eq!(sample.duration_ticks, consts::SAMPLE_TICKS);
        expected_start = sample.byte_range.end;
    }
    assert_eq!(
        expected_start, total,
        "samples must tile the whole mdat payload"
    );
}

/// A media segment past the first carries its own `tfdt`, so its samples
/// come out on the track timeline rather than restarting at zero.
#[kithara::test]
fn a_later_segment_keeps_its_absolute_decode_time() {
    const INDEX: u32 = 3;

    let source = CountingSource::new(media_segment(INDEX));
    let total = source.total();

    let samples = read_samples(&source, total, consts::TRACK_ID).expect("media segment samples");
    let first = samples.first().expect("segment has samples");

    assert_eq!(
        first.decode_ticks,
        u64::from(INDEX)
            * u64::from(consts::SAMPLES_PER_FRAGMENT)
            * u64::from(consts::SAMPLE_TICKS)
    );
}

/// The sample walk reads headers too: it never pulls the payload it is
/// describing.
#[kithara::test]
fn the_sample_walk_does_not_read_the_payload() {
    let source = CountingSource::new(media_segment(0));
    let total = source.total();

    read_samples(&source, total, consts::TRACK_ID).expect("media segment samples");

    let delivered = source.delivered();
    assert!(
        delivered < consts::WALK_BUDGET_BYTES,
        "sample walk pulled {delivered} bytes from a {total}-byte segment; \
         the mdat payload must be seeked over, not read"
    );
}

/// An unknown track falls back to the segment's only `traf`, which is how a
/// single-track segment addresses itself.
#[kithara::test]
fn an_unknown_track_falls_back_to_the_only_traf() {
    let source = CountingSource::new(media_segment(0));
    let total = source.total();

    let samples =
        read_samples(&source, total, consts::TRACK_ID + 99).expect("media segment samples");

    assert_eq!(
        u32::try_from(samples.len()).expect("sample count fits u32"),
        consts::SAMPLES_PER_FRAGMENT
    );
}
