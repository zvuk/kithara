use std::{
    io::{self, ErrorKind, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use kithara_mpa::MpaReader;
use kithara_stream::{NotReadyCause, PendingReason};
use kithara_test_fixtures::mock_fixtures::{mpeg_eight, mpeg_four};
use kithara_test_utils::kithara;
use symphonia::core::{
    codecs::CodecParameters,
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo, TrackType},
    io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
    packet::Packet,
    units::Timestamp,
};

use crate::consts;

#[cfg(not(target_arch = "wasm32"))]
mod flac {
    use symphonia::{
        core::{
            codecs::{
                CodecParameters,
                audio::{AudioCodecParameters, AudioDecoderOptions},
            },
            formats::{FormatOptions, FormatReader},
            io::{MediaSource, MediaSourceStream},
            packet::Packet,
        },
        default::formats::FlacReader,
    };

    use super::{ErrorKind, InterruptingSource, SourceControl, kithara, lock};
    use crate::{
        demuxer::{DemuxOutcome, Demuxer},
        symphonia::SymphoniaDemuxer,
    };

    fn reader(bytes: &[u8], boundary: Option<usize>) -> (FlacReader<'static>, SourceControl) {
        let (source, control) = InterruptingSource::new(bytes.to_vec());
        control.make_seekable();
        assert!(source.is_seekable());
        assert_eq!(source.byte_len(), None);
        if let Some(boundary) = boundary {
            control.arm_at(boundary, usize::MAX);
            lock(&control.state).interrupt_kind = ErrorKind::WouldBlock;
        }
        let adapter = crate::symphonia::adapter::ReadSeekAdapter::new(source, None, true)
            .with_control(control.input.clone());
        let stream = MediaSourceStream::from(adapter);
        let reader =
            FlacReader::try_new(stream, FormatOptions::default()).expect("FLAC fixture reader");
        (reader, control)
    }

    fn params(reader: &FlacReader<'_>) -> AudioCodecParameters {
        let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.as_ref() else {
            panic!("FLAC fixture has audio parameters");
        };
        params.clone()
    }

    #[kithara::hang_watchdog]
    fn packets(
        mut reader: FlacReader<'static>,
        control: &SourceControl,
        boundary: Option<usize>,
    ) -> Vec<Packet> {
        let worker = kithara_platform::thread::spawn_named("flac-native-witness", move || {
            let mut packets = Vec::new();
            while let Some(packet) = reader
                .next_packet()
                .expect("native operation keeps its bytes")
            {
                packets.push(packet);
            }
            packets
        });
        if let Some(boundary) = boundary {
            while control.input.pending_reason().is_none() {
                hang_tick!();
                kithara_platform::thread::yield_runnable();
            }
            assert_eq!(lock(&control.state).pos, boundary);
            assert_eq!(
                control.input.pending_reason(),
                Some(kithara_stream::PendingReason::NotReady(
                    kithara_stream::NotReadyCause::WaitBudgetExhausted
                ))
            );
            control.release();
        }
        worker.join().expect("native FLAC witness owner")
    }

    fn pcm(params: &AudioCodecParameters, packets: &[Packet]) -> Vec<u32> {
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .expect("FLAC fixture codec");
        let mut result = Vec::new();
        for packet in packets {
            let decoded = decoder.decode(packet).expect("decode FLAC packet");
            let mut samples = vec![0.0f32; decoded.samples_interleaved()];
            decoded.copy_to_slice_interleaved(&mut samples);
            result.extend(samples.into_iter().map(f32::to_bits));
        }
        result
    }

    #[kithara::test]
    fn native_continuation_after_pending_keeps_every_packet_and_pcm_sample() {
        let bytes =
            kithara_test_fixtures::assets::signal_profile_flac_flac_44100_2ch_24bit().bytes();
        let (reference, control) = reader(bytes, None);
        let params = params(&reference);
        let reference = packets(reference, &control, None);
        let reference_pcm = pcm(&params, &reference);
        assert_eq!(reference.len(), 58, "fixed FLAC packet witness");
        assert_eq!(reference_pcm.len(), 264_600 * 2);

        for boundary in [31_743, 31_744, 31_745] {
            let (fault, control) = reader(bytes, Some(boundary));
            let actual = packets(fault, &control, Some(boundary));
            assert_eq!(
                actual.len(),
                reference.len(),
                "boundary {boundary}; source cursor {}; actual packet clocks={:?}; expected packet clocks={:?}",
                lock(&control.state).pos,
                actual
                    .iter()
                    .map(|packet| (packet.pts.get(), packet.dur.get(), packet.data.len()))
                    .collect::<Vec<_>>(),
                reference
                    .iter()
                    .map(|packet| (packet.pts.get(), packet.dur.get(), packet.data.len()))
                    .collect::<Vec<_>>(),
            );
            for (index, (actual, expected)) in actual.iter().zip(&reference).enumerate() {
                assert_eq!(
                    actual.track_id, expected.track_id,
                    "packet {index}, boundary {boundary}"
                );
                assert_eq!(
                    actual.pts, expected.pts,
                    "packet {index}, boundary {boundary}"
                );
                assert_eq!(
                    actual.dur, expected.dur,
                    "packet {index}, boundary {boundary}"
                );
                assert!(
                    actual.data == expected.data,
                    "packet {index} bytes changed at boundary {boundary}"
                );
            }
            let actual_pcm = pcm(&params, &actual);
            assert_eq!(actual_pcm.len(), reference_pcm.len());
            assert!(
                actual_pcm == reference_pcm,
                "PCM differs at sample {:?}, boundary {boundary}",
                actual_pcm
                    .iter()
                    .zip(&reference_pcm)
                    .position(|(actual, expected)| actual != expected)
            );
            tracing::info!(
                boundary,
                packets = actual.len(),
                frames = actual_pcm.len() / 2,
                "FLAC native continuation exact"
            );
        }
    }

    #[kithara::test]
    fn demuxer_continuation_after_pending_keeps_the_whole_flac_track() {
        let bytes =
            kithara_test_fixtures::assets::signal_profile_flac_flac_44100_2ch_24bit().bytes();
        let (reference, _) = reader(bytes, None);
        let (fault, control) = reader(bytes, Some(31_744));
        let mut reference = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(reference),
            None,
            None,
            None,
            kithara_stream::ReaderControl::new(None),
        )
        .expect("reference FLAC demuxer");
        let mut fault = SymphoniaDemuxer::from_reader_with_layout(
            Box::new(fault),
            None,
            None,
            None,
            control.input.clone(),
        )
        .expect("fault FLAC demuxer");
        let mut count = 0;
        let mut interrupted = false;
        loop {
            fault
                .prepare_frame_with_wait(crate::PrepareWait::Poll)
                .expect("poll FLAC operation");
            let actual = match fault.next_frame_prepared() {
                Ok(DemuxOutcome::Frame(frame)) => frame,
                Ok(DemuxOutcome::Pending(
                    kithara_stream::PendingReason::Retry
                    | kithara_stream::PendingReason::NotReady(
                        kithara_stream::NotReadyCause::OperationPending,
                    ),
                )) => {
                    kithara_platform::thread::yield_runnable();
                    continue;
                }
                Ok(DemuxOutcome::Pending(_)) => {
                    assert!(!interrupted, "one not-ready gate");
                    assert_eq!(lock(&control.state).pos, 31_744);
                    interrupted = true;
                    control.release();
                    continue;
                }
                Ok(DemuxOutcome::Eof) => break,
                Err(error) => {
                    let position = lock(&control.state).pos;
                    let seeks = control.seek_attempts();
                    panic!("FLAC demuxer failed at byte {position}, seeks={seeks:?}: {error}");
                }
            };
            let DemuxOutcome::Frame(expected) =
                reference.next_frame().expect("reference FLAC frame")
            else {
                panic!("reference must have the same FLAC frame");
            };
            assert_eq!(actual.pts, expected.pts, "frame {count}");
            assert_eq!(actual.duration, expected.duration, "frame {count}");
            assert!(actual.data == expected.data, "frame {count} bytes changed");
            count += 1;
        }
        assert!(interrupted, "regression must exercise not-ready recovery");
        assert!(matches!(
            reference.next_frame().expect("reference EOF"),
            DemuxOutcome::Eof
        ));
        assert_eq!(count, 58, "every FLAC packet survives Pending");
        tracing::info!(packets = count, "FLAC demuxer continuation exact");
    }
}

