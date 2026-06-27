#![allow(unsafe_code)]

use std::{ffi::c_void, ptr};

use kithara_bufpool::PcmBuf;
use kithara_platform::time::Duration;
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
        AudioConverterSetProperty, AudioFormatGetProperty, AudioFormatGetPropertyInfo,
        AudioFormatInfo, AudioFormatListItem, AudioStreamBasicDescription,
        AudioStreamPacketDescription, UInt32,
    },
};
use crate::{
    codec::{CodecPriming, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, PcmSpec},
};

/// Frame-level codec wrapping Apple's `AudioConverter`.
pub(crate) struct AppleCodec {
    converter: AudioConverterRef,
    input_state: Box<ConverterInputState>,
    /// Decoder-owned playback contract. Populated with the captured
    /// [`crate::GaplessInfo`] when `gapless` was requested in
    /// [`AppleCodec::open_with_config`].
    track_info: DecoderTrackInfo,
    /// Last `kAudioConverterPrimeInfo` snapshot. Used to detect when
    /// the post-first-chunk refresh changes from the init query (AAC
    /// reports priming only after consuming one input packet).
    last_prime_info: Option<AudioConverterPrimeInfo>,
    spec: PcmSpec,
    /// Whether gapless capture was requested in
    /// [`AppleCodec::open_with_config`].
    gapless_enabled: bool,
    /// Set to `true` when `gapless_enabled` is on and the init query
    /// did not yet yield priming numbers; cleared after the first
    /// post-decode refresh.
    prime_info_refresh_pending: bool,
    /// `AudioConverter`'s expected output packets-per-callback. Used to
    /// pre-grow the caller's `out` buffer before invoking the FFI so the
    /// converter writes directly into pool memory — no internal scratch
    /// buffer needed.
    frames_per_packet: u32,
    /// Input ASBD `mBytesPerPacket` snapshot, copied at open. Non-zero
    /// for CBR codecs (`LinearPCM`); zero for VBR (AAC/MP3/ALAC/FLAC).
    /// `decode_frame` uses this to size the `AudioConverter` output to
    /// match the actual input packet count when the demuxer batched
    /// multiple packets into one `Frame`.
    input_bytes_per_packet: u32,
}

// SAFETY: `AudioConverterRef` is an opaque CoreAudio handle. Apple's
unsafe impl Send for AppleCodec {}

impl AppleCodec {
    /// Inherent constructor used by [`Self::open_with_config`] (and only
    /// there). Builds a base [`AppleCodec`] from `TrackInfo` without
    /// any of the gapless / `PrimeInfo` follow-up wiring; the
    /// [`Self::open_with_config`] caller layers gapless on top when
    /// requested.
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
        let input_bytes_per_packet = input_format.mBytesPerPacket;
        let output_format = build_pcm_output_format(track.sample_rate, track.channels);

        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: input/output formats are valid stack values; `converter`
        let status = unsafe { AudioConverterNew(&input_format, &output_format, &mut converter) };
        if status != Consts::noErr {
            return Err(DecodeError::backend_msg(format!(
                "AudioConverterNew failed: {}",
                os_status_to_string(status)
            )));
        }

        if let Some(cookie) = cookie.as_ref().filter(|c| !c.is_empty()) {
            let cookie_size = UInt32::try_from(cookie.len())?;
            // SAFETY: `converter` was just successfully created above and
            let status = unsafe {
                AudioConverterSetProperty(
                    converter,
                    Consts::kAudioConverterDecompressionMagicCookie,
                    cookie_size,
                    cookie.as_ptr().cast::<c_void>(),
                )
            };
            if status != Consts::noErr {
                tracing::warn!(
                    status,
                    err = %os_status_to_string(status),
                    cookie_size = cookie.len(),
                    "AppleCodec: AudioConverterSetProperty(MagicCookie) returned non-zero (continuing)",
                );
            }
        }

        let spec = PcmSpec::checked(track.channels, track.sample_rate, "apple.codec.open")?;

