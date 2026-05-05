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

use kithara_bufpool::PcmBuf;
use kithara_stream::AudioCodec;

use super::{
    consts::{Consts, os_status_to_string},
    converter::{
        ConverterInputState, converter_input_callback, gapless_info_from_prime_info,
        log_gapless_prime_info, prime_info_from_converter,
    },
    ffi::{
        AudioBuffer, AudioBufferList, AudioConverterDispose, AudioConverterFillComplexBuffer,
        AudioConverterNew, AudioConverterPrimeInfo, AudioConverterRef, AudioConverterReset,
        AudioConverterSetProperty, AudioStreamBasicDescription, AudioStreamPacketDescription,
        UInt32,
    },
};
use crate::{
    codec::FrameCodec,
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, PcmSpec},
};

/// Configuration knobs for [`AppleCodec`] beyond what `TrackInfo`
/// carries. Set `gapless = true` to capture `kAudioConverterPrimeInfo`
/// at init and after the first decoded chunk; the resulting
/// [`crate::GaplessInfo`] is exposed via [`AppleCodec::track_info`].
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub(crate) struct AppleConfig {
    pub(crate) gapless: bool,
}

/// Frame-level codec wrapping Apple's `AudioConverter`.
pub(crate) struct AppleCodec {
    converter: AudioConverterRef,
    input_state: Box<ConverterInputState>,
    spec: PcmSpec,
    /// `AudioConverter`'s expected output packets-per-callback. Used to
    /// pre-grow the caller's `out` buffer before invoking the FFI so the
    /// converter writes directly into pool memory — no internal scratch
    /// buffer needed.
    frames_per_packet: u32,
    /// Decoder-owned playback contract. Populated with the captured
    /// [`crate::GaplessInfo`] when [`AppleConfig::gapless`] is set.
    track_info: DecoderTrackInfo,
    /// Last `kAudioConverterPrimeInfo` snapshot. Used to detect when
    /// the post-first-chunk refresh changes from the init query (AAC
    /// reports priming only after consuming one input packet).
    last_prime_info: Option<AudioConverterPrimeInfo>,
    /// Whether gapless capture was requested in [`AppleConfig::gapless`].
    gapless_enabled: bool,
    /// Set to `true` when `gapless_enabled` is on and the init query
    /// did not yet yield priming numbers; cleared after the first
    /// post-decode refresh.
    prime_info_refresh_pending: bool,
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

    /// Build an [`AppleCodec`] with extra knobs from [`AppleConfig`].
    /// When `config.gapless` is true, the codec queries
    /// `kAudioConverterPrimeInfo` at init and arms a one-shot refresh
    /// to fire after the first `decode_frame` (AAC populates the
    /// property only after consuming the first input packet; FLAC
    /// reports it at init).
    ///
    /// `FrameCodec::open` keeps its no-config shape so existing
    /// `UniversalDecoder<D, C>` callers don't break.
    ///
    /// # Errors
    ///
    /// Same as [`FrameCodec::open`].
    pub(crate) fn open_with_config(track: &TrackInfo, config: &AppleConfig) -> DecodeResult<Self> {
        let mut codec = Self::open(track)?;
        if config.gapless {
            codec.gapless_enabled = true;
            let prime_info = prime_info_from_converter(codec.converter);
            let gapless = prime_info.and_then(gapless_info_from_prime_info);
            log_gapless_prime_info("init", prime_info, gapless);
            codec.last_prime_info = prime_info;
            // Container-level gapless (MP4 `iTunSMPB` / `elst`) takes
            // priority over codec-only PrimeInfo when both are present:
            // the container value is authoritative because it survives
            // re-encoding and matches what the original encoder wrote
            // (PrimeInfo only reports the decoder's own filter delay).
            // Falls back to PrimeInfo when the demuxer carried nothing.
            let resolved = track.gapless.or(gapless);
            codec.track_info = DecoderTrackInfo {
                gapless: resolved,
                ..DecoderTrackInfo::default()
            };
            // Skip the post-first-chunk refresh when init already
            // produced trim numbers — AAC will repopulate identically
            // and a duplicate log line is just noise.
            codec.prime_info_refresh_pending = resolved.is_none();
        }
        Ok(codec)
    }

