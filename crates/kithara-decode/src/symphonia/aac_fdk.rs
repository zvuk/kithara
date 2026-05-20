//! In-tree `AudioDecoder` impl wrapping [`fdk_aac::dec::Decoder`].
//!
//! Replaces `symphonia-adapter-fdk-aac` 0.2.0 to fix two issues that
//! surfaced when we routed all AAC playback through fdk-aac for prod
//! zvuk content (HE-AAC v1/v2):
//!
//! 1. **`outputDelay` was ignored.** fdk-aac's algorithmic delay (~1744
//!    samples for AAC-LC stereo 48 kHz) was emitted as silent leading
//!    frames, producing an audible ~36 ms gap at the start of every AAC
//!    track. This adapter reads `stream_info.outputDelay` after the
//!    first decode and unconditionally strips that many leading frames
//!    before delivering the PCM, time-aligning the decoded stream with
//!    the encoded one. Container-level [`crate::GaplessTrimmer`] then
//!    owns trimming of encoder-side priming/padding (`elst`, iTunSMPB).
//!    `packet.trim_start` / `packet.trim_end` are honoured on top.
//!
//! 2. The upstream adapter parsed its own `AudioSpecificConfig`/M4A
//!    info under MPL-2.0; here we read just the fields needed for ADTS
//!    framing directly from `AudioCodecParameters` (which the
//!    demuxer/probe stage has already populated) or from
//!    `Decoder::stream_info()` after the first decode succeeds.

use std::fmt;

use fdk_aac::dec::{Decoder, DecoderError, Transport};
use symphonia::core::{
    audio::{
        AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef,
        layouts,
    },
    codecs::{
        CodecInfo,
        audio::{
            AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
            well_known::{
                CODEC_ID_AAC,
                profiles::{CODEC_PROFILE_AAC_HE, CODEC_PROFILE_AAC_HE_V2, CODEC_PROFILE_AAC_LC},
            },
        },
        registry::{RegisterableAudioDecoder, SupportedAudioCodec},
    },
    errors::{Error, Result, decode_error, unsupported_error},
    io::{BitReaderLtr, ReadBitsLtr},
    packet::PacketRef,
};
use symphonia_core::{codec_profile, support_audio_codec};

struct Consts;
impl Consts {
    /// Pre-allocated per-frame PCM buffer. AAC frames are at most
    /// 2048 samples × 2 channels for HE-AAC v2.
    const MAX_SAMPLES: usize = 8192;

    /// ADTS sample-frequency-index table (ISO/IEC 13818-7).
    const AAC_SAMPLE_RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
}

fn sample_rate_index(rate: u32) -> u8 {
    Consts::AAC_SAMPLE_RATES
        .iter()
        .position(|r| *r == rate)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(0xF)
}

fn channel_layout(channels: u8) -> Option<Channels> {
    Some(match channels {
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        3 => layouts::CHANNEL_LAYOUT_AAC_3P0,
        4 => layouts::CHANNEL_LAYOUT_AAC_4P0,
        5 => layouts::CHANNEL_LAYOUT_AAC_5P0,
        6 => layouts::CHANNEL_LAYOUT_AAC_5P1,
        7 => layouts::CHANNEL_LAYOUT_AAC_7P1,
        _ => return None,
    })
}

/// Minimal AAC stream descriptor used to build an ADTS header for each
/// fed packet. Populated either from `AudioSpecificConfig`
/// (`extra_data`) at construction time, or refined from
/// `Decoder::stream_info()` once the first packet has decoded.
#[derive(Clone, Copy, Debug)]
struct AacStreamConfig {
    /// MPEG-4 Audio Object Type (1=Main, 2=LC, 5=SBR, 29=PS, …).
    object_type: u8,
    /// AAC core sample rate (pre-SBR/PS upsampling).
    sample_rate: u32,
    /// Index into [`Consts::AAC_SAMPLE_RATES`](Consts::AAC_SAMPLE_RATES)
    /// for ADTS header byte 2/3.
    sample_rate_index: u8,
    /// Number of audio channels (post-PS, what the decoder outputs).
    channels: u8,
}