        Ok(Self {
            converter,
            spec,
            frames_per_packet,
            input_bytes_per_packet,
            input_state: Box::new(ConverterInputState::default()),
            track_info: DecoderTrackInfo::default(),
            last_prime_info: None,
            gapless_enabled: false,
            prime_info_refresh_pending: false,
        })
    }

    /// Build an [`AppleCodec`] with optional gapless capture. When
    /// `gapless` is `true`, the codec queries
    /// `kAudioConverterPrimeInfo` at init and arms a one-shot refresh
    /// to fire after the first `decode_frame` (AAC populates the
    /// property only after consuming the first input packet; FLAC
    /// reports it at init).
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Backend`] when `CoreAudio`'s `AudioConverterNew`
    /// rejects the input/output ASBD pair.
    pub(crate) fn open_with_config(track: &TrackInfo, gapless: bool) -> DecodeResult<Self> {
        let mut codec = Self::open(track)?;
        if gapless {
            codec.gapless_enabled = true;
            let prime_info = prime_info_from_converter(codec.converter);
            let gapless = prime_info.and_then(gapless_info_from_prime_info);
            log_gapless_prime_info("init", prime_info, gapless);
            codec.last_prime_info = prime_info;
            let resolved = track.gapless.or(gapless);
            codec.track_info = DecoderTrackInfo {
                gapless: resolved,
                ..DecoderTrackInfo::default()
            };
            codec.prime_info_refresh_pending = resolved.is_none();
        }
        Ok(codec)
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

    /// Whether the Apple `AudioConverter` accepts this codec at the
    /// codec layer alone (i.e. without an external container parser).
    ///
    /// Scope: AAC-LC and FLAC over fMP4 (HLS), plus standalone WAV/PCM,
    /// MP3, and ALAC paired with [`super::AppleAudioFileDemuxer`]. PCM
    /// requires the demuxer to stash the source ASBD as a serialized
    /// 40-byte blob in `TrackInfo.extra_data`; ALAC requires the magic
    /// cookie in the same field.
    #[must_use]
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        matches!(
            codec,
            AudioCodec::AacLc
                | AudioCodec::AacHe
                | AudioCodec::AacHeV2
                | AudioCodec::Flac
                | AudioCodec::Pcm
                | AudioCodec::Mp3
                | AudioCodec::Alac
        )
    }
}

impl Drop for AppleCodec {
    fn drop(&mut self) {
        if !self.converter.is_null() {
            // SAFETY: `self.converter` was constructed by `AudioConverterNew`
            let _ = unsafe { AudioConverterDispose(self.converter) };
        }
    }
}

impl FrameCodec for AppleCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        _pts: Duration,
        packet_desc: &[u8],
        out: &mut PcmBuf,
    ) -> DecodeResult<u32> {
        if frame_data.is_empty() {
            out.clear();
            return Ok(0);
        }

        let desc = if packet_desc.len() == size_of::<AudioStreamPacketDescription>() {
            let mut d = AudioStreamPacketDescription::default();
            // SAFETY: `AudioStreamPacketDescription` is `#[repr(C)]` POD;
            unsafe {
                ptr::copy_nonoverlapping(
                    packet_desc.as_ptr(),
                    ptr::from_mut(&mut d).cast::<u8>(),
                    size_of::<AudioStreamPacketDescription>(),
                );
            }
            d
        } else {
            let frame_bytes = UInt32::try_from(frame_data.len())?;
            AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: frame_bytes,
            }
        };
        self.input_state.set(frame_data, desc);

        let channels = usize::from(self.spec.channels);
        let target_frames: u32 = if self.input_bytes_per_packet > 0 {
            let packets = frame_data.len() / usize::try_from(self.input_bytes_per_packet)?;
            u32::try_from(packets)?
        } else {
            self.frames_per_packet.max(Consts::AAC_FRAMES_PER_PACKET)
        };
        if target_frames == 0 {
            out.clear();
            return Ok(0);
        }
        let needed_samples = usize::try_from(target_frames)? * channels;
        out.ensure_len(needed_samples)?;

        let mut output_packets = target_frames;
        let buffer_bytes =
            u32::try_from(out.len() * usize::try_from(Consts::BYTES_PER_F32_SAMPLE).unwrap_or(0))?;
        let mut buffer_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.spec.channels),
                mDataByteSize: buffer_bytes,
                mData: out.as_mut_ptr().cast::<c_void>(),
            }],
        };

        let input_ptr =
            ptr::from_mut::<ConverterInputState>(self.input_state.as_mut()).cast::<c_void>();

        // SAFETY: `self.converter` is live; `input_ptr` points at a live
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

        if status != Consts::noErr
            && status != Consts::kAudioConverterErr_NoDataNow
            && output_packets == 0
        {
            return Err(DecodeError::backend_msg(format!(
                "AudioConverterFillComplexBuffer failed: {}",
                os_status_to_string(status)
            )));
        }

        let frames = output_packets;
        let samples_len = usize::try_from(frames)? * channels;
        out.truncate(samples_len);
        self.refresh_gapless_after_first_chunk();
        Ok(frames)
    }

    fn decoder_algo_delay(&self, codec: AudioCodec) -> u64 {
        apple_decoder_algo_delay(codec)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        // SAFETY: `self.converter` is a live handle.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != Consts::noErr {
            return Err(DecodeError::backend_msg(format!(
                "AudioConverterReset failed: {}",
                os_status_to_string(status)
            )));
        }
        self.input_state.clear();
        Ok(())
    }

    fn priming(&self, codec: AudioCodec) -> CodecPriming {
        apple_codec_priming(codec)
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
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            build_aac_input_format(track)
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
            let max_block = u32::from(u16::from_be_bytes([streaminfo[2], streaminfo[3]]))
                .max(Consts::AAC_FRAMES_PER_PACKET);

            let mut cookie =
                Vec::with_capacity(Consts::FLAC_COOKIE_PREFIX_LEN + Consts::FLAC_STREAMINFO_LEN);
            cookie.extend_from_slice(b"fLaC");
            cookie.push(0x80);
            cookie.push(0x00);
            cookie.push(0x00);
            cookie.push(Consts::FLAC_STREAMINFO_LEN_U8);
            cookie.extend_from_slice(streaminfo);

            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(track.sample_rate),
                mFormatID: Consts::kAudioFormatFLAC,
                mFramesPerPacket: max_block,
                mChannelsPerFrame: u32::from(track.channels),
                ..Default::default()
            };
            Ok(AppleInputFormat {
                asbd,
                frames_per_packet: max_block,
                cookie: Some(cookie),
            })
        }
        AudioCodec::Pcm => {
            let asbd = parse_pcm_extra_data(&track.extra_data)?;
            Ok(AppleInputFormat {
                asbd,
                cookie: None,
                frames_per_packet: 1,
            })
        }
        AudioCodec::Mp3 => {
            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(track.sample_rate),
                mFormatID: Consts::kAudioFormatMPEGLayer3,
                mFramesPerPacket: 0,
                mChannelsPerFrame: u32::from(track.channels),
                ..Default::default()
            };
            Ok(AppleInputFormat {
                asbd,
                cookie: None,
                frames_per_packet: 1152,
            })
        }
        AudioCodec::Alac => {
            if track.extra_data.is_empty() {
                return Err(DecodeError::InvalidData(
                    "alac: missing magic cookie (kAudioFilePropertyMagicCookieData)".to_string(),
                ));
            }
            let asbd = AudioStreamBasicDescription {
                mSampleRate: f64::from(track.sample_rate),
                mFormatID: Consts::kAudioFormatAppleLossless,
                mFramesPerPacket: 0,
                mChannelsPerFrame: u32::from(track.channels),
                ..Default::default()
            };
            Ok(AppleInputFormat {
                asbd,
                cookie: Some(track.extra_data.clone()),
                frames_per_packet: 4096,
            })
        }
        other => Err(DecodeError::UnsupportedCodec(other)),
    }
}

