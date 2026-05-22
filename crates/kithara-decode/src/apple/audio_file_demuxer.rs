#![allow(unsafe_code)]

use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;

use super::{
    audio_file::AppleAudioFile,
    consts::Consts,
    ffi::{AudioStreamBasicDescription, AudioStreamPacketDescription},
};
use crate::{
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::DecodeResult,
    traits::BoxedSource,
};

/// `Demuxer` over `AppleAudioFile` for standalone (non-fMP4) container
/// formats. Currently wires WAV/PCM, MP3, ALAC-in-M4A and ALAC-in-CAF;
/// extends via additional file-type hints.
///
/// The `AppleAudioFile` packet descriptor (a `#[repr(C)]` POD) is
/// serialized into `last_packet_desc_blob` and exposed to the codec
/// layer through `Frame::packet_desc`. CBR codecs ignore it; VBR codecs
/// (MP3, ALAC) reinterpret the bytes back into
/// `AudioStreamPacketDescription`.
pub(crate) struct AppleAudioFileDemuxer {
    file: AppleAudioFile,
    /// `Some(packets_per_call)` for CBR (`LinearPCM`) — every `next_frame`
    /// issues one batched `AudioFileReadPacketData` for that many
    /// packets. `None` for VBR (MP3, ALAC) — one packet per call so
    /// each `Frame` carries its own `AudioStreamPacketDescription`.
    cbr_batch_packets: Option<u32>,
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    last_packet_desc_blob: [u8; size_of::<AudioStreamPacketDescription>()],
    frames_per_packet: u32,
    next_packet: u64,
    total_packets: u64,
    last_read_len: usize,
}

/// Target ~16 `KiB` per CBR read — large enough to amortise the source
/// `wait_range` cost on streamed sources (HLS), small enough to keep
/// the in-flight buffer bounded.
const CBR_BATCH_TARGET_BYTES: u32 = 16 * 1024;

impl AppleAudioFileDemuxer {
    fn open(source: BoxedSource, hint: Option<u32>, codec: AudioCodec) -> DecodeResult<Self> {
        let file = AppleAudioFile::open(source, hint)?;
        let asbd = file.data_format();
        let total_packets = file.packet_count();
        let frames_per_packet = if asbd.mFramesPerPacket > 0 {
            asbd.mFramesPerPacket
        } else {
            4096
        };

        let duration = if asbd.mSampleRate > 0.0 && total_packets > 0 {
            let frames = total_packets.saturating_mul(u64::from(frames_per_packet));
            #[expect(
                clippy::cast_precision_loss,
                reason = "frames * fpp stays well within f64 precision for sub-day audio"
            )]
            Some(Duration::from_secs_f64(frames as f64 / asbd.mSampleRate))
        } else {
            None
        };

        let extra_data = match codec {
            AudioCodec::Pcm => serialize_asbd(&asbd),
            _ => file.magic_cookie().unwrap_or_default(),
        };

        let channels = u16::try_from(asbd.mChannelsPerFrame).unwrap_or(2);
        #[expect(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "sample rate is positive and well under u32::MAX in practice"
        )]
        let sample_rate = asbd.mSampleRate.max(0.0) as u32;

        let track_info = TrackInfo {
            codec,
            duration,
            extra_data,
            channels,
            sample_rate,
            gapless: None,
        };

        let (cbr_batch_packets, buf_cap) = CBR_BATCH_TARGET_BYTES
            .checked_div(asbd.mBytesPerPacket)
            .map_or_else(
                || {
                    (
                        None,
                        usize::try_from(file.max_packet_size().max(4096)).unwrap_or(64 * 1024),
                    )
                },
                |packets| {
                    let packets = packets.max(1);
                    let bytes = packets.saturating_mul(asbd.mBytesPerPacket);
                    (
                        Some(packets),
                        usize::try_from(bytes).unwrap_or(CBR_BATCH_TARGET_BYTES as usize),
                    )
                },
            );

        Ok(Self {
            file,
            track_info,
            total_packets,
            frames_per_packet,
            cbr_batch_packets,
            read_buf: vec![0u8; buf_cap],
            last_read_len: 0,
            last_packet_desc_blob: [0u8; size_of::<AudioStreamPacketDescription>()],
            next_packet: 0,
        })
    }

    pub(crate) fn open_alac_caf(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileCAFType), AudioCodec::Alac)
    }

    pub(crate) fn open_alac_m4a(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileM4AType), AudioCodec::Alac)
    }

    pub(crate) fn open_mp3(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileMP3Type), AudioCodec::Mp3)
    }

    pub(crate) fn open_wav(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileWAVEType), AudioCodec::Pcm)
    }
}

impl Demuxer for AppleAudioFileDemuxer {
    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        if self.next_packet >= self.total_packets {
            return Ok(DemuxOutcome::Eof);
        }

