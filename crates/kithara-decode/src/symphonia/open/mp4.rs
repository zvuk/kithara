use std::{
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::atomic::{AtomicUsize, Ordering},
};

use kithara_platform::sync::Arc;
use kithara_stream::PendingReason;
use kithara_test_fixtures::{SignalAsset, assets::by_name};
use kithara_test_utils::kithara;
use symphonia::core::io::MediaSource;

use crate::{
    demuxer::{DemuxOutcome, Demuxer},
    symphonia::SymphoniaDemuxer,
};

struct Source {
    reads: Arc<AtomicUsize>,
    remaining: Arc<AtomicUsize>,
    cursor: Cursor<Vec<u8>>,
}

impl Read for Source {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let remaining = self.remaining.load(Ordering::Relaxed);
        if remaining == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let len = out.len().min(remaining).min(53);
        let read = self.cursor.read(&mut out[..len])?;
        if remaining != usize::MAX {
            self.remaining.fetch_sub(read, Ordering::Relaxed);
        }
        Ok(read)
    }
}

impl Seek for Source {
    delegate::delegate! {
        to self.cursor {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
        }
    }
}

impl MediaSource for Source {
    fn byte_len(&self) -> Option<u64> {
        Some(self.cursor.get_ref().len() as u64)
    }

    fn is_seekable(&self) -> bool {
        true
    }
}

fn reader(remaining: Arc<AtomicUsize>, reads: Arc<AtomicUsize>) -> SymphoniaDemuxer {
    reader_for(
        SignalAsset::M4A_SINE440_60S,
        remaining,
        reads,
        kithara_stream::ReaderControl::new(None),
    )
}

fn reader_for(
    signal: SignalAsset,
    remaining: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
    control: kithara_stream::ReaderControl,
) -> SymphoniaDemuxer {
    let asset = by_name(signal.name()).expect("signal fixture");
    let source = Source {
        remaining,
        reads,
        cursor: Cursor::new(asset.bytes().to_vec()),
    };
    SymphoniaDemuxer::open_file(
        source,
        super::FileOpen {
            byte_len_handle: None,
            byte_map: None,
            container: kithara_stream::ContainerFormat::parse_extension(signal.ext()),
            hint: Some(signal.ext().to_owned()),
            control,
        },
    )
    .expect("signal production bootstrap")
    .0
}

#[kithara::test]
fn mp4_prepared_frame_consumption_does_not_read_the_source() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut demuxer = reader(Arc::new(AtomicUsize::new(usize::MAX)), Arc::clone(&reads));
    let before = reads.load(Ordering::Relaxed);
    for _ in 0..128 {
        demuxer.prepare_frame().expect("prepare packet");
        let prepared_reads = reads.load(Ordering::Relaxed);
        assert!(matches!(
            demuxer.next_frame_prepared().expect("prepared packet"),
            DemuxOutcome::Frame(_)
        ));
        assert_eq!(reads.load(Ordering::Relaxed), prepared_reads);
        assert!(matches!(
            demuxer.next_frame_prepared().expect("preparation required"),
            DemuxOutcome::Pending(PendingReason::Retry)
        ));
        assert_eq!(reads.load(Ordering::Relaxed), prepared_reads);
    }
    assert!(
        reads.load(Ordering::Relaxed) > before,
        "preparation must exercise source reads"
    );
}

#[kithara::test]
#[case(SignalAsset::WAV_SINE440_60S)]
#[case(SignalAsset::M4A_SINE440_60S)]
#[kithara::hang_watchdog]
fn packet_retry_preserves_bytes_and_timestamps_after_partial_read(#[case] signal: SignalAsset) {
    let remaining = Arc::new(AtomicUsize::new(usize::MAX));
    let control = kithara_stream::ReaderControl::new(None);
    let mut actual = reader_for(
        signal,
        Arc::clone(&remaining),
        Arc::new(AtomicUsize::new(0)),
        control.clone(),
    );
    let mut expected = reader_for(
        signal,
        Arc::new(AtomicUsize::new(usize::MAX)),
        Arc::new(AtomicUsize::new(0)),
        kithara_stream::ReaderControl::new(None),
    );
    remaining.store(17, Ordering::Relaxed);
    let mut interrupted = false;
    let mut completed = 0;
    while completed < 128 {
        hang_tick!();
        actual
            .prepare_frame_with_wait(crate::PrepareWait::Poll)
            .expect("poll packet");
        let packet = match actual.next_frame_prepared().expect("packet read") {
            DemuxOutcome::Pending(
                PendingReason::Retry
                | PendingReason::NotReady(kithara_stream::NotReadyCause::OperationPending),
            ) => {
                kithara_platform::thread::yield_runnable();
                continue;
            }
            DemuxOutcome::Pending(_) => {
                interrupted = true;
                remaining.store(usize::MAX, Ordering::Relaxed);
                control.notify();
                continue;
            }
            DemuxOutcome::Frame(frame) => frame,
            DemuxOutcome::Eof => panic!("unexpected EOF"),
        };
        let DemuxOutcome::Frame(reference) = expected.next_frame().expect("reference read") else {
            panic!("expected reference packet");
        };
        assert_eq!(packet.pts, reference.pts);
        assert_eq!(packet.duration, reference.duration);
        assert_eq!(packet.data, reference.data);
        completed += 1;
        if completed == 128 {
            break;
        }
    }
    assert!(interrupted, "must interrupt a source read");
    assert_eq!(completed, 128);
}
