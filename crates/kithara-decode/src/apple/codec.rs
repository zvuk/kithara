use std::{ffi::c_void, mem::size_of};

use kithara_apple::audio_toolbox::{
    AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE, AudioConverter, AudioFormatInfo,
    AudioFormatListItem, AudioStreamBasicDescription, AudioStreamPacketDescription,
    SingleAudioBufferList, audio_format_get_property, audio_format_get_property_info,
    pod_from_prefix,
};
use kithara_bufpool::PcmBuf;
use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;

use super::{
    consts::{Consts, os_status_to_string},
    converter::{
        ConverterInputState, gapless_info_from_prime_info, log_gapless_prime_info,
        prime_info_from_converter,
    },
    flac,
};
use crate::{
    GaplessTailCompensation,
    codec::{CodecPriming, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, PcmSpec},
};

/// Frame-level codec wrapping Apple's `AudioConverter`.
pub(crate) struct AppleCodec {
    converter: AudioConverter,
    input_state: Box<ConverterInputState>,
    /// Decoder-owned playback contract. Populated with the captured
    /// [`crate::GaplessInfo`] when `gapless` was requested in
    /// [`AppleCodec::open_with_config`].
    track_info: DecoderTrackInfo,
    /// Last `kAudioConverterPrimeInfo` snapshot. Used to detect when
    /// the post-first-chunk refresh changes from the init query (AAC
    /// reports priming only after consuming one input packet).
    last_prime_info: Option<kithara_apple::audio_toolbox::AudioConverterPrimeInfo>,
    spec: PcmSpec,
    /// Whether gapless capture was requested in
    /// [`AppleCodec::open_with_config`].
    gapless_enabled: bool,
    /// Set to `true` when `gapless_enabled` is on and the init query
    /// did not yet yield priming numbers; cleared after the first
    /// post-decode refresh.
    prime_info_refresh_pending: bool,
    /// Source-domain sample rate from `TrackInfo`; `spec.sample_rate`
    /// is the actual output/device-domain rate.
    source_sample_rate: u32,
    /// `AudioConverter`'s expected output packets-per-callback. Used to
    /// pre-grow the caller's `out` buffer before invoking the FFI so the
    /// converter writes directly into pool memory — no internal scratch
    /// buffer needed.
    frames_per_packet: u32,
    /// Input ASBD `bytes_per_packet` snapshot, copied at open. Non-zero
    /// for CBR codecs (`LinearPCM`); zero for VBR (AAC/MP3/ALAC/FLAC).
    /// `decode_frame` uses this to size the `AudioConverter` output to
    /// match the actual input packet count when the demuxer batched
    /// multiple packets into one `Frame`.
    input_bytes_per_packet: u32,
    /// True once a source-rate-changing converter has reported no more
    /// SRC tail frames after true EOF.
    eof_drained: bool,
    tail_compensation_enabled: bool,
    source_frames_seen: u64,
}

impl AppleCodec {
    const SRC_OUTPUT_MARGIN_FRAMES: u32 = 1;

