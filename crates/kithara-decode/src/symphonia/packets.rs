use std::{
    io,
    num::{NonZeroU16, NonZeroU64},
};

use kithara_platform::{sync::mpsc, thread};
use kithara_stream::{NotReadyCause, PendingReason, ReaderControl};
use symphonia_core::{
    codecs::audio::{
        AudioCodecParameters,
        well_known::{CODEC_ID_ADPCM_IMA_WAV, CODEC_ID_ADPCM_MS},
    },
    errors::{Error, Result},
    formats::{
        FormatId, FormatReader, SeekMode, SeekTo, SeekedTo,
        well_known::{FORMAT_ID_AIFF, FORMAT_ID_WAVE},
    },
    packet::Packet,
    units::{Duration, Timestamp},
};

use super::track::pcm_bytes_per_sample;
use crate::{DecodeError, DecodeResult, InputState, PrepareWait};

pub(super) struct PacketInfo {
    pub(super) dur: Duration,
    pub(super) pts: Timestamp,
    pub(super) track_id: u32,
}

enum Request {
    Read,
    Seek { mode: SeekMode, to: SeekTo },
    Retire,
}

enum Reply {
    Read(Result<Option<Packet>>),
    Seek(Result<SeekedTo>),
    Retired,
}

#[derive(Clone, Copy)]
enum Operation {
    Idle,
    Reading,
    Seeking,
    Retiring,
    Retired,
}

pub(super) struct Packets {
    requests: Option<mpsc::Sender<Request>>,
    completion: mpsc::Receiver<Reply>,
    operation: Operation,
    packet: Option<Packet>,
    control: ReaderControl,
}

struct NativePacketGeometry {
    track_id: u32,
    block_bytes: NonZeroU64,
    frames_per_block: NonZeroU64,
}

impl NativePacketGeometry {
    fn new(
        format: FormatId,
        track_id: u32,
        params: &AudioCodecParameters,
        wav_block_align: Option<NonZeroU16>,
    ) -> DecodeResult<Option<Self>> {
        if !matches!(format, FORMAT_ID_WAVE | FORMAT_ID_AIFF) {
            return Ok(None);
        }
        let (block_bytes, frames_per_block) = if let Some(width) =
            pcm_bytes_per_sample(params.codec)
        {
            let block_bytes = params
                .channels
                .as_ref()
                .and_then(|channels| u64::try_from(channels.count()).ok())
                .and_then(|channels| channels.checked_mul(u64::from(width)))
                .and_then(NonZeroU64::new)
                .ok_or(DecodeError::InvalidData {
                    detail: "native PCM track has no valid frame stride",
                })?;
            if format == FORMAT_ID_WAVE
                && wav_block_align.is_some_and(|align| u64::from(align.get()) != block_bytes.get())
            {
                return Err(DecodeError::InvalidData {
                    detail: "WAV block alignment disagrees with native PCM storage width",
                });
            }
            (block_bytes, NonZeroU64::MIN)
        } else if format == FORMAT_ID_WAVE
            && matches!(params.codec, CODEC_ID_ADPCM_IMA_WAV | CODEC_ID_ADPCM_MS)
        {
            let block_bytes =
                wav_block_align
                    .map(NonZeroU64::from)
                    .ok_or(DecodeError::InvalidData {
                        detail: "native WAV ADPCM track has no header block alignment",
                    })?;
            let frames_per_block = params.frames_per_block.and_then(NonZeroU64::new).ok_or(
                DecodeError::InvalidData {
                    detail: "native WAV ADPCM track has no frames per block",
                },
            )?;
            (block_bytes, frames_per_block)
        } else {
            return Ok(None);
        };
        Ok(Some(Self {
            track_id,
            block_bytes,
            frames_per_block,
        }))
    }

    fn normalize(&self, packet: &mut Packet) -> Result<bool> {
        if packet.track_id != self.track_id {
            return Ok(true);
        }
        if packet.data.is_empty() {
            return Ok(false);
        }
        let bytes = u64::try_from(packet.data.len())
            .map_err(|_| Error::DecodeError("native audio packet byte count overflows"))?;
        if bytes % self.block_bytes != 0 {
            tracing::error!(
                bytes,
                block_bytes = self.block_bytes.get(),
                frames_per_block = self.frames_per_block.get(),
                track_id = packet.track_id,
                pts = packet.pts.get(),
                declared_frames = packet.dur.get(),
                "native audio packet contains an incomplete encoded block"
            );
            return Err(Error::DecodeError(
                "native audio packet contains an incomplete encoded block",
            ));
        }
        let frames = (bytes / self.block_bytes)
            .checked_mul(self.frames_per_block.get())
            .ok_or(Error::DecodeError("native audio packet duration overflows"))?;
        packet.dur = Duration::new(frames);
        Ok(true)
    }
}