struct SourceState {
    interrupt_at: Option<usize>,
    interrupt_kind: ErrorKind,
    bytes: Vec<u8>,
    interrupts_remaining: usize,
    pos: usize,
    seekable: bool,
    seeks: Vec<(SeekFrom, Option<ErrorKind>)>,
}

struct SourceControl {
    state: Arc<Mutex<SourceState>>,
    input: kithara_stream::ReaderControl,
}

impl SourceControl {
    fn make_seekable(&self) {
        lock(&self.state).seekable = true;
    }

    fn block_after(&self, offset: usize) {
        self.arm_after(offset, usize::MAX);
        lock(&self.state).interrupt_kind = ErrorKind::WouldBlock;
    }

    fn release(&self) {
        lock(&self.state).interrupts_remaining = 0;
        self.input.notify();
    }

    fn seek_attempts(&self) -> Vec<(SeekFrom, Option<ErrorKind>)> {
        lock(&self.state).seeks.clone()
    }

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
            interrupt_kind: ErrorKind::Interrupted,
            interrupts_remaining: 0,
            pos: 0,
            seekable: false,
            seeks: Vec::new(),
        }));
        let control = SourceControl {
            state: Arc::clone(&state),
            input: kithara_stream::ReaderControl::new(None),
        };
        (Self { state }, control)
    }
}