    /// Inherent constructor used by [`Self::open_with_config`] (and only
    /// there). Builds a base [`AppleCodec`] from `TrackInfo` without
    /// any of the gapless / `PrimeInfo` follow-up wiring; the
    /// [`Self::open_with_config`] caller layers gapless on top when
    /// `AppleConfig::gapless` is set.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Backend`] when `CoreAudio`'s
    /// `AudioConverterNew` rejects the input/output ASBD pair.
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
            // arrives through the ASBD path. Log + continue.
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

        Ok(Self {
            converter,
            spec,
            frames_per_packet,
            input_state: Box::new(ConverterInputState::new()),
            track_info: DecoderTrackInfo::default(),
            last_prime_info: None,
            gapless_enabled: false,
            prime_info_refresh_pending: false,
        })
    }

    /// AAC's converter populates `kAudioConverterPrimeInfo` only after
    /// at least one input packet is consumed; FLAC fills it at init.
    /// We arm a one-shot refresh during `open_with_config` and run it
    /// after the first `decode_frame` to capture priming on AAC without
    /// a separate API.
    fn refresh_gapless_after_first_chunk(&mut self) {
        if !self.gapless_enabled || !self.prime_info_refresh_pending {
            return;
        }
        self.prime_info_refresh_pending = false;
        let prime_info = prime_info_from_converter(self.converter);
        let gapless = prime_info.and_then(gapless_info_from_prime_info);
        log_gapless_prime_info("post_first_chunk", prime_info, gapless);
        if let Some(prime_info) = prime_info {
            self.last_prime_info = Some(prime_info);
            self.track_info.gapless = gapless;
        }
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
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        _pts: Duration,
        out: &mut PcmBuf,
    ) -> DecodeResult<u32> {
        if frame_data.is_empty() {
            out.clear();
            return Ok(0);
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
        let needed_samples = target_frames as usize * channels;
        out.ensure_len(needed_samples)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        let mut output_packets = target_frames;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "PCM buffer byte size fits in u32 for one packet"
        )]
        let mut buffer_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.spec.channels),
                mDataByteSize: (out.len() * Consts::BYTES_PER_F32_SAMPLE as usize) as u32,
                mData: out.as_mut_ptr() as *mut c_void,
            }],
        };

        let input_ptr = self.input_state.as_mut() as *mut ConverterInputState as *mut c_void;

        // SAFETY: `self.converter` is live; `input_ptr` points at a live
        // `ConverterInputState` owned by `self`; `buffer_list` is a
        // valid output buffer pointing into `out` for the FFI's lifetime.
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

        let frames = output_packets;
        let samples_len = frames as usize * channels;
        out.truncate(samples_len);
        // AudioConverter publishes `kAudioConverterPrimeInfo` only
        // after consuming the first input packet on AAC. We refresh
        // here so the captured `GaplessInfo` is visible by the time
        // the universal decoder reads `track_info` upstream.
        self.refresh_gapless_after_first_chunk();
        Ok(frames)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        // SAFETY: `self.converter` is a live handle.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != Consts::noErr {
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!(
                    "AudioConverterReset failed: {}",
                    os_status_to_string(status)
                ),
            ))));
        }
        self.input_state.clear();
        Ok(())
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

/// Computed Apple input-format triple — ASBD, frames-per-packet
/// estimate, optional magic cookie.
struct AppleInputFormat {
    asbd: AudioStreamBasicDescription,
    cookie: Option<Vec<u8>>,
    frames_per_packet: u32,
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
                cookie,
                frames_per_packet: Consts::AAC_FRAMES_PER_PACKET,
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