impl Packets {
    pub(super) fn new(
        reader: Box<dyn FormatReader>,
        track_id: u32,
        params: &AudioCodecParameters,
        wav_block_align: Option<NonZeroU16>,
        control: ReaderControl,
    ) -> DecodeResult<Self> {
        let native_geometry = NativePacketGeometry::new(
            reader.format_info().format,
            track_id,
            params,
            wav_block_align,
        )?;
        let (requests, requested) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let worker_control = control.clone();
        drop(thread::spawn_named("kithara-native-parser", move || {
            run(
                reader,
                native_geometry.as_ref(),
                &requested,
                &completed,
                &worker_control,
            );
        }));
        Ok(Self {
            requests: Some(requests),
            completion,
            operation: Operation::Idle,
            packet: None,
            control,
        })
    }

    pub(super) fn data(&self) -> &[u8] {
        &self
            .packet
            .as_ref()
            .expect("packet was read successfully")
            .data
    }

    fn interrupted(reason: PendingReason) -> Error {
        Error::IoError(io::Error::new(io::ErrorKind::Interrupted, reason))
    }

    pub(super) fn pending_reason(&self) -> PendingReason {
        self.control
            .pending_reason()
            .unwrap_or(PendingReason::NotReady(NotReadyCause::OperationPending))
    }

    fn submit(&mut self, request: Request, operation: Operation) -> Result<()> {
        self.control.clear_pending();
        self.requests
            .as_ref()
            .ok_or_else(|| Self::interrupted(PendingReason::SessionRetired))?
            .send(request)
            .map_err(|_| Self::interrupted(PendingReason::SessionRetired))?;
        self.operation = operation;
        Ok(())
    }

    fn receive(&mut self, wait: PrepareWait) -> Result<Option<Reply>> {
        let result = match wait {
            PrepareWait::Wait => self.completion.recv().map(Some).map_err(|_| ()),
            PrepareWait::Poll => match self.completion.try_recv() {
                Ok(reply) => Ok(Some(reply)),
                Err(mpsc::TryRecvError::Empty) => Ok(None),
                Err(_) => Err(()),
            },
        };
        if let Ok(reply) = result {
            Ok(reply)
        } else {
            self.operation = Operation::Retired;
            Err(Self::interrupted(PendingReason::SessionRetired))
        }
    }

    pub(super) fn read(&mut self, wait: PrepareWait) -> Result<Option<PacketInfo>> {
        match self.operation {
            Operation::Idle => {
                self.packet = None;
                self.submit(Request::Read, Operation::Reading)?;
            }
            Operation::Reading => {}
            Operation::Seeking => return Err(Self::interrupted(self.pending_reason())),
            Operation::Retiring | Operation::Retired => {
                return Err(Self::interrupted(PendingReason::SessionRetired));
            }
        }
        let Some(reply) = self.receive(wait)? else {
            return Err(Self::interrupted(self.pending_reason()));
        };
        match reply {
            Reply::Read(result) => {
                self.operation = if result.is_ok() {
                    Operation::Idle
                } else {
                    Operation::Retired
                };
                self.packet = result?;
                Ok(self.packet.as_ref().map(|packet| PacketInfo {
                    track_id: packet.track_id,
                    pts: packet.pts,
                    dur: packet.dur,
                }))
            }
            Reply::Retired => {
                self.operation = Operation::Retired;
                Err(Self::interrupted(PendingReason::SessionRetired))
            }
            Reply::Seek(_) => Err(Error::DecodeError(
                "parser replied to a different operation",
            )),
        }
    }

    pub(super) fn seek(
        &mut self,
        mode: SeekMode,
        to: SeekTo,
        wait: PrepareWait,
    ) -> Result<Option<SeekedTo>> {
        if !matches!(self.operation, Operation::Idle) {
            return Err(Self::interrupted(PendingReason::SessionRetired));
        }
        self.packet = None;
        self.submit(Request::Seek { mode, to }, Operation::Seeking)?;
        self.prepare_seek(wait)
    }

    pub(super) fn prepare_seek(&mut self, wait: PrepareWait) -> Result<Option<SeekedTo>> {
        if !matches!(self.operation, Operation::Seeking) {
            return Ok(None);
        }
        let Some(reply) = self.receive(wait)? else {
            return Ok(None);
        };
        match reply {
            Reply::Seek(result) => {
                self.operation = if result.is_ok() {
                    Operation::Idle
                } else {
                    Operation::Retired
                };
                result.map(Some)
            }
            Reply::Retired => {
                self.operation = Operation::Retired;
                Err(Self::interrupted(PendingReason::SessionRetired))
            }
            Reply::Read(_) => Err(Error::DecodeError(
                "parser replied to a different operation",
            )),
        }
    }

