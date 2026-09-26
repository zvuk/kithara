use std::{
    io::{self, ErrorKind, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use kithara_mpa::MpaReader;
use kithara_test_fixtures::mock_fixtures::{mpeg_eight, mpeg_four};
use kithara_test_utils::kithara;
use symphonia::core::{
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo},
    io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
    packet::Packet,
    units::Timestamp,
};

use crate::consts;

struct SourceState {
    interrupt_at: Option<usize>,
    bytes: Vec<u8>,
    interrupts_remaining: usize,
    pos: usize,
}

struct SourceControl {
    state: Arc<Mutex<SourceState>>,
}

impl SourceControl {
    fn arm_after(&self, offset: usize, count: usize) {
        let mut state = lock(&self.state);
        let interrupt_at = state.pos.saturating_add(offset);
        state.interrupt_at = Some(interrupt_at);
        state.interrupts_remaining = count;
    }

    fn arm_at(&self, pos: usize, count: usize) {
        let mut state = lock(&self.state);
        assert!(
            state.pos <= pos,
            "interruption target {pos} already passed (source at {})",
            state.pos
        );
        state.interrupt_at = Some(pos);
        state.interrupts_remaining = count;
    }
}

struct InterruptingSource {
    state: Arc<Mutex<SourceState>>,
}

impl InterruptingSource {
    const READ_CHUNK: usize = 53;

    fn new(bytes: Vec<u8>) -> (Self, SourceControl) {
        let state = Arc::new(Mutex::new(SourceState {
            bytes,
            interrupt_at: None,
            interrupts_remaining: 0,
            pos: 0,
        }));
        let control = SourceControl {
            state: Arc::clone(&state),
        };
        (Self { state }, control)
    }
}

impl Read for InterruptingSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = lock(&self.state);

        if state.interrupts_remaining > 0 && state.interrupt_at == Some(state.pos) {
            state.interrupts_remaining -= 1;
            return Err(io::Error::new(
                ErrorKind::Interrupted,
                consts::INTERRUPTED_MESSAGE,
            ));
        }

        let start = state.pos;
        let mut end = start
            .saturating_add(buf.len().min(Self::READ_CHUNK))
            .min(state.bytes.len());
        if state.interrupts_remaining > 0
            && let Some(interrupt_at) = state.interrupt_at
            && start < interrupt_at
        {
            end = end.min(interrupt_at);
        }

        let read = end - start;
        buf[..read].copy_from_slice(&state.bytes[start..end]);
        state.pos = end;
        Ok(read)
    }
}

impl Seek for InterruptingSource {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "synthetic source is forward-only",
        ))
    }
}

impl MediaSource for InterruptingSource {
    fn byte_len(&self) -> Option<u64> {
        None
    }

    fn is_seekable(&self) -> bool {
        false
    }
}

fn lock(state: &Arc<Mutex<SourceState>>) -> MutexGuard<'_, SourceState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn mpa_reader(source: InterruptingSource) -> MpaReader<'static> {
    let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    match MpaReader::try_new(stream, FormatOptions::default()) {
        Ok(reader) => reader,
        Err(error) => panic!("synthetic MPEG stream must open: {error}"),
    }
}

fn next_packet(reader: &mut MpaReader<'_>) -> Packet {
    match reader.next_packet() {
        Ok(Some(packet)) => packet,
        Ok(None) => panic!("synthetic MPEG stream ended early"),
        Err(error) => panic!("synthetic MPEG packet failed: {error}"),
    }
}

fn assert_packet_eq(actual: &Packet, expected: &Packet) {
    assert_eq!(actual.pts, expected.pts);
    assert_eq!(actual.dur, expected.dur);
    assert_eq!(actual.data.as_ref(), expected.data.as_ref());
}

fn assert_interrupted(reader: &mut MpaReader<'_>) {
    match reader.next_packet() {
        Err(SymphoniaError::IoError(error)) => {
            assert_eq!(error.kind(), ErrorKind::Interrupted);
            assert_eq!(error.to_string(), consts::INTERRUPTED_MESSAGE);
        }
        Ok(_) | Err(_) => panic!("packet read must return the original interruption"),
    }
}