    /// Inherent constructor used by [`Self::open_with_config`] (and only
    /// there). Builds a base [`AppleCodec`] from `TrackInfo` without
    /// any of the gapless / `PrimeInfo` follow-up wiring; the
    /// [`Self::open_with_config`] caller layers gapless on top when
    /// requested.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Backend`] when `CoreAudio`'s
    /// `audio_converter_new` rejects the input/output ASBD pair.
    fn open(track: &TrackInfo, target_output_rate: Option<u32>) -> DecodeResult<Self> {
        let AppleInputFormat {
            asbd: input_format,
            frames_per_packet,
            cookie,
        } = build_input_format(track)?;
        let input_bytes_per_packet = input_format.bytes_per_packet;
        let output_sample_rate = resolve_output_sample_rate(track.sample_rate, target_output_rate);
        let spec = PcmSpec::checked(track.channels, output_sample_rate, "apple.codec.output")?;
        let output_format =
            build_pcm_output_format(track.sample_rate, track.channels, target_output_rate);

        let mut converter = AudioConverter::new(&input_format, &output_format).map_err(|err| {
            DecodeError::BackendStatus {
                code: audio_toolbox_status(&err),
                op: "AudioConverterNew",
            }
        })?;

        if let Some(cookie) = cookie.as_ref().filter(|c| !c.is_empty()) {
            let status =
                converter.set_property_bytes(AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE, cookie);
            if status != Consts::noErr {
                tracing::warn!(
                    status,
                    err = %os_status_to_string(status),
                    cookie_size = cookie.len(),
                    "AppleCodec: audio_converter_set_property(MagicCookie) returned non-zero (continuing)",
                );
            }
        }

        Ok(Self {
            converter,
            spec,
            frames_per_packet,
            input_bytes_per_packet,
            source_sample_rate: track.sample_rate,
            input_state: Box::new(ConverterInputState::default()),
            track_info: DecoderTrackInfo::default(),
            last_prime_info: None,
            gapless_enabled: false,
            prime_info_refresh_pending: false,
            eof_drained: false,
            tail_compensation_enabled: output_sample_rate != track.sample_rate,
            source_frames_seen: 0,
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
    /// Returns [`DecodeError::Backend`] when `CoreAudio`'s `audio_converter_new`
    /// rejects the input/output ASBD pair.
    pub(crate) fn open_with_config(
        track: &TrackInfo,
        gapless: bool,
        target_output_rate: Option<u32>,
    ) -> DecodeResult<Self> {
        let mut codec = Self::open(track, target_output_rate)?;
        if gapless {
            codec.gapless_enabled = true;
            let prime_info = prime_info_from_converter(&codec.converter);
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
        let prime_info = prime_info_from_converter(&self.converter);
        let gapless = prime_info.and_then(gapless_info_from_prime_info);
        log_gapless_prime_info("post_first_chunk", prime_info, gapless);
        if let Some(prime_info) = prime_info {
            self.last_prime_info = Some(prime_info);
            self.track_info.gapless = gapless;
        }
    }

    fn needs_src_eof_drain(&self) -> bool {
        self.spec.sample_rate.get() != self.source_sample_rate
    }

    fn refresh_tail_compensation(&mut self) {
        if !self.tail_compensation_enabled {
            return;
        }
        self.track_info.gapless_tail = GaplessTailCompensation::for_source_frames(
            self.source_frames_seen,
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        );
    }

    fn eof_flush_frame_capacity(&self) -> DecodeResult<u32> {
        output_frame_capacity(
            self.frames_per_packet.max(Consts::AAC_FRAMES_PER_PACKET),
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        )
    }

    fn drain_eof(&mut self, out: &mut PcmBuf) -> DecodeResult<u32> {
        if !self.needs_src_eof_drain() || self.eof_drained {
            out.clear();
            return Ok(0);
        }

        self.input_state.finish();
        let frames = self.fill_converter(out, self.eof_flush_frame_capacity()?)?;
        if frames == 0 {
            self.eof_drained = true;
            self.input_state.clear();
        }
        Ok(frames)
    }

    fn fill_converter(&mut self, out: &mut PcmBuf, target_frames: u32) -> DecodeResult<u32> {
        if target_frames == 0 {
            out.clear();
            return Ok(0);
        }

        let channels = usize::from(self.spec.channels);
        let needed_samples = output_sample_capacity(target_frames, channels)?;
        out.ensure_len(needed_samples)?;

        let mut output_packets = target_frames;
        let mut buffer_list =
            SingleAudioBufferList::interleaved_f32(u32::from(self.spec.channels), out)
                .map_err(DecodeError::backend)?;
        let status = self.converter.fill_complex_buffer(
            self.input_state.as_mut(),
            1,
            &mut output_packets,
            &mut buffer_list,
        );

        if status != Consts::noErr
            && status != Consts::kAudioConverterErr_NoDataNow
            && output_packets == 0
        {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioConverterFillComplexBuffer",
            });
        }

        let frames = output_packets;
        let samples_len = output_sample_capacity(frames, channels)?;
        out.truncate(samples_len);
        Ok(frames)
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

impl FrameCodec for AppleCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        _pts: Duration,
        packet_desc: &[u8],
        out: &mut PcmBuf,
    ) -> DecodeResult<u32> {
        if frame_data.is_empty() {
            return self.drain_eof(out);
        }

        let desc = if packet_desc.len() == size_of::<AudioStreamPacketDescription>() {
            pod_from_prefix(packet_desc).ok_or(DecodeError::InvalidData {
                detail: "packet descriptor has invalid Apple ABI shape",
            })?
        } else {
            let frame_bytes = u32::try_from(frame_data.len())?;
            AudioStreamPacketDescription {
                start_offset: 0,
                variable_frames_in_packet: 0,
                data_byte_size: frame_bytes,
            }
        };
        self.eof_drained = false;
        let status = self.input_state.set(frame_data, desc);
        if status != Consts::noErr {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioConverter input packet",
            });
        }

        let input_frames = if self.input_bytes_per_packet > 0 {
            let packets = frame_data.len() / usize::try_from(self.input_bytes_per_packet)?;
            let frames =
                u64::try_from(packets)?.saturating_mul(u64::from(self.frames_per_packet.max(1)));
            u32::try_from(frames)?
        } else {
            self.frames_per_packet.max(Consts::AAC_FRAMES_PER_PACKET)
        };
        self.source_frames_seen = self
            .source_frames_seen
            .saturating_add(u64::from(input_frames));
        self.refresh_tail_compensation();
        let target_frames = output_frame_capacity(
            input_frames,
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        )?;
        let frames = self.fill_converter(out, target_frames)?;
        self.refresh_gapless_after_first_chunk();
        Ok(frames)
    }

