//! `AppleCodec` — Apple `AudioToolbox` `AudioConverter` as a [`FrameCodec`].
//!
//! Container parsing happens upstream in a [`crate::Demuxer`]; this codec
//! consumes already-demuxed frame bytes and produces interleaved f32 PCM.
//! Magic cookie and ASBD are derived from [`TrackInfo`] alone — no
//! `AudioFile`, no fragmented-mp4 reader, no per-codec container glue.
//!
//! Initial scope: AAC-LC and FLAC, the two codecs that flow through
//! [`crate::fmp4::Fmp4SegmentDemuxer`] today. ALAC / MP3 require codec-specific
//! ASBD + magic-cookie parameters that aren't carried by `TrackInfo` yet
//! and will land alongside the file-Symphonia migration.

#![allow(unsafe_code)]

use std::{ffi::c_void, ptr, time::Duration};

use kithara_stream::AudioCodec;

use super::{
    consts::{Consts, os_status_to_string},
    converter::{ConverterInputState, converter_input_callback},
    ffi::{
        AudioBuffer, AudioBufferList, AudioConverterDispose, AudioConverterFillComplexBuffer,
        AudioConverterNew, AudioConverterRef, AudioConverterReset, AudioConverterSetProperty,
        AudioStreamBasicDescription, AudioStreamPacketDescription, UInt32,
    },
};
use crate::{
    codec::{DecodedFrame, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::PcmSpec,
};

/// Frame-level codec wrapping Apple's `AudioConverter`.
pub(crate) struct AppleCodec {
    converter: AudioConverterRef,
    input_state: Box<ConverterInputState>,
    spec: PcmSpec,
    /// Output buffer reused across `decode_frame` calls — sized to one
    /// codec packet worth of f32 frames at construction.
    pcm_buffer: Vec<f32>,
    frames_per_packet: u32,
}

// SAFETY: `AudioConverterRef` is an opaque CoreAudio handle. Apple's
// docs guarantee thread-safety for `AudioConverterFillComplexBuffer`
// when serialised through one converter; we hold `&mut self` for every
// call, so no concurrent access. The boxed `ConverterInputState` keeps
// a stable pointer for the input callback.
unsafe impl Send for AppleCodec {}

impl AppleCodec {
    /// Whether the Apple `AudioConverter` accepts this codec at the
    /// codec layer alone (i.e. without an external container parser).
    /// Initial scope: AAC family + FLAC. ALAC / MP3 land in a follow-up
    /// when `TrackInfo` carries a codec-specific extra-data shape.
    #[must_use]
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        matches!(codec, AudioCodec::AacLc | AudioCodec::Flac)
    }
}

impl Drop for AppleCodec {
    fn drop(&mut self) {
        if !self.converter.is_null() {
            // SAFETY: `self.converter` was constructed by `AudioConverterNew`
            // and is owned exclusively by `self`.
            let _ = unsafe { AudioConverterDispose(self.converter) };
        }
    }
}