fn parse_pcm_extra_data(extra: &[u8]) -> DecodeResult<AudioStreamBasicDescription> {
    if extra.len() < size_of::<AudioStreamBasicDescription>() {
        return Err(DecodeError::InvalidData(format!(
            "pcm: extra_data too short ({} bytes, need {})",
            extra.len(),
            size_of::<AudioStreamBasicDescription>()
        )));
    }
    let mut asbd = AudioStreamBasicDescription::default();
    // SAFETY: `AudioStreamBasicDescription` is `#[repr(C)]` POD; length
    unsafe {
        ptr::copy_nonoverlapping(
            extra.as_ptr(),
            ptr::from_mut(&mut asbd).cast::<u8>(),
            size_of::<AudioStreamBasicDescription>(),
        );
    }
    Ok(asbd)
}

/// Derive the input ASBD + ESDS-wrapped cookie for an AAC track using
/// Apple's canonical `kAudioFormatProperty_FormatList` discovery path.
///
/// Why not manual ASBD construction?
///
/// For plain AAC-LC, `mFormatID = kAudioFormatMPEG4AAC` + raw ASC as
/// `MagicCookie` works. For HE-AAC v1 (SBR, AOT=5) and HE-AAC v2 (PS,
/// AOT=29) with **explicit** signalling in the ASC, `AudioConverterNew`
/// silently builds an LC pipeline and `SetProperty(MagicCookie)`
/// returns `'!dat'`. The codec then emits `'bada'` (`kAudioCodecBadDataError`)
/// on the first `FillComplexBuffer`. Apple's documented fix is to let
/// `AudioFormat` parse the ESDS and hand back the correct ASBD via
/// `kAudioFormatProperty_FormatList`, which enumerates every layer the
/// cookie can produce (sorted MOST → LEAST rich), then use the richest
/// entry's ASBD for `AudioConverterNew`.
///
/// `AudioFormat` APIs reject raw ASC bytes (also `'!dat'`) — Apple expects
/// an ESDS atom body (the same shape `AudioFileGetProperty(MagicCookieData)`
/// returns for m4a files), so we wrap the demuxer's raw ASC in the
/// minimum ISO/IEC 14496-1 descriptor chain first.
fn build_aac_input_format(track: &TrackInfo) -> DecodeResult<AppleInputFormat> {
    if track.extra_data.is_empty() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: f64::from(track.sample_rate),
            mFormatID: Consts::kAudioFormatMPEG4AAC,
            mFramesPerPacket: Consts::AAC_FRAMES_PER_PACKET,
            mChannelsPerFrame: u32::from(track.channels),
            ..Default::default()
        };
        return Ok(AppleInputFormat {
            asbd,
            cookie: None,
            frames_per_packet: Consts::AAC_FRAMES_PER_PACKET,
        });
    }

    // WHY: first byte 0x03 = full ESDS body (M4A cookie), else raw ASC to wrap (CONTEXT.md "Apple AAC input format (ESDS rationale)").
    let esds = if track.extra_data.first() == Some(&0x03) {
        track.extra_data.clone()
    } else {
        esds_wrap_asc(&track.extra_data)?
    };
    let asbd = derive_aac_asbd_from_esds(&esds, track)?;
    let frames_per_packet = if asbd.mFramesPerPacket > 0 {
        asbd.mFramesPerPacket
    } else {
        Consts::AAC_FRAMES_PER_PACKET
    };
    Ok(AppleInputFormat {
        asbd,
        frames_per_packet,
        cookie: Some(esds),
    })
}