impl Read for InterruptingSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = lock(&self.state);

        if state.interrupts_remaining > 0 && state.interrupt_at == Some(state.pos) {
            state.interrupts_remaining -= 1;
            if state.interrupt_kind == ErrorKind::WouldBlock {
                return Err(io::Error::new(
                    state.interrupt_kind,
                    PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
                ));
            }
            return Err(io::Error::new(
                state.interrupt_kind,
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
        drop(state);
        Ok(read)
    }
}

impl Seek for InterruptingSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let mut state = lock(&self.state);
        let result = if !state.seekable {
            Err(io::Error::new(
                ErrorKind::Unsupported,
                "synthetic source is forward-only",
            ))
        } else if matches!(pos, SeekFrom::End(_)) {
            Err(io::Error::new(
                ErrorKind::Unsupported,
                "seek from end requires known length",
            ))
        } else {
            let target = match pos {
                SeekFrom::Start(target) => usize::try_from(target).ok(),
                SeekFrom::Current(offset) => state.pos.checked_add_signed(
                    isize::try_from(offset)
                        .map_err(|_| io::Error::from(ErrorKind::InvalidInput))?,
                ),
                SeekFrom::End(_) => unreachable!("unknown-length end seek handled above"),
            };
            target
                .filter(|target| *target <= state.bytes.len())
                .map_or_else(
                    || Err(io::Error::from(ErrorKind::InvalidInput)),
                    |target| {
                        state.pos = target;
                        Ok(target as u64)
                    },
                )
        };
        state
            .seeks
            .push((pos, result.as_ref().err().map(io::Error::kind)));
        result
    }
}

impl MediaSource for InterruptingSource {
    fn byte_len(&self) -> Option<u64> {
        None
    }

    fn is_seekable(&self) -> bool {
        lock(&self.state).seekable
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

fn controlled_mpa_reader(
    source: InterruptingSource,
    control: &SourceControl,
) -> MpaReader<'static> {
    control.make_seekable();
    let adapter = crate::symphonia::adapter::ReadSeekAdapter::new(source, None, true)
        .with_control(control.input.clone());
    let stream = MediaSourceStream::from(adapter);
    MpaReader::try_new(stream, FormatOptions::default()).expect("controlled MPEG reader")
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
    fault_control.arm_after(128, usize::MAX);

    assert_interrupted(&mut fault);
    assert_interrupted(&mut fault);
    fault_control.release();
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
    let delay = fault.tracks()[0].delay.expect("Layer III decoder priming");
    assert_eq!(delay, 529);

    // Interrupt inside the stop frame's side info (`read_main_data_begin`).
    // A retried scan legitimately picks a shallower bit-reservoir reference
    // (its frame ring restarts at the checkpoint), so fault/control equality
    // does not hold here. The pinned property is narrower: packet data must
    // stay aligned with pts — without a rollback the retry would deliver
    // frame 7's bytes under frame 6's timestamp.
    fault_control.arm_at(6 * consts::MPEG_FRAME_LEN + 5, 1);

    let target_ts = 6 * consts::MPEG_FRAME_DUR;
    let seeked = seek_after_interruption(&mut fault, target_ts);
    assert_eq!(seeked.required_ts, Timestamp::new(target_ts));

    let expected_pts = target_ts - i64::from(delay);
    let packet = packet_at(&mut fault, expected_pts);
    let end = packet.pts.checked_add(packet.dur).expect("MPEG packet end");
    assert!(
        packet.pts.get() < target_ts && target_ts < end.get(),
        "the seek target remains inside frame 6"
    );
    assert!(
        packet.data[4..].iter().all(|&byte| byte == 0x77),
        "packet at pts {expected_pts} must carry frame 6's bytes, got fill {:#04x}",
        packet.data[4]
    );
}

#[kithara::test]
#[kithara::hang_watchdog]
fn mpa_packets_preserve_bytes_timestamps_and_interrupted_reads(mpeg_four: &'static [u8]) {
    let (source, fault_control) = InterruptingSource::new(mpeg_four.to_vec());
    let (control_source, _) = InterruptingSource::new(mpeg_four.to_vec());
    let mut control = mpa_reader(control_source);
    let reader = controlled_mpa_reader(source, &fault_control);
    let track = reader
        .default_track(TrackType::Audio)
        .expect("MPEG audio track")
        .clone();
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        panic!("MPEG audio codec parameters");
    };
    let mut packets = super::packets::Packets::new(
        Box::new(reader),
        track.id,
        params,
        None,
        fault_control.input.clone(),
    )
    .expect("MPEG packets");
    let mut interrupted = false;
    for index in 0..4 {
        if index == 1 {
            fault_control.block_after(128);
        }
        let expected = next_packet(&mut control);
        let actual = loop {
            hang_tick!();
            match packets.read(crate::PrepareWait::Poll) {
                Ok(Some(packet)) => break packet,
                Ok(None) => panic!("synthetic MPEG stream ended early"),
                Err(SymphoniaError::IoError(error)) => {
                    assert_eq!(error.kind(), ErrorKind::Interrupted);
                    let reason = error
                        .get_ref()
                        .and_then(|error| error.downcast_ref::<PendingReason>());
                    match reason {
                        Some(
                            PendingReason::Retry
                            | PendingReason::NotReady(NotReadyCause::OperationPending),
                        ) => {}
                        Some(PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted)) => {
                            assert!(!interrupted, "released input must remain ready");
                            for _ in 0..2 {
                                let Err(SymphoniaError::IoError(error)) =
                                    packets.read(crate::PrepareWait::Poll)
                                else {
                                    panic!("withheld input must keep its packet pending");
                                };
                                assert_eq!(error.kind(), ErrorKind::Interrupted);
                                assert_eq!(
                                    error
                                        .get_ref()
                                        .and_then(|error| error.downcast_ref::<PendingReason>()),
                                    Some(&PendingReason::NotReady(
                                        NotReadyCause::WaitBudgetExhausted
                                    ))
                                );
                            }
                            interrupted = true;
                            fault_control.release();
                        }
                        _ => panic!("unexpected packet input condition: {error}"),
                    }
                    kithara_platform::thread::yield_runnable();
                }
                Err(error) => panic!("synthetic MPEG packet failed: {error}"),
            }
        };
        assert_eq!(actual.track_id, expected.track_id);
        assert_eq!(actual.pts, expected.pts);
        assert_eq!(actual.dur, expected.dur);
        assert_eq!(packets.data(), expected.data.as_ref());
    }
    assert!(
        interrupted,
        "packet reading must encounter the withheld input"
    );
    assert!(
        packets
            .read(crate::PrepareWait::Wait)
            .expect("end of stream")
            .is_none()
    );
}