    pub(super) fn release_input(&mut self) -> InputState {
        if matches!(self.operation, Operation::Reading | Operation::Seeking) {
            match self.receive(PrepareWait::Poll) {
                Ok(Some(Reply::Read(Ok(_)) | Reply::Seek(Ok(_)))) => {
                    self.operation = Operation::Idle;
                }
                Ok(Some(Reply::Read(Err(_)) | Reply::Seek(Err(_)) | Reply::Retired)) | Err(_) => {
                    self.operation = Operation::Retired;
                }
                Ok(None) => {
                    self.control.cancel();
                    if let Some(requests) = &self.requests {
                        drop(requests.send(Request::Retire));
                    }
                    self.operation = Operation::Retiring;
                }
            }
        }
        if matches!(self.operation, Operation::Retiring) {
            for _ in 0..2 {
                match self.receive(PrepareWait::Poll) {
                    Ok(Some(Reply::Retired)) | Err(_) => {
                        self.operation = Operation::Retired;
                        break;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                }
            }
        }
        match self.operation {
            Operation::Idle => InputState::Idle,
            Operation::Retired => InputState::Retired,
            Operation::Reading | Operation::Seeking | Operation::Retiring => InputState::Retiring,
        }
    }
}

impl Drop for Packets {
    fn drop(&mut self) {
        self.control.cancel();
        drop(self.requests.take());
    }
}

fn run(
    mut reader: Box<dyn FormatReader>,
    geometry: Option<&NativePacketGeometry>,
    requests: &mpsc::Receiver<Request>,
    completed: &mpsc::Sender<Reply>,
    control: &ReaderControl,
) {
    while let Ok(request) = requests.recv() {
        if control.is_cancelled() || matches!(request, Request::Retire) {
            break;
        }
        let reply = match request {
            Request::Read => Reply::Read(read_packet(reader.as_mut(), geometry)),
            Request::Seek { mode, to } => Reply::Seek(reader.seek(mode, to)),
            Request::Retire => break,
        };
        if control.is_cancelled() {
            break;
        }
        let failed = matches!(&reply, Reply::Read(Err(_)) | Reply::Seek(Err(_)));
        if failed {
            drop(reader);
            drop(completed.send(reply));
            drop(completed.send(Reply::Retired));
            control.wake_worker();
            return;
        }
        if completed.send(reply).is_err() {
            return;
        }
        control.wake_worker();
    }
    drop(reader);
    drop(completed.send(Reply::Retired));
    control.wake_worker();
}

fn read_packet(
    reader: &mut dyn FormatReader,
    geometry: Option<&NativePacketGeometry>,
) -> Result<Option<Packet>> {
    let mut packet = loop {
        match reader.next_packet() {
            Err(Error::ResetRequired) => {}
            result => break result?,
        }
    };
    if let (Some(geometry), Some(packet)) = (geometry, &mut packet)
        && !geometry.normalize(packet)?
    {
        return Ok(None);
    }
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use symphonia_core::{
        audio::Channels,
        codecs::audio::well_known::{
            CODEC_ID_MP3, CODEC_ID_PCM_ALAW, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_MULAW,
            CODEC_ID_PCM_S16LE, CODEC_ID_PCM_S24LE,
        },
    };

    use super::*;

    #[kithara::test]
    fn native_operations_wait_for_completion_without_resubmitting() {
        let (requests, requested) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let control = ReaderControl::new(None);
        let mut packets = Packets {
            requests: Some(requests),
            completion,
            operation: Operation::Idle,
            packet: None,
            control: control.clone(),
        };
        let pending = PendingReason::NotReady(NotReadyCause::OperationPending);
        for reason in [
            pending,
            PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
            PendingReason::Retry,
        ] {
            if reason != pending {
                control.set_pending(reason);
            }
            let Err(error) = packets.read(PrepareWait::Poll) else {
                panic!("a retained read has no packet before completion");
            };
            assert_eq!(super::super::error::pending_reason(&error), Some(reason));
        }
        assert!(matches!(requested.try_recv(), Ok(Request::Read)));
        assert!(matches!(
            requested.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        control.notify();
        assert_eq!(packets.pending_reason(), pending);
        let packet = Packet::new(7, Timestamp::new(17), Duration::new(3), vec![1, 2, 3]);
        assert!(completed.send(Reply::Read(Ok(Some(packet)))).is_ok());
        let read = packets
            .read(PrepareWait::Poll)
            .expect("completed read")
            .expect("one packet");
        assert_eq!(read.track_id, 7);
        assert_eq!(read.pts, Timestamp::new(17));
        assert_eq!(read.dur, Duration::new(3));
        assert_eq!(packets.data(), [1, 2, 3]);
        let target = Timestamp::new(19);
        assert!(
            packets
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Timestamp {
                        ts: target,
                        track_id: 7
                    },
                    PrepareWait::Poll,
                )
                .expect("pending seek")
                .is_none()
        );
        assert_eq!(packets.pending_reason(), pending);
        let Err(error) = packets.read(PrepareWait::Poll) else {
            panic!("a pending seek cannot produce a read");
        };
        assert_eq!(super::super::error::pending_reason(&error), Some(pending));
        assert!(matches!(requested.try_recv(), Ok(Request::Seek { .. })));
        assert!(
            packets
                .prepare_seek(PrepareWait::Poll)
                .expect("same seek")
                .is_none()
        );
        assert!(matches!(
            requested.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let landed = SeekedTo {
            track_id: 7,
            required_ts: target,
            actual_ts: Timestamp::new(17),
        };
        assert!(completed.send(Reply::Seek(Ok(landed))).is_ok());
        let actual = packets
            .prepare_seek(PrepareWait::Poll)
            .expect("completed seek")
            .expect("seek landing");
        assert_eq!(actual.track_id, landed.track_id);
        assert_eq!(actual.required_ts, landed.required_ts);
        assert_eq!(actual.actual_ts, landed.actual_ts);
        assert!(completed.send(Reply::Read(Ok(None))).is_ok());
        assert!(
            packets
                .read(PrepareWait::Poll)
                .expect("completed EOF")
                .is_none()
        );
        assert!(matches!(requested.try_recv(), Ok(Request::Read)));
        assert!(matches!(
            requested.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[kithara::test]
    fn native_pcm_geometry_uses_storage_width_and_preserves_packet_fields() {
        for (codec, width) in [
            (CODEC_ID_PCM_S16LE, 2_u16),
            (CODEC_ID_PCM_S24LE, 3),
            (CODEC_ID_PCM_F32LE, 4),
            (CODEC_ID_PCM_ALAW, 1),
            (CODEC_ID_PCM_MULAW, 1),
        ] {
            let mut params = AudioCodecParameters::new();
            params
                .for_codec(codec)
                .with_channels(Channels::Discrete(2))
                .with_bits_per_sample(12);
            for format in [FORMAT_ID_WAVE, FORMAT_ID_AIFF] {
                let geometry =
                    NativePacketGeometry::new(format, 7, &params, NonZeroU16::new(width * 2))
                        .expect("valid native PCM geometry")
                        .expect("native track geometry");
                let data = vec![23; usize::from(width) * 2 * 3];
                let mut packet =
                    Packet::new(7, Timestamp::new(17), Duration::new(99), data.clone());
                packet.dts = Timestamp::new(13);
                packet.trim_start = Duration::new(1);
                packet.trim_end = Duration::new(2);
                assert!(
                    geometry
                        .normalize(&mut packet)
                        .expect("complete PCM frames")
                );
                assert_eq!(packet.dur, Duration::new(3));
                assert_eq!(packet.pts, Timestamp::new(17));
                assert_eq!(packet.dts, Timestamp::new(13));
                assert_eq!(packet.track_id, 7);
                assert_eq!(packet.trim_start, Duration::new(1));
                assert_eq!(packet.trim_end, Duration::new(2));
                assert_eq!(packet.data.as_ref(), data);
            }
        }
    }

    #[kithara::test]
    fn native_geometry_rejects_incomplete_blocks_and_inconsistent_headers() {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_ADPCM_IMA_WAV)
            .with_channels(Channels::Discrete(1))
            .with_frames_per_block(9);
        assert!(matches!(
            NativePacketGeometry::new(FORMAT_ID_WAVE, 0, &params, None),
            Err(DecodeError::InvalidData { .. })
        ));
        let geometry = NativePacketGeometry::new(FORMAT_ID_WAVE, 0, &params, NonZeroU16::new(8))
            .expect("IMA geometry")
            .expect("IMA track geometry");
        let mut packet = Packet::new(0, Timestamp::ZERO, Duration::new(18), vec![0; 8]);
        assert!(geometry.normalize(&mut packet).expect("one IMA block"));
        assert_eq!(packet.dur, Duration::new(9));
        packet.data = vec![0; 7].into_boxed_slice();
        assert!(matches!(
            geometry.normalize(&mut packet),
            Err(Error::DecodeError(_))
        ));
        packet.data = Box::default();
        assert!(!geometry.normalize(&mut packet).expect("physical EOF"));
        params.for_codec(CODEC_ID_PCM_S16LE);
        assert!(matches!(
            NativePacketGeometry::new(FORMAT_ID_WAVE, 0, &params, NonZeroU16::new(4)),
            Err(DecodeError::InvalidData { .. })
        ));
    }

    #[kithara::test]
    fn riff_mpeg_packets_do_not_acquire_native_block_geometry() {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_MP3);
        assert!(
            NativePacketGeometry::new(FORMAT_ID_WAVE, 0, &params, NonZeroU16::new(1))
                .expect("MPEG track uses its packet clock")
                .is_none()
        );
    }
}