impl AacStreamConfig {
    fn from_extra_data(extra: &[u8]) -> Result<Self> {
        if extra.len() < 2 {
            return decode_error("aac: AudioSpecificConfig too short");
        }
        let mut bs = BitReaderLtr::new(extra);
        let mut object_type = read_object_type(&mut bs)?;
        let mut sample_rate = read_sample_rate(&mut bs)?;
        let mut channels = read_channel_config(&mut bs)?;
        // SBR / PS explicit signalling: re-read the extension fields
        // so `object_type` reflects the wrapped (core) AOT.
        if object_type == 5 || object_type == 29 {
            sample_rate = read_sample_rate(&mut bs)?;
            object_type = read_object_type(&mut bs)?;
            if object_type == 22 {
                channels = read_channel_config(&mut bs)?;
            }
        }
        if sample_rate == 0 {
            return decode_error("aac: invalid sample rate in AudioSpecificConfig");
        }
        if channels == 0 || channels > 7 {
            return unsupported_error("aac: unsupported channel config");
        }
        Ok(Self {
            object_type,
            sample_rate,
            sample_rate_index: sample_rate_index(sample_rate),
            channels,
        })
    }

    fn from_params(params: &AudioCodecParameters) -> Result<Self> {
        let sample_rate = params.sample_rate.ok_or(Error::Unsupported(
            "aac: sample_rate required without extra_data",
        ))?;
        let Some(ch) = &params.channels else {
            return unsupported_error("aac: channel layout required without extra_data");
        };
        let channels = u8::try_from(ch.count()).map_err(|_| {
            Error::Unsupported("aac: channel count overflows u8 in AacStreamConfig")
        })?;
        Ok(Self {
            object_type: 2,
            sample_rate,
            sample_rate_index: sample_rate_index(sample_rate),
            channels,
        })
    }
}

fn read_object_type(bs: &mut BitReaderLtr<'_>) -> Result<u8> {
    let base = bs.read_bits_leq32(5)?;
    let ot = if base == 31 {
        bs.read_bits_leq32(6)? + 32
    } else {
        base
    };
    u8::try_from(ot).map_err(|_| Error::DecodeError("aac: object_type overflows u8"))
}

fn read_sample_rate(bs: &mut BitReaderLtr<'_>) -> Result<u32> {
    let idx = bs.read_bits_leq32(4)?;
    if idx < 15 {
        Ok(Consts::AAC_SAMPLE_RATES
            .get(idx as usize)
            .copied()
            .unwrap_or_default())
    } else {
        Ok(bs.read_bits_leq32(24)?)
    }
}

fn read_channel_config(bs: &mut BitReaderLtr<'_>) -> Result<u8> {
    let idx = bs.read_bits_leq32(4)?;
    u8::try_from(idx).map_err(|_| Error::DecodeError("aac: channel_config overflows u8"))
}

/// Construct a 7-byte ADTS header (no CRC) wrapping a raw AAC payload
/// of `payload_len` bytes — fdk-aac's `Transport::Adts` decoder
/// expects every fed packet to carry this prefix.
fn build_adts_header(cfg: AacStreamConfig, payload_len: usize) -> [u8; 7] {
    let frame_length = u16::try_from(7 + payload_len).unwrap_or(u16::MAX);
    let adts_object_type = cfg.object_type.saturating_sub(1);
    [
        0xFF,
        0xF1, // MPEG-4, no CRC
        (adts_object_type << 6) | (cfg.sample_rate_index << 2) | ((cfg.channels >> 2) & 0x01),
        ((cfg.channels & 0x03) << 6) | u8::try_from((frame_length >> 11) & 0x03).unwrap_or(0),
        u8::try_from((frame_length >> 3) & 0xFF).unwrap_or(0),
        (u8::try_from(frame_length & 0x07).unwrap_or(0) << 5) | 0x1F,
        0xFC,
    ]
}