#[kithara::test]
fn mpa_seek_preserves_packet_position(mpeg_eight: &'static [u8]) {
    let (source, _) = InterruptingSource::new(mpeg_eight.to_vec());
    let (control_source, _) = InterruptingSource::new(mpeg_eight.to_vec());
    let mut control = mpa_reader(control_source);
    let reader = mpa_reader(source);
    let track = reader
        .default_track(TrackType::Audio)
        .expect("MPEG audio track")
        .clone();
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        panic!("MPEG audio codec parameters");
    };
    let mut packets = super::packets::Packets::new(
        Box::new(reader),
        track.id,
        params,
        None,
        kithara_stream::ReaderControl::new(None),
    )
    .expect("MPEG packets");
    let target = 6 * consts::MPEG_FRAME_DUR;
    let expected_seek = seek_once(&mut control, target);
    let actual_seek = packets
        .seek(
            SeekMode::Accurate,
            seek_to(target),
            crate::PrepareWait::Wait,
        )
        .expect("pooled seek")
        .expect("settled seek");
    assert_eq!(actual_seek.actual_ts, expected_seek.actual_ts);
    let expected = next_packet(&mut control);
    let actual = packets
        .read(crate::PrepareWait::Wait)
        .expect("packet read")
        .expect("packet present");
    assert_eq!(actual.pts, expected.pts);
    assert_eq!(actual.dur, expected.dur);
    assert_eq!(packets.data(), expected.data.as_ref());
}

#[cfg(not(target_arch = "wasm32"))]
mod wav {
    use std::io::Cursor;

    use kithara_platform::{sync::mpsc, thread, time::Duration};
    use kithara_stream::{ContainerFormat, NotReadyCause, PendingReason};
    use kithara_test_fixtures::{
        fixtures::tone_wav,
        hls_fixtures::{hls_header_fifty, hls_pcm_fifty},
    };
    use kithara_test_utils::wait_until_blocking;
    use symphonia::{
        core::{
            codecs::{CodecParameters, audio::well_known::CODEC_ID_PCM_S16LE},
            formats::{FormatOptions, FormatReader},
            io::{MediaSourceStream, MediaSourceStreamOptions},
        },
        default::formats::WavReader,
    };

    use super::{InterruptingSource, SourceControl, SymphoniaError, kithara};
    use crate::{
        CodecPriming, DecodeError, Decoder, DecoderBackend, DecoderChunkOutcome, DecoderConfig,
        DecoderFactory, DecoderSeekOutcome,
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
        symphonia::{FileOpen, SymphoniaDemuxer},
        test_pools::{TestPools, pools},
    };

    type WavPacket = (Duration, Duration, Vec<u8>);

    fn native_reader(bytes: &[u8]) -> (WavReader<'static>, SourceControl) {
        let (source, control) = InterruptingSource::new(bytes.to_vec());
        control.make_seekable();
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let reader = WavReader::try_new(stream, FormatOptions::default()).expect("WAV reader");
        (reader, control)
    }

    fn reader(bytes: &[u8]) -> (SymphoniaDemuxer, SourceControl) {
        let (source, control) = InterruptingSource::new(bytes.to_vec());
        control.make_seekable();
        let (demuxer, _) = SymphoniaDemuxer::open_file(
            source,
            FileOpen {
                control: control.input.clone(),
                container: Some(ContainerFormat::Wav),
                hint: None,
                byte_len_handle: None,
                byte_map: None,
            },
        )
        .expect("WAV demuxer");
        (demuxer, control)
    }

    fn decoder(bytes: &[u8]) -> Box<dyn Decoder> {
        let config = DecoderConfig::<kithara_resampler::NoResamplerBackend, TestPools>::builder()
            .backend(DecoderBackend::Symphonia)
            .pools(pools())
            .build();
        DecoderFactory::create_with_probe(Cursor::new(bytes.to_vec()), Some("wav"), config)
            .expect("WAV decoder")
    }