impl FrameCodec for AppleCodec {
    fn open(track: &TrackInfo) -> DecodeResult<Self> {
        let AppleInputFormat {
            asbd: input_format,
            frames_per_packet,
            cookie,
        } = build_input_format(track)?;
        let output_format = build_pcm_output_format(track.sample_rate, track.channels);

        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: input/output formats are valid stack values; `converter`
        // is a writable out-pointer.
        let status = unsafe { AudioConverterNew(&input_format, &output_format, &mut converter) };
        if status != Consts::noErr {
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!("AudioConverterNew failed: {}", os_status_to_string(status)),
            ))));
        }

        if let Some(cookie) = cookie.as_ref().filter(|c| !c.is_empty()) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "magic cookie length fits in u32 for valid configs"
            )]
            // SAFETY: `converter` was just successfully created above and
            // is owned by us; `cookie` is a readable byte slice.
            let status = unsafe {
                AudioConverterSetProperty(
                    converter,
                    Consts::kAudioConverterDecompressionMagicCookie,
                    cookie.len() as UInt32,
                    cookie.as_ptr() as *const c_void,
                )
            };
            // AAC AudioConverter rejects DecompressionMagicCookie with
            // 'NoDataNow' — it doesn't consume a cookie, ESDS metadata
            // arrives through the ASBD path. Log + continue, mirroring
            // the legacy `AppleDecoder::apply_magic_cookie` semantics.
            if status != Consts::noErr {
                tracing::warn!(
                    status,
                    err = %os_status_to_string(status),
                    cookie_size = cookie.len(),
                    "AppleCodec: AudioConverterSetProperty(MagicCookie) returned non-zero (continuing)",
                );
            }
        }

        let spec = PcmSpec {
            channels: track.channels,
            sample_rate: track.sample_rate,
        };
        // Sized for one full input packet at f32 stereo+ — grown lazily
        // by `ensure_pcm_capacity` if a packet ever exceeds it.
        let initial_frames = frames_per_packet.max(Consts::AAC_FRAMES_PER_PACKET);
        let pcm_buffer = vec![0.0f32; initial_frames as usize * track.channels as usize];

        Ok(Self {
            converter,
            input_state: Box::new(ConverterInputState::new()),
            spec,
            pcm_buffer,
            frames_per_packet,
        })
    }

    fn decode_frame(&mut self, frame_data: &[u8], _pts: Duration) -> DecodeResult<DecodedFrame> {
        if frame_data.is_empty() {
            return Ok(DecodedFrame {
                samples: Vec::new(),
                frames: 0,
            });
        }

        let desc = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            #[expect(
                clippy::cast_possible_truncation,
                reason = "frame size fits in u32 for any realistic codec packet"
            )]
            mDataByteSize: frame_data.len() as UInt32,
        };
        self.input_state.set(frame_data, desc);

        let channels = self.spec.channels as usize;
        let target_frames = self.frames_per_packet.max(Consts::AAC_FRAMES_PER_PACKET);
        self.ensure_pcm_capacity(target_frames, channels);

        let mut output_packets = target_frames;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "PCM buffer byte size fits in u32 for one packet"
        )]
        let mut buffer_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.spec.channels),
                mDataByteSize: (self.pcm_buffer.len() * Consts::BYTES_PER_F32_SAMPLE as usize)
                    as u32,
                mData: self.pcm_buffer.as_mut_ptr() as *mut c_void,
            }],
        };

        let input_ptr = self.input_state.as_mut() as *mut ConverterInputState as *mut c_void;

        // SAFETY: `self.converter` is live; `input_ptr` points at a live
        // `ConverterInputState` owned by `self`; `buffer_list` is a
        // valid output buffer pointing into `self.pcm_buffer`.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter,
                converter_input_callback,
                input_ptr,
                &mut output_packets,
                &mut buffer_list,
                ptr::null_mut(),
            )
        };

        // The callback returns `kAudioConverterErr_NoDataNow` after it
        // hands the single staged packet to AudioConverter — that's the
        // expected steady state for one-frame-in / N-frames-out drain.
        if status != Consts::noErr
            && status != Consts::kAudioConverterErr_NoDataNow
            && output_packets == 0
        {
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!(
                    "AudioConverterFillComplexBuffer failed: {}",
                    os_status_to_string(status)
                ),
            ))));
        }

        let frames = output_packets as usize;
        let samples_len = frames * channels;
        let mut samples = Vec::with_capacity(samples_len);
        samples.extend_from_slice(&self.pcm_buffer[..samples_len]);

        Ok(DecodedFrame {
            samples,
            #[expect(
                clippy::cast_possible_truncation,
                reason = "output frame count fits in u32 for one packet"
            )]
            frames: frames as u32,
        })
    }

    fn flush(&mut self) {
        // SAFETY: `self.converter` is a live handle.
        let _ = unsafe { AudioConverterReset(self.converter) };
        self.input_state.clear();
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

impl AppleCodec {
    fn ensure_pcm_capacity(&mut self, target_frames: u32, channels: usize) {
        let needed = target_frames as usize * channels;
        if self.pcm_buffer.len() < needed {
            self.pcm_buffer.resize(needed, 0.0);
        }
    }
}

/// Computed Apple input-format triple — ASBD, frames-per-packet
/// estimate, optional magic cookie.
struct AppleInputFormat {
    asbd: AudioStreamBasicDescription,
    frames_per_packet: u32,
    cookie: Option<Vec<u8>>,
}

/// Build the input ASBD + cookie + frames-per-packet for the given track.
fn build_input_format(track: &TrackInfo) -> DecodeResult<AppleInputFormat> {
    match track.codec {
        AudioCodec::AacLc => {
            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(track.sample_rate),
                mFormatID: Consts::kAudioFormatMPEG4AAC,
                mFormatFlags: 0,
                mBytesPerPacket: 0,
                mFramesPerPacket: Consts::AAC_FRAMES_PER_PACKET,
                mBytesPerFrame: 0,
                mChannelsPerFrame: u32::from(track.channels),
                mBitsPerChannel: 0,
                mReserved: 0,
            };
            // AAC magic cookie = AudioSpecificConfig bytes verbatim.
            let cookie = (!track.extra_data.is_empty()).then(|| track.extra_data.clone());
            Ok(AppleInputFormat {
                asbd,
                frames_per_packet: Consts::AAC_FRAMES_PER_PACKET,
                cookie,
            })
        }
        AudioCodec::Flac => {
            if track.extra_data.len() < Consts::FLAC_STREAMINFO_LEN {
                return Err(DecodeError::InvalidData(format!(
                    "flac: STREAMINFO too short ({} bytes, need {})",
                    track.extra_data.len(),
                    Consts::FLAC_STREAMINFO_LEN
                )));
            }
            let streaminfo = &track.extra_data[..Consts::FLAC_STREAMINFO_LEN];
            // max_block_size = u16 BE at bytes [2..4] of STREAMINFO.
            let max_block = u32::from(u16::from_be_bytes([streaminfo[2], streaminfo[3]]))
                .max(Consts::AAC_FRAMES_PER_PACKET);

            // Apple's FLAC decoder expects the magic cookie in native-FLAC
            // header form: "fLaC" + METADATA_BLOCK_HEADER + STREAMINFO.
            // Symphonia / our fmp4 demuxer surface only the STREAMINFO
            // body, so we rebuild the prefix here.
            let mut cookie =
                Vec::with_capacity(Consts::FLAC_COOKIE_PREFIX_LEN + Consts::FLAC_STREAMINFO_LEN);
            cookie.extend_from_slice(b"fLaC");
            cookie.push(0x80); // last=1, type=0 (STREAMINFO)
            cookie.push(0x00);
            cookie.push(0x00);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "FLAC_STREAMINFO_LEN is 34, fits in u8"
            )]
            {
                cookie.push(Consts::FLAC_STREAMINFO_LEN as u8);
            }
            cookie.extend_from_slice(streaminfo);

            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(track.sample_rate),
                mFormatID: Consts::kAudioFormatFLAC,
                mFormatFlags: 0,
                mBytesPerPacket: 0,
                mFramesPerPacket: max_block,
                mBytesPerFrame: 0,
                mChannelsPerFrame: u32::from(track.channels),
                mBitsPerChannel: 0,
                mReserved: 0,
            };
            Ok(AppleInputFormat {
                asbd,
                frames_per_packet: max_block,
                cookie: Some(cookie),
            })
        }
        other => Err(DecodeError::UnsupportedCodec(other)),
    }
}

fn build_pcm_output_format(sample_rate: u32, channels: u16) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: Consts::kAudioFormatLinearPCM,
        mFormatFlags: Consts::kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
        mFramesPerPacket: 1,
        mBytesPerFrame: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
        mChannelsPerFrame: u32::from(channels),
        mBitsPerChannel: Consts::BITS_PER_F32_SAMPLE,
        mReserved: 0,
    }
}