fn seek_to(ts: i64) -> SeekTo {
    SeekTo::Timestamp {
        ts: Timestamp::new(ts),
        track_id: 0,
    }
}

fn seek_once(reader: &mut MpaReader<'_>, ts: i64) -> SeekedTo {
    match reader.seek(SeekMode::Accurate, seek_to(ts)) {
        Ok(seeked) => seeked,
        Err(error) => panic!("synthetic MPEG seek failed: {error}"),
    }
}

fn seek_after_interruption(reader: &mut MpaReader<'_>, ts: i64) -> SeekedTo {
    match reader.seek(SeekMode::Accurate, seek_to(ts)) {
        Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::Interrupted => {}
        Ok(_) => panic!("armed interruption must fire during the seek scan"),
        Err(error) => panic!("synthetic MPEG seek failed: {error}"),
    }
    for _ in 0..consts::MAX_SEEK_RETRIES {
        match reader.seek(SeekMode::Accurate, seek_to(ts)) {
            Ok(seeked) => return seeked,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => panic!("synthetic MPEG seek failed: {error}"),
        }
    }
    panic!(
        "synthetic MPEG seek did not settle after {MAX_SEEK_RETRIES} retries",
        MAX_SEEK_RETRIES = consts::MAX_SEEK_RETRIES
    )
}

fn packet_at(reader: &mut MpaReader<'_>, ts: i64) -> Packet {
    for _ in 0..consts::MAX_SEEK_RETRIES {
        let packet = next_packet(reader);
        if packet.pts.get() == ts {
            return packet;
        }
        assert!(
            packet.pts.get() < ts,
            "scan overshot pts {ts}: got {}",
            packet.pts.get()
        );
    }
    panic!("no packet reached pts {ts}")
}

#[kithara::test]
fn mpa_packet_read_rolls_back_each_interruption(mpeg_four: &'static [u8]) {
    let bytes = mpeg_four.to_vec();
    let (fault_source, fault_control) = InterruptingSource::new(bytes.clone());
    let (control_source, _) = InterruptingSource::new(bytes);
    let mut fault = mpa_reader(fault_source);
    let mut control = mpa_reader(control_source);

    assert_packet_eq(&next_packet(&mut fault), &next_packet(&mut control));
    fault_control.arm_after(128, 2);

    assert_interrupted(&mut fault);
    assert_interrupted(&mut fault);
    assert_packet_eq(&next_packet(&mut fault), &next_packet(&mut control));
    assert_packet_eq(&next_packet(&mut fault), &next_packet(&mut control));
}

#[kithara::test]
fn mpa_seek_retried_after_interruption_matches_uninterrupted_seek(mpeg_eight: &'static [u8]) {
    let bytes = mpeg_eight.to_vec();
    let (fault_source, fault_control) = InterruptingSource::new(bytes.clone());
    let (control_source, _) = InterruptingSource::new(bytes);
    let mut fault = mpa_reader(fault_source);
    let mut control = mpa_reader(control_source);

    // Interrupt mid-body of frame 4 so the seek scan is cut after it has
    // part-consumed that frame. A retried seek must resume from the same
    // frame boundary; resyncing to the next header would count the frame
    // it skipped and shift every later timestamp by one frame.
    fault_control.arm_at(4 * consts::MPEG_FRAME_LEN + consts::MPEG_FRAME_LEN / 2, 1);

    let target_ts = 6 * consts::MPEG_FRAME_DUR;
    let fault_seeked = seek_after_interruption(&mut fault, target_ts);
    let control_seeked = seek_once(&mut control, target_ts);

    assert_eq!(fault_seeked.actual_ts, control_seeked.actual_ts);
    assert_packet_eq(&next_packet(&mut fault), &next_packet(&mut control));
}