    fn decoder_algo_delay(&self, codec: AudioCodec) -> u64 {
        apple_decoder_algo_delay(codec)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        let status = self.converter.reset();
        if status != Consts::noErr {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioConverterReset",
            });
        }
        self.input_state.clear();
        self.eof_drained = false;
        self.source_frames_seen = 0;
        self.tail_compensation_enabled = false;
        self.track_info.gapless_tail = None;
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
            let streaminfo = flac::streaminfo_body(&track.extra_data)?;
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
                sample_rate: f64::from(track.sample_rate),
                format_id: Consts::kAudioFormatFLAC,
                frames_per_packet: max_block,
                channels_per_frame: u32::from(track.channels),
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
                sample_rate: f64::from(track.sample_rate),
                format_id: Consts::kAudioFormatMPEGLayer3,
                frames_per_packet: 0,
                channels_per_frame: u32::from(track.channels),
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
                return Err(DecodeError::InvalidData {
                    detail: "alac: missing magic cookie (kAudioFilePropertyMagicCookieData)",
                });
            }
            let asbd = AudioStreamBasicDescription {
                sample_rate: f64::from(track.sample_rate),
                format_id: Consts::kAudioFormatAppleLossless,
                frames_per_packet: 0,
                channels_per_frame: u32::from(track.channels),
                ..Default::default()
            };
            Ok(AppleInputFormat {
                asbd,
                cookie: Some(track.extra_data.clone()),
                frames_per_packet: 4096,
            })
        }
        other => Err(DecodeError::UnsupportedCodec { codec: other }),
    }
}

fn parse_pcm_extra_data(extra: &[u8]) -> DecodeResult<AudioStreamBasicDescription> {
    if extra.len() < size_of::<AudioStreamBasicDescription>() {
        return Err(DecodeError::InvalidData {
            detail: "pcm: extra_data too short for AudioStreamBasicDescription",
        });
    }
    pod_from_prefix(extra).ok_or(DecodeError::InvalidData {
        detail: "pcm: invalid AudioStreamBasicDescription payload",
    })
}