fn derive_aac_asbd_from_esds(
    esds: &[u8],
    track: &TrackInfo,
) -> DecodeResult<AudioStreamBasicDescription> {
    let cookie_size = UInt32::try_from(esds.len())?;
    let specifier_size = UInt32::try_from(size_of::<AudioFormatInfo>())?;
    let format_info = AudioFormatInfo {
        mASBD: AudioStreamBasicDescription {
            mFormatID: Consts::kAudioFormatMPEG4AAC,
            ..Default::default()
        },
        mMagicCookie: esds.as_ptr().cast::<c_void>(),
        mMagicCookieSize: cookie_size,
    };

    let mut list_bytes: UInt32 = 0;
    // SAFETY: `format_info` is a valid stack value; its `mMagicCookie` points into the live `esds` slice; `list_bytes` is a writable `u32` slot.
    let status = unsafe {
        AudioFormatGetPropertyInfo(
            Consts::kAudioFormatProperty_FormatList,
            specifier_size,
            ptr::from_ref(&format_info).cast::<c_void>(),
            &mut list_bytes,
        )
    };
    if status != Consts::noErr || list_bytes == 0 {
        return Err(DecodeError::backend_msg(format!(
            "AudioFormatGetPropertyInfo(FormatList) failed: {} (size={}, esds_len={})",
            os_status_to_string(status),
            list_bytes,
            esds.len()
        )));
    }

    let item_size = size_of::<AudioFormatListItem>();
    let item_count = usize::try_from(list_bytes)? / item_size;
    if item_count == 0 {
        return Err(DecodeError::backend_msg(format!(
            "FormatList returned {list_bytes} bytes (< 1 item)"
        )));
    }
    let mut items: Vec<AudioFormatListItem> = vec![AudioFormatListItem::default(); item_count];
    let mut io_size = list_bytes;
    // SAFETY: `items` holds `item_count` `AudioFormatListItem`s (verified above); `format_info` is still alive.
    let status = unsafe {
        AudioFormatGetProperty(
            Consts::kAudioFormatProperty_FormatList,
            specifier_size,
            ptr::from_ref(&format_info).cast::<c_void>(),
            &mut io_size,
            items.as_mut_ptr().cast::<c_void>(),
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::backend_msg(format!(
            "AudioFormatGetProperty(FormatList) failed: {}",
            os_status_to_string(status)
        )));
    }

    let returned = usize::try_from(io_size)? / item_size;
    let chosen = items
        .first()
        .copied()
        .ok_or_else(|| DecodeError::backend_msg("FormatList returned zero items"))?;

    tracing::debug!(
        format_id = format!("{:#010x}", chosen.mASBD.mFormatID),
        sample_rate = chosen.mASBD.mSampleRate,
        channels = chosen.mASBD.mChannelsPerFrame,
        frames_per_packet = chosen.mASBD.mFramesPerPacket,
        channel_layout = format!("{:#010x}", chosen.mChannelLayoutTag),
        item_count = returned,
        esds_len = esds.len(),
        track_codec = ?track.codec,
        track_sample_rate = track.sample_rate,
        track_channels = track.channels,
        "AppleCodec: AAC ASBD derived from FormatList"
    );

    Ok(chosen.mASBD)
}