#[kithara::test]
fn mpa_seek_retry_interrupted_mid_header_matches_uninterrupted_seek(mpeg_eight: &'static [u8]) {
    let bytes = mpeg_eight.to_vec();
    let (fault_source, fault_control) = InterruptingSource::new(bytes.clone());
    let (control_source, _) = InterruptingSource::new(bytes);
    let mut fault = mpa_reader(fault_source);
    let mut control = mpa_reader(control_source);

    // Interrupt right after the first header byte of frame 4, inside
    // `sync_frame`. Without a rollback the stranded 0xFF makes the retried
    // scan resync at frame 5 while carrying frame 4's timestamp.
    fault_control.arm_at(4 * consts::MPEG_FRAME_LEN + 1, 1);

    let target_ts = 6 * consts::MPEG_FRAME_DUR;
    let fault_seeked = seek_after_interruption(&mut fault, target_ts);
    let control_seeked = seek_once(&mut control, target_ts);

    assert_eq!(fault_seeked.actual_ts, control_seeked.actual_ts);
    assert_packet_eq(&next_packet(&mut fault), &next_packet(&mut control));
}

#[kithara::test]
fn mpa_seek_retry_interrupted_in_side_info_keeps_pts_aligned_with_data(mpeg_eight: &'static [u8]) {
    let bytes = mpeg_eight.to_vec();
    let (fault_source, fault_control) = InterruptingSource::new(bytes);
    let mut fault = mpa_reader(fault_source);

    // Interrupt inside the stop frame's side info (`read_main_data_begin`).
    // A retried scan legitimately picks a shallower bit-reservoir reference
    // (its frame ring restarts at the checkpoint), so fault/control equality
    // does not hold here. The pinned property is narrower: packet data must
    // stay aligned with pts — without a rollback the retry would deliver
    // frame 7's bytes under frame 6's timestamp.
    fault_control.arm_at(6 * consts::MPEG_FRAME_LEN + 5, 1);

    let target_ts = 6 * consts::MPEG_FRAME_DUR;
    seek_after_interruption(&mut fault, target_ts);

    let packet = packet_at(&mut fault, target_ts);
    assert!(
        packet.data[4..].iter().all(|&byte| byte == 0x77),
        "packet at pts {target_ts} must carry frame 6's bytes, got fill {:#04x}",
        packet.data[4]
    );
}

#[kithara::test]
fn mpa_packets_preserve_bytes_timestamps_and_interrupted_reads(mpeg_four: &'static [u8]) {
    let (source, fault_control) = InterruptingSource::new(mpeg_four.to_vec());
    let (control_source, _) = InterruptingSource::new(mpeg_four.to_vec());
    let mut control = mpa_reader(control_source);
    let mut packets = super::packets::Packets::new(Box::new(mpa_reader(source)));
    assert!(packets.restores_interrupted_packet());
    for index in 0..4 {
        if index == 1 {
            fault_control.arm_after(128, 2);
            for _ in 0..2 {
                match packets.read() {
                    Err(SymphoniaError::IoError(error)) => {
                        assert_eq!(error.kind(), ErrorKind::Interrupted);
                    }
                    _ => panic!("packet read must return the original interruption"),
                }
            }
        }
        let expected = next_packet(&mut control);
        let actual = packets
            .read()
            .expect("packet read")
            .expect("packet present");
        assert_eq!(actual.track_id, expected.track_id);
        assert_eq!(actual.pts, expected.pts);
        assert_eq!(actual.dur, expected.dur);
        assert_eq!(packets.data(), expected.data.as_ref());
    }
    assert!(packets.read().expect("end of stream").is_none());
}

#[kithara::test]
fn mpa_seek_preserves_packet_position(mpeg_eight: &'static [u8]) {
    let (source, _) = InterruptingSource::new(mpeg_eight.to_vec());
    let (control_source, _) = InterruptingSource::new(mpeg_eight.to_vec());
    let mut control = mpa_reader(control_source);
    let mut packets = super::packets::Packets::new(Box::new(mpa_reader(source)));
    let target = 6 * consts::MPEG_FRAME_DUR;
    let expected_seek = seek_once(&mut control, target);
    let actual_seek = packets
        .seek(SeekMode::Accurate, seek_to(target))
        .expect("pooled seek");
    assert_eq!(actual_seek.actual_ts, expected_seek.actual_ts);
    let expected = next_packet(&mut control);
    let actual = packets
        .read()
        .expect("packet read")
        .expect("packet present");
    assert_eq!(actual.pts, expected.pts);
    assert_eq!(actual.dur, expected.dur);
    assert_eq!(packets.data(), expected.data.as_ref());
}