    fn assert_decoded_pcm16(bytes: &[u8], expected: &[u8], start_frame: u64) {
        let mut decoder = decoder(bytes);
        let spec = decoder.spec();
        if start_frame != 0 {
            assert!(matches!(
                decoder
                    .seek(spec.duration_for(start_frame).expect("seek timestamp"))
                    .expect("PCM seek"),
                DecoderSeekOutcome::Landed { .. }
            ));
        }
        let mut next_frame = start_frame;
        let mut actual = Vec::new();
        loop {
            let chunk = match decoder.next_chunk().expect("physical WAV PCM") {
                DecoderChunkOutcome::Chunk(chunk) => chunk,
                DecoderChunkOutcome::Eof => break,
                DecoderChunkOutcome::Pending(reason) => {
                    panic!("complete WAV source is pending: {reason:?}")
                }
            };
            assert_eq!(chunk.meta.spec, spec);
            assert_eq!(chunk.meta.frame_offset, next_frame);
            assert_eq!(
                chunk.meta.timestamp,
                spec.duration_for(next_frame).expect("PCM timestamp"),
                "PCM must remain anchored to its physical input"
            );
            assert_eq!(
                chunk.frames(),
                usize::try_from(chunk.meta.frames).expect("chunk frames")
            );
            assert_eq!(
                chunk.meta.end_timestamp - chunk.meta.timestamp,
                Duration::from_secs_f64(
                    f64::from(chunk.meta.frames) / f64::from(spec.sample_rate.get())
                ),
                "chunk duration must describe its actual PCM"
            );
            next_frame += u64::from(chunk.meta.frames);
            actual.extend(chunk.samples.iter().map(|sample| sample.to_bits()));
        }
        assert_eq!(expected.len() % (2 * usize::from(spec.channels)), 0);
        assert_eq!(
            next_frame - start_frame,
            u64::try_from(expected.len() / (2 * usize::from(spec.channels))).expect("PCM frames")
        );
        let expected = expected
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| {
                (f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0).to_bits()
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "every physical PCM16 sample must survive");
    }

    fn frame(demuxer: &mut SymphoniaDemuxer) -> Option<WavPacket> {
        try_frame(demuxer).expect("WAV packet")
    }

    fn try_frame(demuxer: &mut SymphoniaDemuxer) -> Result<Option<WavPacket>, String> {
        match demuxer.next_frame().map_err(|error| error.to_string())? {
            DemuxOutcome::Frame(frame) => {
                Ok(Some((frame.pts, frame.duration, frame.data.to_vec())))
            }
            DemuxOutcome::Eof => Ok(None),
            DemuxOutcome::Pending(reason) => Err(format!("WAV packet is pending: {reason:?}")),
        }
    }

    #[kithara::hang_watchdog]
    fn pending_frame(demuxer: &mut SymphoniaDemuxer) -> PendingReason {
        loop {
            hang_tick!();
            demuxer
                .prepare_frame_with_wait(crate::PrepareWait::Poll)
                .expect("poll parser operation");
            match demuxer.next_frame_prepared().expect("poll retained packet") {
                DemuxOutcome::Pending(
                    PendingReason::Retry | PendingReason::NotReady(NotReadyCause::OperationPending),
                ) => {
                    thread::yield_runnable();
                }
                DemuxOutcome::Pending(reason) => return reason,
                DemuxOutcome::Frame(frame) => panic!(
                    "partial WAV packet was published: pts={:?} duration={:?} bytes={}",
                    frame.pts,
                    frame.duration,
                    frame.data.len()
                ),
                DemuxOutcome::Eof => panic!("pending WAV source was reported as EOF"),
            }
        }
    }