fn audio_buffer(
    channels: u8,
    sample_rate: u32,
    samples_per_frame: usize,
) -> Result<AudioBuffer<i16>> {
    let layout = channel_layout(channels)
        .ok_or(Error::Unsupported("aac: unsupported number of channels"))?;
    Ok(AudioBuffer::new(
        AudioSpec::new(sample_rate, layout),
        samples_per_frame,
    ))
}

/// Symphonia [`AudioDecoder`] wrapping libfdk-aac via [`fdk_aac`].
pub(crate) struct AacDecoder {
    decoder: Decoder,
    codec_params: AudioCodecParameters,
    config: AacStreamConfig,
    pcm: [i16; Consts::MAX_SAMPLES],
    buf: AudioBuffer<i16>,
    /// `Transport::Raw` when fmp4 / M4A supplied an
    /// `AudioSpecificConfig` via `extra_data` (packets are raw AAC
    /// frames); `Transport::Adts` when the upstream is ADTS-framed
    /// (each packet carries its own header — bare HLS AAC, .aac
    /// files).
    transport: Transport,
    /// Algorithmic-delay frames still to drop from the head of the
    /// PCM stream. Initialised from `stream_info.outputDelay` on the
    /// first successful decode, decremented as each chunk consumes it.
    /// Always applied — the decoder is the sole owner of its own
    /// algorithmic delay (`outputDelay`); container-level gapless
    /// trim (`elst`, iTunSMPB) operates on top of the time-aligned
    /// PCM stream this adapter produces.
    delay_remaining: u32,
    /// First-decode-only refresh: rebuild [`Self::buf`] and capture
    /// `outputDelay` once the decoder reports authoritative metadata.
    metadata_validated: bool,
}

impl fmt::Debug for AacDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AacDecoder")
            .field("config", &self.config)
            .field("transport", &self.transport)
            .field("delay_remaining", &self.delay_remaining)
            .field("metadata_validated", &self.metadata_validated)
            .finish_non_exhaustive()
    }
}

impl AacDecoder {
    fn try_new(params: &AudioCodecParameters, _opts: AudioDecoderOptions) -> Result<Self> {
        let (config, transport) = if let Some(extra) = &params.extra_data {
            // fmp4 / M4A path: AudioSpecificConfig in extra_data,
            // packets are raw AAC frames. `Transport::Raw` + `config_raw`
            // is the correct mode; ADTS wrapping would need profile
            // bits the spec only defines for AOT 1..=4, which mangles
            // HE-AAC v1/v2 (AOT 5 / 29) into garbage and crashes the
            // decoder.
            (AacStreamConfig::from_extra_data(extra)?, Transport::Raw)
        } else {
            // ADTS / raw HLS path: every packet carries its own ADTS
            // header, no extra_data available. The adapter prepends a
            // fresh ADTS prefix (`build_adts_header`) on each `fill`.
            (AacStreamConfig::from_params(params)?, Transport::Adts)
        };
        let mut decoder = Decoder::new(transport);
        if matches!(transport, Transport::Raw)
            && let Some(extra) = &params.extra_data
        {
            decoder
                .config_raw(extra)
                .map_err(|e| Error::DecodeError(e.message()))?;
        }
        // Pre-allocate buffer; refined in `configure_metadata` once
        // the decoder reports the actual frame size (1024 for AAC-LC,
        // 2048 for HE-AAC, etc.).
        let buf = audio_buffer(config.channels, config.sample_rate, 1024)?;
        Ok(Self {
            decoder,
            codec_params: params.clone(),
            config,
            pcm: [0; Consts::MAX_SAMPLES],
            buf,
            transport,
            delay_remaining: 0,
            metadata_validated: false,
        })
    }