/// Derive the input ASBD + ESDS-wrapped cookie for an AAC track using
/// Apple's canonical `kAudioFormatProperty_FormatList` discovery path.
///
/// Why not manual ASBD construction?
///
/// For plain AAC-LC, `format_id = kAudioFormatMPEG4AAC` + raw ASC as
/// `MagicCookie` works. For HE-AAC v1 (SBR, AOT=5) and HE-AAC v2 (PS,
/// AOT=29) with **explicit** signalling in the ASC, `audio_converter_new`
/// silently builds an LC pipeline and `SetProperty(MagicCookie)`
/// returns `'!dat'`. The codec then emits `'bada'` (`kAudioCodecBadDataError`)
/// on the first `FillComplexBuffer`. Apple's documented fix is to let
/// `AudioFormat` parse the ESDS and hand back the correct ASBD via
/// `kAudioFormatProperty_FormatList`, which enumerates every layer the
/// cookie can produce (sorted MOST → LEAST rich), then use the richest
/// entry's ASBD for `audio_converter_new`.
///
/// `AudioFormat` APIs reject raw ASC bytes (also `'!dat'`) — Apple expects
/// an ESDS atom body (the same shape `audio_file_get_property_raw(MagicCookieData)`
/// returns for m4a files), so we wrap the demuxer's raw ASC in the
/// minimum ISO/IEC 14496-1 descriptor chain first.
fn build_aac_input_format(track: &TrackInfo) -> DecodeResult<AppleInputFormat> {
    if track.extra_data.is_empty() {
        let asbd = AudioStreamBasicDescription {
            sample_rate: f64::from(track.sample_rate),
            format_id: Consts::kAudioFormatMPEG4AAC,
            frames_per_packet: Consts::AAC_FRAMES_PER_PACKET,
            channels_per_frame: u32::from(track.channels),
            ..Default::default()
        };
        return Ok(AppleInputFormat {
            asbd,
            cookie: None,
            frames_per_packet: Consts::AAC_FRAMES_PER_PACKET,
        });
    }

    // First byte 0x03 means a full ESDS body; otherwise wrap raw ASC.
    let esds = if track.extra_data.first() == Some(&0x03) {
        track.extra_data.clone()
    } else {
        esds_wrap_asc(&track.extra_data)?
    };
    let asbd = derive_aac_asbd_from_esds(&esds, track)?;
    let frames_per_packet = if asbd.frames_per_packet > 0 {
        asbd.frames_per_packet
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
    let cookie_size = u32::try_from(esds.len())?;
    let format_info = AudioFormatInfo {
        asbd: AudioStreamBasicDescription {
            format_id: Consts::kAudioFormatMPEG4AAC,
            ..Default::default()
        },
        magic_cookie: esds.as_ptr().cast::<c_void>(),
        magic_cookie_size: cookie_size,
    };

    let list_bytes =
        audio_format_get_property_info(Consts::kAudioFormatProperty_FormatList, &format_info)
            .map_err(|status| DecodeError::BackendStatus {
                code: status,
                op: "AudioFormatGetPropertyInfo(FormatList)",
            })?;
    if list_bytes == 0 {
        return Err(DecodeError::BackendStatus {
            code: Consts::noErr,
            op: "AudioFormatGetPropertyInfo(FormatList)",
        });
    }

    let item_size = size_of::<AudioFormatListItem>();
    let item_count = usize::try_from(list_bytes)? / item_size;
    if item_count == 0 {
        return Err(DecodeError::InvalidData {
            detail: "FormatList returned fewer than one item",
        });
    }
    let mut items: Vec<AudioFormatListItem> = vec![AudioFormatListItem::default(); item_count];
    let io_size = audio_format_get_property(
        Consts::kAudioFormatProperty_FormatList,
        &format_info,
        &mut items,
        list_bytes,
    )
    .map_err(|status| DecodeError::BackendStatus {
        code: status,
        op: "AudioFormatGetProperty(FormatList)",
    })?;

    let returned = usize::try_from(io_size)? / item_size;
    let chosen = items.first().copied().ok_or(DecodeError::InvalidData {
        detail: "FormatList returned zero items",
    })?;

    tracing::debug!(
        format_id = format!("{:#010x}", chosen.asbd.format_id),
        sample_rate = chosen.asbd.sample_rate,
        channels = chosen.asbd.channels_per_frame,
        frames_per_packet = chosen.asbd.frames_per_packet,
        channel_layout = format!("{:#010x}", chosen.channel_layout_tag),
        item_count = returned,
        esds_len = esds.len(),
        track_codec = ?track.codec,
        track_sample_rate = track.sample_rate,
        track_channels = track.channels,
        "AppleCodec: AAC ASBD derived from FormatList"
    );

    Ok(chosen.asbd)
}

/// Wrap a raw `AudioSpecificConfig` in the minimum ISO/IEC 14496-1
/// ESDS descriptor chain Apple's `AudioFormat` / `AudioConverter`
/// APIs accept as a magic cookie. Layout documented in
/// `kithara-decode/CONTEXT.md` "Apple AAC input format (ESDS rationale)".
fn esds_wrap_asc(asc: &[u8]) -> DecodeResult<Vec<u8>> {
    const TOO_LONG: DecodeError = DecodeError::InvalidData {
        detail: "aac: descriptor too long for short-form ESDS size field",
    };
    let dsi_body: u8 = asc.len().try_into().map_err(|_| TOO_LONG)?;
    let dcd_body_len = 1 + 1 + 3 + 4 + 4 + 2 + asc.len();
    let dcd_body: u8 = dcd_body_len.try_into().map_err(|_| TOO_LONG)?;
    let esd_body_len = 2 + 1 + 2 + dcd_body_len + 3;
    let esd_body: u8 = esd_body_len.try_into().map_err(|_| TOO_LONG)?;

    // ES_Descriptor chain; field layout is documented in CONTEXT.md.
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

fn resolve_output_sample_rate(source_rate: u32, target_output_rate: Option<u32>) -> u32 {
    match target_output_rate {
        Some(rate) if rate != source_rate => rate,
        _ => source_rate,
    }
}

fn ceil_resampled_frames(
    input_frames: u32,
    source_rate: u32,
    output_rate: u32,
) -> DecodeResult<u32> {
    if source_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "apple.codec.source",
        });
    }
    if output_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "apple.codec.output",
        });
    }

    let numerator = u128::from(input_frames) * u128::from(output_rate);
    let divisor = u128::from(source_rate);
    let frames = numerator.div_ceil(divisor);
    u32::try_from(frames).map_err(|_| DecodeError::InvalidData {
        detail: "apple output frame capacity overflow",
    })
}