        let rate = f64::from(self.track_info.sample_rate.max(1));
        let start_packet = self.next_packet;
        let frame_idx = start_packet.saturating_mul(u64::from(self.frames_per_packet));
        #[expect(
            clippy::cast_precision_loss,
            reason = "frame index fits well within f64 precision for sub-day audio"
        )]
        let pts = Duration::from_secs_f64(frame_idx as f64 / rate);

        if let Some(batch_packets) = self.cbr_batch_packets {
            let remaining =
                u32::try_from(self.total_packets.saturating_sub(start_packet)).unwrap_or(u32::MAX);
            let want = batch_packets.min(remaining);
            let (bytes, packets_read) =
                self.file
                    .read_packets_cbr(start_packet, want, &mut self.read_buf)?;
            if packets_read == 0 {
                return Ok(DemuxOutcome::Eof);
            }
            self.last_read_len = bytes as usize;
            let total_frames =
                u64::from(packets_read).saturating_mul(u64::from(self.frames_per_packet));
            #[expect(
                clippy::cast_precision_loss,
                reason = "total_frames * fpp within f64 precision for sub-day audio"
            )]
            let dur = Duration::from_secs_f64(total_frames as f64 / rate);
            self.next_packet = start_packet.saturating_add(u64::from(packets_read));
            return Ok(DemuxOutcome::Frame(Frame {
                pts,
                data: &self.read_buf[..self.last_read_len],
                duration: dur,
                packet_desc: &[],
            }));
        }

        let Some((bytes, desc)) = self.file.read_packet(start_packet, &mut self.read_buf)? else {
            return Ok(DemuxOutcome::Eof);
        };

        self.last_read_len = bytes as usize;
        // SAFETY: `AudioStreamPacketDescription` is `#[repr(C)]` POD
        unsafe {
            std::ptr::copy_nonoverlapping(
                &desc as *const _ as *const u8,
                self.last_packet_desc_blob.as_mut_ptr(),
                size_of::<AudioStreamPacketDescription>(),
            );
        }

        let frames = if desc.mVariableFramesInPacket > 0 {
            u64::from(desc.mVariableFramesInPacket)
        } else {
            u64::from(self.frames_per_packet)
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "frames-per-packet small, exact in f64"
        )]
        let dur = Duration::from_secs_f64(frames as f64 / rate);

        let frame = Frame {
            pts,
            data: &self.read_buf[..self.last_read_len],
            duration: dur,
            packet_desc: &self.last_packet_desc_blob,
        };

        self.next_packet = start_packet.saturating_add(1);
        Ok(DemuxOutcome::Frame(frame))
    }

    fn seek(&mut self, target: Duration) -> DecodeResult<DemuxSeekOutcome> {
        let rate = f64::from(self.track_info.sample_rate.max(1));
        let total_frames = self
            .total_packets
            .saturating_mul(u64::from(self.frames_per_packet));
        #[expect(
            clippy::cast_precision_loss,
            reason = "frame counts within f64 range for sub-day audio"
        )]
        let total_duration = Duration::from_secs_f64(total_frames as f64 / rate);

        if target >= total_duration {
            return Ok(DemuxSeekOutcome::PastEof {
                duration: total_duration,
            });
        }

        #[expect(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "target is non-negative and well below u64::MAX in practice"
        )]
        let target_frame = (target.as_secs_f64() * rate) as u64;
        let target_packet = target_frame / u64::from(self.frames_per_packet.max(1));
        self.next_packet = target_packet;

        let landed_frame = target_packet.saturating_mul(u64::from(self.frames_per_packet));
        #[expect(
            clippy::cast_precision_loss,
            reason = "landed_frame within f64 precision range"
        )]
        let landed_at = Duration::from_secs_f64(landed_frame as f64 / rate);

        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte: None,
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

fn serialize_asbd(asbd: &AudioStreamBasicDescription) -> Vec<u8> {
    let mut out = vec![0u8; size_of::<AudioStreamBasicDescription>()];
    // SAFETY: `AudioStreamBasicDescription` is `#[repr(C)]` POD; we copy
    unsafe {
        std::ptr::copy_nonoverlapping(asbd as *const _ as *const u8, out.as_mut_ptr(), out.len());
    }
    out
}

#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod tests {
    use std::io::Cursor;

    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::AppleAudioFileDemuxer;
    use crate::demuxer::{DemuxOutcome, Demuxer};

    fn read_asset(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .join("assets")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    #[kithara::test]
    fn open_wav_demuxer_track_info_and_first_frame() {
        let bytes = read_asset("silence_1s.wav");
        let mut dx = AppleAudioFileDemuxer::open_wav(Box::new(Cursor::new(bytes)))
            .expect("open_wav must succeed");

        let info = dx.track_info();
        assert_eq!(info.codec, AudioCodec::Pcm);
        assert!(info.channels >= 1);
        assert!(info.sample_rate >= 8000);
        assert!(info.duration.is_some());

        match dx.next_frame().expect("next_frame ok") {
            DemuxOutcome::Frame(frame) => assert!(!frame.data.is_empty()),
            other => panic!("expected Frame, got {other:?}"),
        }
    }
}