    fn configure_metadata(&mut self) -> Result<()> {
        let info = self.decoder.stream_info();
        // `aacSampleRate` is the core (pre-SBR) rate — what we encode
        // into the ADTS header. `sampleRate` is the post-SBR/PS rate
        // (what fdk-aac actually emits as PCM), used for the
        // AudioBuffer spec so downstream knows the true output rate.
        let core_rate = u32::try_from(info.aacSampleRate).unwrap_or(self.config.sample_rate);
        let output_rate = u32::try_from(info.sampleRate).unwrap_or(core_rate);
        let channels = u8::try_from(info.numChannels).unwrap_or(self.config.channels);
        let samples_per_frame =
            self.decoder.decoded_frame_size().max(channels as usize) / channels.max(1) as usize;

        self.config = AacStreamConfig {
            object_type: u8::try_from(info.aot).unwrap_or(self.config.object_type),
            sample_rate: core_rate,
            sample_rate_index: sample_rate_index(core_rate),
            channels,
        };
        self.buf = audio_buffer(channels, output_rate, samples_per_frame)?;
        self.delay_remaining = info.outputDelay;
        self.metadata_validated = true;
        tracing::debug!(
            target: "kithara_decode::symphonia::aac_fdk",
            core_rate,
            output_rate,
            channels,
            samples_per_frame,
            output_delay = self.delay_remaining,
            "AAC stream metadata refreshed from decoder",
        );
        Ok(())
    }
}

impl AudioDecoder for AacDecoder {
    fn reset(&mut self) {
        self.delay_remaining = 0;
        self.metadata_validated = false;
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()
            .first()
            .expect("aac codec descriptor must exist")
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.codec_params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        let mut reader = packet.as_buf_reader();
        let payload = reader.read_buf_bytes_available_ref();
        match self.transport {
            Transport::Raw => {
                self.decoder
                    .fill(payload)
                    .map_err(|e| Error::DecodeError(e.message()))?;
            }
            Transport::Adts => {
                let header = build_adts_header(self.config, payload.len());
                let mut frame = Vec::with_capacity(header.len() + payload.len());
                frame.extend_from_slice(&header);
                frame.extend_from_slice(payload);
                self.decoder
                    .fill(&frame)
                    .map_err(|e| Error::DecodeError(e.message()))?;
            }
        }

        match self.decoder.decode_frame(&mut self.pcm) {
            Ok(()) => {}
            Err(e) if e == DecoderError::TRANSPORT_SYNC_ERROR => {
                tracing::warn!(
                    target: "kithara_decode::symphonia::aac_fdk",
                    "aac transport sync error: {}",
                    e.message()
                );
                self.buf.clear();
                return Ok(self.buf.as_generic_audio_buffer_ref());
            }
            Err(e) => return Err(Error::DecodeError(e.message())),
        }
        if !self.metadata_validated {
            self.configure_metadata()?;
        }

        let extra_trim_start = usize::try_from(packet.trim_start.get()).unwrap_or(0);
        let trim_end = usize::try_from(packet.trim_end.get()).unwrap_or(0);
        let capacity = self.decoder.decoded_frame_size();
        let pcm = &self.pcm[..capacity];

        let channels = usize::from(self.config.channels.max(1));
        let frames_in_chunk = capacity / channels;
        let delay_frames = usize::try_from(self.delay_remaining)
            .unwrap_or(usize::MAX)
            .min(frames_in_chunk);
        self.delay_remaining = self
            .delay_remaining
            .saturating_sub(u32::try_from(delay_frames).unwrap_or(u32::MAX));

        self.buf.clear();
        self.buf.render_uninit(None);
        self.buf.copy_from_slice_interleaved(&pcm);
        self.buf.trim(delay_frames + extra_trim_start, trim_end);

        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for AacDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(Self::try_new(params, *opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(
            CODEC_ID_AAC,
            "aac",
            "Advanced Audio Coding",
            &[
                codec_profile!(CODEC_PROFILE_AAC_LC, "aac-lc", "Low Complexity"),
                codec_profile!(CODEC_PROFILE_AAC_HE, "aac-he", "High Efficiency"),
                codec_profile!(CODEC_PROFILE_AAC_HE_V2, "aac-he-v2", "High Efficiency V2"),
            ]
        )]
    }
}