/// Wrap a raw `AudioSpecificConfig` in the minimum ISO/IEC 14496-1
/// ESDS descriptor chain Apple's `AudioFormat` / `AudioConverter`
/// APIs accept as a magic cookie. Layout documented in
/// `kithara-decode/CONTEXT.md` "Apple AAC input format (ESDS rationale)".
fn esds_wrap_asc(asc: &[u8]) -> DecodeResult<Vec<u8>> {
    let too_long = |scope: &str, n: usize| {
        DecodeError::InvalidData(format!(
            "aac: {scope} too long for short-form ESDS size field ({n} > 127)"
        ))
    };
    let dsi_body: u8 = asc
        .len()
        .try_into()
        .map_err(|_| too_long("ASC", asc.len()))?;
    let dcd_body_len = 1 + 1 + 3 + 4 + 4 + 2 + asc.len();
    let dcd_body: u8 = dcd_body_len
        .try_into()
        .map_err(|_| too_long("DecoderConfigDescriptor", dcd_body_len))?;
    let esd_body_len = 2 + 1 + 2 + dcd_body_len + 3;
    let esd_body: u8 = esd_body_len
        .try_into()
        .map_err(|_| too_long("ES_Descriptor", esd_body_len))?;

    // NOTE: ES_Descriptor chain; field-by-field layout in CONTEXT.md "Apple AAC input format".
    let header: [u8; 22] = [
        0x03, esd_body, 0x00, 0x00, 0x00, 0x04, dcd_body, 0x40, 0x15, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, dsi_body,
    ];
    let trailer: [u8; 3] = [0x06, 0x01, 0x02];
    Ok(header
        .into_iter()
        .chain(asc.iter().copied())
        .chain(trailer)
        .collect())
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
        ..Default::default()
    }
}

/// Per-codec priming requirements for the Apple `AudioConverter` backend.
/// `AudioConverter` does not strip its own MDCT/SBR warm-up — these values
/// represent how far back the demuxer must park before the seek target so
/// the codec can decode-and-discard the right number of warm-up packets.
/// `FrameCodec::priming` delegates here; the free function exists so tests
/// can pin the table without needing a live `AudioConverterRef`.
#[must_use]
pub(crate) fn apple_codec_priming(codec: AudioCodec) -> CodecPriming {
    match codec {
        AudioCodec::AacHeV2 => CodecPriming {
            frames: 4096,
            packets: 3,
            byte_margin: 32768,
        },
        AudioCodec::AacHe => CodecPriming {
            frames: 2048,
            packets: 2,
            byte_margin: 16384,
        },
        AudioCodec::AacLc => CodecPriming {
            frames: 1024,
            packets: 2,
            byte_margin: 8192,
        },
        AudioCodec::Mp3 => CodecPriming {
            frames: 1152,
            packets: 1,
            byte_margin: 4608,
        },
        _ => CodecPriming::default(),
    }
}