fn output_frame_capacity(
    input_frames: u32,
    source_rate: u32,
    output_rate: u32,
) -> DecodeResult<u32> {
    let frames = ceil_resampled_frames(input_frames, source_rate, output_rate)?;
    let margin = if source_rate == output_rate {
        0
    } else {
        AppleCodec::SRC_OUTPUT_MARGIN_FRAMES
    };
    frames.checked_add(margin).ok_or(DecodeError::InvalidData {
        detail: "apple output frame capacity overflow",
    })
}

fn output_sample_capacity(frames: u32, channels: usize) -> DecodeResult<usize> {
    usize::try_from(frames)?
        .checked_mul(channels)
        .ok_or(DecodeError::InvalidData {
            detail: "apple output sample capacity overflow",
        })
}

fn build_pcm_output_format(
    source_rate: u32,
    channels: u16,
    target_output_rate: Option<u32>,
) -> AudioStreamBasicDescription {
    let sample_rate = resolve_output_sample_rate(source_rate, target_output_rate);
    AudioStreamBasicDescription {
        sample_rate: f64::from(sample_rate),
        format_id: Consts::kAudioFormatLinearPCM,
        format_flags: Consts::kAudioFormatFlagsNativeFloatPacked,
        bytes_per_packet: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
        frames_per_packet: 1,
        bytes_per_frame: Consts::BYTES_PER_F32_SAMPLE * u32::from(channels),
        channels_per_frame: u32::from(channels),
        bits_per_channel: Consts::BITS_PER_F32_SAMPLE,
        ..Default::default()
    }
}

