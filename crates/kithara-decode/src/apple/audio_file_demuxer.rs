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
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    last_read_len: usize,
    last_packet_desc_blob: [u8; size_of::<AudioStreamPacketDescription>()],
    next_packet: u64,
    total_packets: u64,
    frames_per_packet: u32,
}

impl AppleAudioFileDemuxer {
    pub(crate) fn open_wav(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileWAVEType), AudioCodec::Pcm)
    }

    pub(crate) fn open_mp3(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileMP3Type), AudioCodec::Mp3)
    }

    pub(crate) fn open_alac_m4a(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileM4AType), AudioCodec::Alac)
    }

    pub(crate) fn open_alac_caf(source: BoxedSource) -> DecodeResult<Self> {
        Self::open(source, Some(Consts::kAudioFileCAFType), AudioCodec::Alac)
    }

    fn open(source: BoxedSource, hint: Option<u32>, codec: AudioCodec) -> DecodeResult<Self> {
        let file = AppleAudioFile::open(source, hint)?;
        let asbd = file.data_format();
        let total_packets = file.packet_count();
        let frames_per_packet = if asbd.mFramesPerPacket > 0 {
            asbd.mFramesPerPacket
        } else {
            // VBR upper bound: MP3 frame = 1152 samples; ALAC default
            // packet = 4096. Pick the larger so the read buffer is safe
            // for both before per-packet sizing kicks in.
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
            gapless: None,
            extra_data,
            channels,
            sample_rate,
        };

        let buf_cap = usize::try_from(file.max_packet_size().max(4096)).unwrap_or(64 * 1024);

        Ok(Self {
            file,
            track_info,
            read_buf: vec![0u8; buf_cap],
            last_read_len: 0,
            last_packet_desc_blob: [0u8; size_of::<AudioStreamPacketDescription>()],
            next_packet: 0,
            total_packets,
            frames_per_packet,
        })
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

        let Some((bytes, desc)) = self
            .file
            .read_packet(self.next_packet, &mut self.read_buf)?
        else {
            return Ok(DemuxOutcome::Eof);
        };

        self.last_read_len = bytes as usize;
        // SAFETY: `AudioStreamPacketDescription` is `#[repr(C)]` POD
        // with no padding traps; we copy its bit pattern into an owned
        // byte blob the codec layer will read back through `Frame::packet_desc`.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &desc as *const _ as *const u8,
                self.last_packet_desc_blob.as_mut_ptr(),
                size_of::<AudioStreamPacketDescription>(),
            );
        }

        let rate = f64::from(self.track_info.sample_rate.max(1));
        let frame_idx = self
            .next_packet
            .saturating_mul(u64::from(self.frames_per_packet));
        #[expect(
            clippy::cast_precision_loss,
            reason = "frame index fits well within f64 precision for sub-day audio"
        )]
        let pts = Duration::from_secs_f64(frame_idx as f64 / rate);
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
            data: &self.read_buf[..self.last_read_len],
            duration: dur,
            pts,
            packet_desc: &self.last_packet_desc_blob,
        };

        self.next_packet = self.next_packet.saturating_add(1);
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
    // its bit pattern into a freshly sized `Vec<u8>`. Length matches.
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