    #[kithara::test]
    fn native_prefix_owner_test() {
        const HEADER_BYTES: usize = 44;
        const STRIDE: usize = 4;
        const RATE: u64 = 44_100;
        const SEGMENT_BYTES: usize = 200_000;
        const FRAMES: usize = 2_500_000;
        const PACKET_FRAMES: usize = 1_152;
        const CUT: usize = 146_304;
        const PREFIX_PACKETS: usize = CUT / PACKET_FRAMES + 2;
        let mut wav = hls_header_fifty();
        assert_eq!(wav.len(), HEADER_BYTES);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(&wav[20..24], &[1, 0, 2, 0]);
        assert_eq!(&wav[24..28], &44_100_u32.to_le_bytes());
        assert_eq!(&wav[32..36], &[4, 0, 16, 0]);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(&wav[40..44], &10_000_000_u32.to_le_bytes());
        assert_eq!(&wav[4..8], &10_000_036_u32.to_le_bytes());
        wav.extend(hls_pcm_fifty());
        assert_eq!(wav.len(), HEADER_BYTES + 50 * SEGMENT_BYTES);
        assert_eq!(wav.len() - HEADER_BYTES, FRAMES * STRIDE);
        assert_eq!(CUT % PACKET_FRAMES, 0);
        assert_eq!(PREFIX_PACKETS, 129);
        let gate_byte = HEADER_BYTES + 3 * SEGMENT_BYTES;
        let prefix_end = HEADER_BYTES + (CUT + 2 * PACKET_FRAMES) * STRIDE;
        assert_eq!(gate_byte, 600_044);
        assert_eq!(HEADER_BYTES + CUT * STRIDE, 585_260);
        assert_eq!(HEADER_BYTES + (CUT + PACKET_FRAMES) * STRIDE, 589_868);
        assert_eq!(prefix_end, 594_476);
        assert!(prefix_end < gate_byte);

        let (mut reference, _) = reader(&wav);
        assert_eq!(reference.track_info().sample_rate, 44_100);
        assert_eq!(reference.track_info().channels, 2);
        let reference = std::iter::from_fn(|| frame(&mut reference)).collect::<Vec<_>>();
        assert_eq!(reference.len(), FRAMES.div_ceil(PACKET_FRAMES));
        let clock = |frames: usize| {
            Duration::from_nanos(
                u64::try_from(frames).expect("fixture frames") * 1_000_000_000 / RATE,
            )
        };
        let mut next_frame = 0;
        for (index, (pts, duration, data)) in reference.iter().enumerate() {
            let frames = (FRAMES - next_frame).min(PACKET_FRAMES);
            assert_eq!(*pts, clock(next_frame), "reference packet {index}");
            assert_eq!(*duration, clock(frames), "reference packet {index}");
            assert_eq!(data.len(), frames * STRIDE, "reference packet {index}");
            let start = HEADER_BYTES + next_frame * STRIDE;
            assert!(
                data == &wav[start..start + frames * STRIDE],
                "reference PCM bytes differ in packet {index}"
            );
            next_frame += frames;
        }
        assert_eq!(next_frame, FRAMES);
        for (index, start) in [(127, 146_304), (128, 147_456)] {
            let (pts, duration, data) = &reference[index];
            assert_eq!(*pts, clock(start));
            assert_eq!(*duration, clock(PACKET_FRAMES));
            assert_eq!(data.len(), 4_608);
        }

        let (source, control) = InterruptingSource::new(wav);
        control.make_seekable();
        assert!(symphonia::core::io::MediaSource::is_seekable(&source));
        assert_eq!(symphonia::core::io::MediaSource::byte_len(&source), None);
        assert_eq!(super::lock(&control.state).pos, 0);
        control.arm_at(gate_byte, usize::MAX);
        super::lock(&control.state).interrupt_kind = std::io::ErrorKind::WouldBlock;
        let input = control.input.clone();
        let (completed, completion) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let worker = thread::spawn_named("wav-native-prefix", move || {
            let (mut demuxer, _) = SymphoniaDemuxer::open_file(
                source,
                FileOpen {
                    control: input,
                    container: Some(ContainerFormat::Wav),
                    hint: None,
                    byte_len_handle: None,
                    byte_map: None,
                },
            )
            .map_err(|error| error.to_string())?;
            let mut prefix = Vec::new();
            for _ in 0..PREFIX_PACKETS {
                let Some(packet) = try_frame(&mut demuxer)? else {
                    break;
                };
                prefix.push(packet);
            }
            completed
                .send(prefix)
                .map_err(|_| "prefix receiver dropped".to_owned())?;
            // The parser reads per request; pause before a later packet can reach the gate.
            if !matches!(resumed.recv(), Ok(true)) {
                return Ok(Vec::new());
            }
            let mut remaining = Vec::new();
            while let Some(packet) = try_frame(&mut demuxer)? {
                remaining.push(packet);
            }
            Ok::<_, String>(remaining)
        });
        let mut prefix = None;
        let wait = wait_until_blocking(Duration::from_secs(1), "native WAV prefix output", || {
            match completion.try_recv() {
                Ok(packets) => {
                    prefix = Some(packets);
                    true
                }
                Err(mpsc::TryRecvError::Empty) => control.input.pending_reason().is_some(),
                Err(mpsc::TryRecvError::Disconnected) => true,
            }
        });
        let pending = control.input.pending_reason();
        let state = super::lock(&control.state);
        let source_pos = state.pos;
        let held = state.interrupt_at == Some(gate_byte) && state.interrupts_remaining > 0;
        drop(state);
        let mismatch = prefix.as_ref().and_then(|packets| {
            packets
                .iter()
                .zip(&reference)
                .position(|(actual, expected)| actual != expected)
        });
        let prefix_ok = wait.is_ok()
            && pending.is_none()
            && !control.input.is_cancelled()
            && held
            && (prefix_end..=gate_byte).contains(&source_pos)
            && prefix
                .as_ref()
                .is_some_and(|packets| packets.len() == PREFIX_PACKETS && mismatch.is_none());
        // Seal the verdict while held, then unblock both owners before any failing assertion.
        if !prefix_ok {
            control.input.cancel();
        }
        control.release();
        let acknowledged = resume.send(prefix_ok).is_ok();
        let remaining = worker.join();
        let worker_error = match &remaining {
            Ok(Err(error)) => Some(error.as_str()),
            Err(_) => Some("worker panicked"),
            Ok(Ok(_)) => None,
        };
        assert!(
            prefix_ok,
            "native WAV packets 146304..147456 and 147456..148608 must complete with exact PCM before release; source_byte={source_pos}, gate_byte={gate_byte}, held={held}, pending={pending:?}, wait={wait:?}, packets={:?}, mismatch={mismatch:?}, worker_error={worker_error:?}",
            prefix.as_ref().map(Vec::len)
        );
        assert!(
            acknowledged,
            "prefix worker must retain its acknowledgement"
        );
        let prefix = prefix.expect("validated prefix output");
        let remaining = remaining
            .expect("native prefix worker")
            .expect("WAV continuation");
        assert_eq!(prefix.len() + remaining.len(), reference.len());
        let mut pcm_bytes = 0;
        for (index, (actual, expected)) in
            prefix.iter().chain(&remaining).zip(&reference).enumerate()
        {
            assert_eq!(actual.0, expected.0, "WAV packet {index} timestamp");
            assert_eq!(actual.1, expected.1, "WAV packet {index} duration");
            assert_eq!(actual.2.len(), expected.2.len(), "WAV packet {index} size");
            assert!(
                actual.2 == expected.2,
                "WAV packet {index} PCM bytes changed"
            );
            pcm_bytes += actual.2.len();
        }
        assert_eq!(pcm_bytes, FRAMES * STRIDE);
    }