/// Apple-backend MP3 decoder algorithmic delay in PCM frames.
///
/// `AudioConverter` for MP3 leaves the LAME-convention 529-frame
/// algorithmic delay un-compensated. Symphonia `mpa` declares the
/// same number; mirror it here so gapless priming matches across
/// backends. Non-MP3 codecs default to 0 — AAC priming is captured
/// via `AudioConverterPrimeInfo` in the gapless capture path.
#[must_use]
pub(crate) fn apple_decoder_algo_delay(codec: AudioCodec) -> u64 {
    match codec {
        AudioCodec::Mp3 => 529,
        _ => 0,
    }
}

#[cfg(test)]
mod algo_delay_tests {
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::apple_decoder_algo_delay;

    #[kithara::test]
    fn apple_decoder_algo_delay_mp3_is_529() {
        assert_eq!(apple_decoder_algo_delay(AudioCodec::Mp3), 529);
    }

    #[kithara::test]
    fn apple_decoder_algo_delay_non_mp3_codecs_zero() {
        assert_eq!(apple_decoder_algo_delay(AudioCodec::AacLc), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::AacHe), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::AacHeV2), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::Flac), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::Opus), 0);
    }
}

#[cfg(test)]
mod priming_table_tests {
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::apple_codec_priming;
    use crate::codec::CodecPriming;

    #[kithara::test]
    fn apple_priming_aac_he_v2() {
        assert_eq!(
            apple_codec_priming(AudioCodec::AacHeV2),
            CodecPriming {
                frames: 4096,
                packets: 3,
                byte_margin: 32768
            }
        );
    }

    #[kithara::test]
    fn apple_priming_aac_he() {
        assert_eq!(
            apple_codec_priming(AudioCodec::AacHe),
            CodecPriming {
                frames: 2048,
                packets: 2,
                byte_margin: 16384
            }
        );
    }

    #[kithara::test]
    fn apple_priming_aac_lc() {
        assert_eq!(
            apple_codec_priming(AudioCodec::AacLc),
            CodecPriming {
                frames: 1024,
                packets: 2,
                byte_margin: 8192
            }
        );
    }

    #[kithara::test]
    fn apple_priming_mp3() {
        assert_eq!(
            apple_codec_priming(AudioCodec::Mp3),
            CodecPriming {
                frames: 1152,
                packets: 1,
                byte_margin: 4608
            }
        );
    }

    #[kithara::test]
    fn apple_priming_flac_is_default() {
        assert_eq!(
            apple_codec_priming(AudioCodec::Flac),
            CodecPriming::default()
        );
    }
}

#[cfg(test)]
mod aac_lc_decode_tests {
    use kithara_bufpool::PcmPool;
    use kithara_platform::time::Duration;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::AppleCodec;
    use crate::{
        codec::FrameCodec,
        demuxer::TrackInfo,
        fmp4::parsing::{CodecConfig, parse_init, parse_segment_frames},
    };

    fn read_fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    /// RED (device repro): the Apple AAC-LC decoder must turn real fMP4
    /// access units into finite PCM with no symphonia fallback compiled in
    /// — the exact decode path the size-reduced iOS framework exercises.
    /// The module already lives under the `apple` + macOS/iOS gate, so no
    /// per-item `cfg` is needed.
    #[kithara::test]
    fn apple_aac_lc_decode_produces_finite_pcm() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let extra_data = match &init.config {
            CodecConfig::Aac(bytes) | CodecConfig::Flac(bytes) => bytes.clone(),
        };
        let track = TrackInfo {
            extra_data,
            codec: init.codec,
            sample_rate: init.sample_rate,
            channels: init.channels,
            duration: None,
            gapless: init.gapless,
        };

        let seg = read_fixture("segment-1-slq-a1.m4s");
        let ranges: Vec<(usize, usize)> = parse_segment_frames(&init, &seg)
            .expect("BUG: parse segment frames")
            .iter()
            .map(|f| (f.offset, f.size))
            .collect();
        assert!(!ranges.is_empty(), "segment yielded no AAC frames");

        let pool = PcmPool::default();
        let mut codec =
            AppleCodec::open_with_config(&track, false).expect("BUG: open Apple AAC-LC codec");
        let mut pcm = Vec::new();
        for &(offset, size) in &ranges {
            let mut buf = pool.get();
            codec
                .decode_frame(&seg[offset..offset + size], Duration::ZERO, &[], &mut buf)
                .expect("BUG: decode Apple AAC-LC frame");
            pcm.extend_from_slice(&buf[..]);
        }

        assert!(!pcm.is_empty(), "Apple AAC-LC decode produced no PCM");
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "Apple AAC-LC decode produced non-finite PCM",
        );
    }
}
