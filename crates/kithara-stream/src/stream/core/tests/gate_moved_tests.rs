use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;

use super::{ActivityWriter, DeferredWake, DummyType, ScriptSource, Source, Stream, WaitOutcome};
const INIT_BYTES: u64 = 627;
const SEGMENT_BYTES: u64 = 8_000;
const READ_AHEAD_BYTES: usize = 32 * 1024;
fn waited(pos: u64, segmented: bool) -> std::ops::Range<u64> {
    let mut source = ScriptSource::new(
        ActivityWriter::new(),
        [WaitOutcome::Interrupted],
        [],
        vec![0; READ_AHEAD_BYTES],
    );
    if segmented {
        source = source
            .with_segments(
                (0..3).map(|index| {
                    let start = INIT_BYTES + index * SEGMENT_BYTES;
                    start..start + SEGMENT_BYTES
                }),
                u64::MAX,
            )
            .with_peer_wake(Arc::new(DeferredWake::default()));
    }
    source.set_position(pos);
    let mut stream = Stream::<DummyType> { source };
    let _ = stream
        .try_read(&mut [0; READ_AHEAD_BYTES])
        .expect("range probe");
    assert_eq!(stream.source.waited.len(), 1);
    stream.source.waited.remove(0)
}
#[kithara::test(native, flash(false))]
fn a_wait_on_a_segment_boundary_ends_at_that_segment() {
    let window = waited(INIT_BYTES, true);
    assert_eq!(window, INIT_BYTES..INIT_BYTES + SEGMENT_BYTES);
}
#[kithara::test(native, flash(false))]
fn a_wait_inside_a_segment_ends_at_its_read_boundary() {
    let window = waited(INIT_BYTES + SEGMENT_BYTES / 2, true);
    assert_eq!(window.end, INIT_BYTES + SEGMENT_BYTES);
}
#[kithara::test(native, flash(false))]
fn a_wait_on_a_source_with_no_segments_spans_the_read_ahead_window() {
    let pos = INIT_BYTES;
    let window = waited(pos, false);
    assert_eq!(window, pos..pos + READ_AHEAD_BYTES as u64);
}