    #[kithara::test]
    fn wav_seek_pending_preserves_every_packet_byte_and_timestamp(tone_wav: &'static [u8]) {
        let (mut fault, gate) = reader(tone_wav);
        let (mut reference, _) = reader(tone_wav);
        let target = Duration::from_millis(500);
        for demuxer in [&mut fault, &mut reference] {
            assert!(matches!(
                demuxer
                    .seek(target, CodecPriming::default())
                    .expect("WAV seek"),
                DemuxSeekOutcome::Landed { .. }
            ));
        }
        gate.block_after(128);
        assert_eq!(
            pending_frame(&mut fault),
            PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted)
        );
        gate.release();
        let mut packets = 0;
        loop {
            let expected = frame(&mut reference);
            let actual = frame(&mut fault);
            assert_eq!(actual, expected, "WAV packet {packets} after Pending");
            if expected.is_none() {
                break;
            }
            packets += 1;
        }
        assert!(
            packets > 1,
            "seek must leave multiple WAV packets to verify"
        );
    }

    #[kithara::test]
    #[kithara::hang_watchdog]
    fn dropping_a_native_parser_releases_its_paused_input(tone_wav: &'static [u8]) {
        let (mut demuxer, control) = reader(tone_wav);
        control.block_after(128);
        assert_eq!(
            pending_frame(&mut demuxer),
            PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted)
        );
        assert_eq!(std::sync::Arc::strong_count(&control.state), 2);
        drop(demuxer);
        while std::sync::Arc::strong_count(&control.state) != 1 {
            hang_tick!();
            thread::yield_runnable();
        }
        assert!(control.input.is_cancelled());
    }

    #[kithara::test]
    fn wav_truncated_eof_preserves_every_complete_frame(tone_wav: &'static [u8]) {
        let (mut complete, _) = native_reader(tone_wav);
        let declared_frames = complete.tracks()[0].num_frames.expect("WAV frame count");
        let Some(CodecParameters::Audio(params)) = complete.tracks()[0].codec_params.as_ref()
        else {
            panic!("WAV audio parameters");
        };
        assert_eq!(params.codec, CODEC_ID_PCM_S16LE);
        let first = complete
            .next_packet()
            .expect("WAV head")
            .expect("WAV head packet");
        let packet_frames = usize::try_from(first.dur.get()).expect("head frames");
        let block_size = first.data.len() / packet_frames;
        assert!(block_size > 0);
        let mut expected = first.data.into_vec();
        while let Some(packet) = complete.next_packet().expect("complete WAV packet") {
            expected.extend_from_slice(&packet.data);
        }
        assert_eq!(
            expected.len() / block_size,
            usize::try_from(declared_frames).expect("frames")
        );
        let header_bytes = tone_wav.len() - expected.len();
        assert_eq!(&tone_wav[header_bytes..], expected);
        let physical_frames = expected.len() / block_size - 3;
        let last_packet = ((physical_frames - 1) / packet_frames) * packet_frames;
        for (frames, start) in [
            (physical_frames, 0),
            (packet_frames - 3, 0),
            (physical_frames, last_packet + 1),
        ] {
            let bytes = &tone_wav[..header_bytes + frames * block_size];
            let expected = &expected[..frames * block_size];
            let (mut truncated, _) = reader(bytes);
            let spec = decoder(bytes).spec();
            let packet_start = (start / packet_frames) * packet_frames;
            let mut next_frame = u64::try_from(packet_start).expect("packet start");
            if packet_start != 0 {
                let target = spec
                    .duration_for(u64::try_from(start).expect("seek frame"))
                    .expect("packet seek");
                let expected_landing = spec.duration_for(next_frame).expect("packet landing");
                assert!(matches!(
                    truncated
                        .seek(target, CodecPriming::default())
                        .expect("WAV seek"),
                    DemuxSeekOutcome::Landed { landed_at, .. } if landed_at == expected_landing
                ));
            }
            let mut actual = Vec::new();
            while let Some((pts, duration, data)) = frame(&mut truncated) {
                assert_eq!(data.len() % block_size, 0);
                let frames = u64::try_from(data.len() / block_size).expect("packet frames");
                assert_eq!(
                    pts,
                    spec.duration_for(next_frame).expect("packet timestamp")
                );
                assert_eq!(
                    duration,
                    spec.duration_for(frames).expect("packet duration")
                );
                next_frame += frames;
                actual.extend_from_slice(&data);
            }
            assert_eq!(next_frame, u64::try_from(frames).expect("physical frames"));
            assert_eq!(actual, &expected[packet_start * block_size..]);
            assert_decoded_pcm16(
                bytes,
                &expected[start * block_size..],
                u64::try_from(start).expect("PCM start"),
            );
        }
        assert_eq!(
            u64::try_from(physical_frames).expect("physical frames"),
            declared_frames - 3
        );
    }

    #[kithara::test]
    fn wav_adpcm_physical_eof_uses_only_complete_encoded_blocks() {
        // One zero IMA block encodes nine mono frames; the header declares two blocks.
        const WAV: &[u8] = b"RIFF\x44\0\0\0WAVEfmt \x14\0\0\0\
            \x11\0\x01\0\x80\xbb\0\0\xaa\xa6\0\0\x08\0\x04\0\x02\0\x09\0\
            fact\x04\0\0\0\x12\0\0\0data\x10\0\0\0\0\0\0\0\0\0\0\0";
        const HEADER_BYTES: usize = 60;
        assert_eq!(WAV.len(), HEADER_BYTES + 8);
        for physical_bytes in [8, 0] {
            let bytes = &WAV[..HEADER_BYTES + physical_bytes];
            let (mut demuxer, _) = reader(bytes);
            if physical_bytes != 0 {
                let (pts, duration, data) = frame(&mut demuxer).expect("IMA packet");
                assert_eq!(pts, Duration::ZERO);
                assert_eq!(duration, Duration::from_nanos(187_500));
                assert_eq!(data, &WAV[HEADER_BYTES..]);
            }
            assert!(frame(&mut demuxer).is_none(), "physical EOF is terminal");
            let expected = vec![0; physical_bytes / 8 * 9 * 2];
            assert_decoded_pcm16(bytes, &expected, 0);
        }
        let bytes = &WAV[..WAV.len() - 1];
        let (mut demuxer, _) = reader(bytes);
        let mut decoder = decoder(bytes);
        let Err(packet_error) = demuxer.next_frame() else {
            panic!("incomplete IMA block must not become an audio packet");
        };
        for error in [
            packet_error,
            decoder.next_chunk().expect_err("incomplete IMA block"),
        ] {
            let DecodeError::Backend { source } = error else {
                panic!("incomplete IMA block must remain a typed decoder error");
            };
            assert!(matches!(
                source.downcast_ref::<SymphoniaError>(),
                Some(SymphoniaError::DecodeError(_))
            ));
        }
    }

    #[kithara::test]
    fn wav_large_native_packets_preserve_every_byte_through_short_reads_and_pending() {
        const CHANNELS: u16 = 18;
        const RATE: u32 = 48_000;
        const FRAMES: usize = 1_152 * 3 + 17;
        const STRIDE: u16 = CHANNELS * 8;
        const HEADER_BYTES: usize = 44;
        let wav = kithara_test_fixtures::assets::decoder_float64_wav_default()
            .bytes()
            .to_vec();
        assert_eq!(wav.len(), HEADER_BYTES + FRAMES * usize::from(STRIDE));
        assert!(1_152 * usize::from(STRIDE) > 2 * 64 * 1_024);
        for pending in [false, true] {
            let (mut actual, gate) = reader(&wav);
            let (mut reference, _) = SymphoniaDemuxer::open_file(
                Cursor::new(wav.clone()),
                FileOpen {
                    control: kithara_stream::ReaderControl::new(None),
                    container: Some(ContainerFormat::Wav),
                    hint: None,
                    byte_len_handle: None,
                    byte_map: None,
                },
            )
            .expect("contiguous large WAV reference");
            assert_eq!(actual.track_info().channels, CHANNELS);
            if pending {
                gate.block_after(128);
                assert_eq!(
                    pending_frame(&mut actual),
                    PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted)
                );
                gate.release();
            }
            let mut next_frame = 0;
            let mut bytes = Vec::new();
            loop {
                let expected = frame(&mut reference);
                let actual = frame(&mut actual);
                assert_eq!(actual, expected, "large WAV packet at frame {next_frame}");
                let Some((pts, duration, data)) = actual else {
                    break;
                };
                assert_eq!(data.len() % usize::from(STRIDE), 0);
                let frames = data.len() / usize::from(STRIDE);
                assert_eq!(
                    pts,
                    Duration::from_nanos(
                        u64::try_from(next_frame).expect("frame position") * 1_000_000_000
                            / u64::from(RATE)
                    )
                );
                assert_eq!(
                    duration,
                    Duration::from_nanos(
                        u64::try_from(frames).expect("packet frames") * 1_000_000_000
                            / u64::from(RATE)
                    )
                );
                next_frame += frames;
                bytes.extend_from_slice(&data);
            }
            assert_eq!(next_frame, FRAMES);
            assert_eq!(bytes, &wav[HEADER_BYTES..]);
        }
    }
}