fn audio_toolbox_status(err: &kithara_apple::audio_toolbox::AudioToolboxError) -> i32 {
    match err {
        kithara_apple::audio_toolbox::AudioToolboxError::Status { status, .. } => *status,
        kithara_apple::audio_toolbox::AudioToolboxError::Config { .. } => -50,
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
mod output_rate_tests {
    use std::{fs, path::Path};

    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::{
        AppleCodec, build_pcm_output_format, output_frame_capacity, resolve_output_sample_rate,
    };
    use crate::{
        codec::FrameCodec,
        demuxer::TrackInfo,
        fmp4::parsing::{CodecConfig, parse_init},
    };

    struct Consts;
    impl Consts {
        const ALT_RATE: u32 = 48_000;
        const DOWNSAMPLE_CAPACITY: u32 = 942;
        const INPUT_FRAMES: u32 = 1024;
        const SOURCE_RATE: u32 = 44_100;
        const TEST_CHANNELS: u16 = 2;
        const UPSAMPLE_CAPACITY: u32 = 1116;
    }

    fn read_fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    fn aac_lc_track() -> TrackInfo {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse AAC init");
        let extra_data = match &init.config {
            CodecConfig::Aac(bytes) | CodecConfig::Flac(bytes) => bytes.clone(),
        };
        TrackInfo {
            extra_data,
            codec: AudioCodec::AacLc,
            sample_rate: init.sample_rate,
            channels: init.channels,
            duration: None,
            gapless: init.gapless,
        }
    }

    #[kithara::test]
    fn pcm_output_format_uses_source_rate_without_target() {
        let asbd = build_pcm_output_format(Consts::SOURCE_RATE, Consts::TEST_CHANNELS, None);

        assert_eq!(asbd.sample_rate, f64::from(Consts::SOURCE_RATE));
    }

    #[kithara::test]
    fn pcm_output_format_uses_source_rate_when_target_matches() {
        let asbd = build_pcm_output_format(
            Consts::SOURCE_RATE,
            Consts::TEST_CHANNELS,
            Some(Consts::SOURCE_RATE),
        );

        assert_eq!(asbd.sample_rate, f64::from(Consts::SOURCE_RATE));
    }

    #[kithara::test]
    fn pcm_output_format_uses_target_rate_when_different() {
        let asbd = build_pcm_output_format(
            Consts::SOURCE_RATE,
            Consts::TEST_CHANNELS,
            Some(Consts::ALT_RATE),
        );

        assert_eq!(asbd.sample_rate, f64::from(Consts::ALT_RATE));
    }

    #[kithara::test]
    fn output_frame_capacity_preserves_equal_rate_passthrough() {
        let capacity = output_frame_capacity(
            Consts::INPUT_FRAMES,
            Consts::SOURCE_RATE,
            Consts::SOURCE_RATE,
        )
        .expect("BUG: compute equal-rate output capacity");

        assert_eq!(capacity, Consts::INPUT_FRAMES);
    }

    #[kithara::test]
    fn output_frame_capacity_covers_upsample_ratio() {
        let capacity =
            output_frame_capacity(Consts::INPUT_FRAMES, Consts::SOURCE_RATE, Consts::ALT_RATE)
                .expect("BUG: compute upsample output capacity");

        assert_eq!(capacity, Consts::UPSAMPLE_CAPACITY);
    }

    #[kithara::test]
    fn output_frame_capacity_covers_downsample_ratio() {
        let capacity =
            output_frame_capacity(Consts::INPUT_FRAMES, Consts::ALT_RATE, Consts::SOURCE_RATE)
                .expect("BUG: compute downsample output capacity");

        assert_eq!(capacity, Consts::DOWNSAMPLE_CAPACITY);
    }

    #[kithara::test]
    fn apple_codec_spec_uses_resolved_output_rate() {
        let track = aac_lc_track();
        let target_rate = if track.sample_rate == Consts::ALT_RATE {
            Consts::SOURCE_RATE
        } else {
            Consts::ALT_RATE
        };
        for target_output_rate in [None, Some(track.sample_rate), Some(target_rate)] {
            let codec = AppleCodec::open_with_config(&track, false, target_output_rate)
                .expect("BUG: open Apple codec with target rate");
            let expected_rate = resolve_output_sample_rate(track.sample_rate, target_output_rate);

            assert_eq!(codec.spec().sample_rate.get(), expected_rate);
        }
    }
}

#[cfg(test)]
mod aac_lc_decode_tests {
    use kithara_bufpool::PcmPool;
    use kithara_platform::time::Duration;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::{AppleCodec, ceil_resampled_frames, output_frame_capacity};
    use crate::{
        codec::FrameCodec,
        demuxer::TrackInfo,
        fmp4::parsing::{CodecConfig, Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
    };

    fn read_fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    struct Consts;
    impl Consts {
        const COMMON_TARGET_RATE: u32 = 48_000;
        const HIGH_TARGET_RATE: u32 = 96_000;
        const MAX_SRC_DELAY_FRAMES: u32 = 1024;
        const MAX_EOF_DRAIN_CALLS: usize = 8;
        const OUTPUT_LENGTH_TOLERANCE_FRAMES: u64 = 1;
        const RESAMPLED_TEST_PACKETS: usize = 16;
    }

    fn track_from_init(init: &Fmp4InitInfo) -> TrackInfo {
        let extra_data = match &init.config {
            CodecConfig::Aac(bytes) | CodecConfig::Flac(bytes) => bytes.clone(),
        };
        TrackInfo {
            extra_data,
            codec: init.codec,
            sample_rate: init.sample_rate,
            channels: init.channels,
            duration: None,
            gapless: init.gapless,
        }
    }

    fn target_rate_for_source(source_rate: u32) -> u32 {
        if source_rate < Consts::COMMON_TARGET_RATE {
            Consts::COMMON_TARGET_RATE
        } else {
            Consts::HIGH_TARGET_RATE
        }
    }

    fn decode_frames(
        codec: &mut AppleCodec,
        pool: &PcmPool,
        seg: &[u8],
        frames: &[Fmp4Frame],
    ) -> u64 {
        let mut total = 0_u64;
        for frame in frames {
            let mut buf = pool.get();
            let decoded = codec
                .decode_frame(
                    &seg[frame.offset..frame.offset + frame.size],
                    Duration::ZERO,
                    &[],
                    &mut buf,
                )
                .expect("BUG: decode Apple AAC-LC frame");
            total += u64::from(decoded);
        }
        total
    }

    fn drain_eof(codec: &mut AppleCodec, pool: &PcmPool) -> u64 {
        let mut total = 0_u64;
        for _ in 0..Consts::MAX_EOF_DRAIN_CALLS {
            let mut buf = pool.get();
            let frames = codec
                .decode_frame(&[], Duration::ZERO, &[], &mut buf)
                .expect("BUG: drain Apple AAC-LC EOF");
            if frames == 0 {
                return total;
            }
            total += u64::from(frames);
        }
        panic!("Apple AAC-LC EOF drain did not finish");
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
        let track = track_from_init(&init);

        let seg = read_fixture("segment-1-slq-a1.m4s");
        let ranges: Vec<(usize, usize)> = parse_segment_frames(&init, &seg)
            .expect("BUG: parse segment frames")
            .iter()
            .map(|f| (f.offset, f.size))
            .collect();
        assert!(!ranges.is_empty(), "segment yielded no AAC frames");

        let pool = PcmPool::default();
        let mut codec = AppleCodec::open_with_config(&track, false, None)
            .expect("BUG: open Apple AAC-LC codec");
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

    #[kithara::test]
    fn apple_aac_lc_resampled_decode_produces_ratio_sized_frames() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);
        let target_rate = target_rate_for_source(init.sample_rate);
        assert_ne!(
            target_rate, init.sample_rate,
            "fixture sample rate must differ from target rate"
        );

        let seg = read_fixture("segment-1-slq-a1.m4s");
        let ranges: Vec<(usize, usize)> = parse_segment_frames(&init, &seg)
            .expect("BUG: parse segment frames")
            .iter()
            .take(Consts::RESAMPLED_TEST_PACKETS)
            .map(|f| (f.offset, f.size))
            .collect();
        assert!(
            ranges.len() >= 2,
            "segment yielded too few AAC frames for resampled decode"
        );

        let pool = PcmPool::new(2, 8192);
        let mut codec = AppleCodec::open_with_config(&track, false, Some(target_rate))
            .expect("BUG: open Apple AAC-LC codec with target rate");
        let mut total_output_frames = 0_u64;
        let mut total_capacity_frames = 0_u64;
        let mut produced_more_than_source_packet = false;
        for &(offset, size) in &ranges {
            let mut buf = pool.get();
            let frames = codec
                .decode_frame(&seg[offset..offset + size], Duration::ZERO, &[], &mut buf)
                .expect("BUG: decode resampled Apple AAC-LC frame");
            let capacity = output_frame_capacity(
                super::Consts::AAC_FRAMES_PER_PACKET,
                init.sample_rate,
                target_rate,
            )
            .expect("BUG: compute per-packet output capacity");

            assert!(frames <= capacity, "converter wrote past computed capacity");
            produced_more_than_source_packet |= frames > super::Consts::AAC_FRAMES_PER_PACKET;
            total_output_frames += u64::from(frames);
            total_capacity_frames += u64::from(capacity);
        }

        let packet_count = u32::try_from(ranges.len()).expect("BUG: test packet count fits in u32");
        let input_frames = packet_count
            .checked_mul(super::Consts::AAC_FRAMES_PER_PACKET)
            .expect("BUG: test input frame count fits in u32");
        let ideal_total = ceil_resampled_frames(input_frames, init.sample_rate, target_rate)
            .expect("BUG: compute total resampled frame count");
        let minimum_expected = ideal_total.saturating_sub(Consts::MAX_SRC_DELAY_FRAMES);

        assert!(
            produced_more_than_source_packet,
            "upsampled decode never exceeded the old source-rate frame cap"
        );
        assert!(
            total_output_frames >= u64::from(minimum_expected),
            "resampled decode produced too few frames: {total_output_frames} < {minimum_expected}"
        );
        assert!(
            total_output_frames <= total_capacity_frames,
            "resampled decode exceeded requested output capacity"
        );
    }

    #[kithara::test]
    fn apple_aac_lc_src_eof_flush_total_output_within_one_frame() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);
        let target_rate = target_rate_for_source(init.sample_rate);
        assert_ne!(
            target_rate, init.sample_rate,
            "fixture sample rate must differ from target rate"
        );

        let seg = read_fixture("segment-1-slq-a1.m4s");
        let frames = parse_segment_frames(&init, &seg).expect("BUG: parse segment frames");
        assert!(!frames.is_empty(), "segment yielded no AAC frames");

        let pool = PcmPool::new(2, 8192);
        let mut source_codec =
            AppleCodec::open_with_config(&track, false, None).expect("BUG: open source-rate codec");
        let source_frames = decode_frames(&mut source_codec, &pool, &seg, &frames);
        let source_drain = drain_eof(&mut source_codec, &pool);
        assert_eq!(
            source_drain, 0,
            "equal-rate EOF drain must not change passthrough length"
        );

        let mut src_codec = AppleCodec::open_with_config(&track, false, Some(target_rate))
            .expect("BUG: open SRC codec");
        let before_drain = decode_frames(&mut src_codec, &pool, &seg, &frames);
        let drained = drain_eof(&mut src_codec, &pool);
        let total_output = before_drain + drained;
        let source_frames_u32 =
            u32::try_from(source_frames).expect("BUG: test source frame count fits in u32");
        let ideal = u64::from(
            ceil_resampled_frames(source_frames_u32, init.sample_rate, target_rate)
                .expect("BUG: compute ideal SRC frame count"),
        );

        assert!(
            drained > 0,
            "SRC EOF drain emitted no tail frames; before={before_drain}, ideal={ideal}"
        );
        assert!(
            total_output.abs_diff(ideal) <= Consts::OUTPUT_LENGTH_TOLERANCE_FRAMES,
            "SRC total output length off: total={total_output}, ideal={ideal}, \
             before_drain={before_drain}, drained={drained}, source={source_frames}"
        );
    }

    #[kithara::test]
    fn apple_aac_lc_passthrough_eof_drain_preserves_length() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);

        let seg = read_fixture("segment-1-slq-a1.m4s");
        let frames = parse_segment_frames(&init, &seg).expect("BUG: parse segment frames");
        assert!(!frames.is_empty(), "segment yielded no AAC frames");

        let pool = PcmPool::new(2, 8192);
        let mut codec = AppleCodec::open_with_config(&track, false, None)
            .expect("BUG: open Apple AAC-LC codec");
        let before_drain = decode_frames(&mut codec, &pool, &seg, &frames);
        let drained = drain_eof(&mut codec, &pool);

        assert!(before_drain > 0, "passthrough decode produced no frames");
        assert_eq!(drained, 0, "passthrough EOF drain produced extra frames");
    }
}
