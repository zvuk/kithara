use fdk_aac::dec::{Decoder, DecoderError, Transport};
use symphonia::core::{
    audio::{
        AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, Channels,
        GenericAudioBufferRef, layouts,
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
    /// ADTS sample-frequency-index table (ISO/IEC 13818-7).
    const AAC_SAMPLE_RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];

    /// Pre-allocated per-frame PCM buffer. AAC frames are at most
    /// 2048 samples × 2 channels for HE-AAC v2.
    const MAX_SAMPLES: usize = 8192;
}

fn sample_rate_index(rate: u32) -> u8 {
    Consts::AAC_SAMPLE_RATES
        .iter()
        .position(|r| *r == rate)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(0xF)
}

const fn channel_layout(channels: u8) -> Option<Channels> {
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

/// AAC configuration supplied at construction and refined from decoded metadata.
#[derive(Clone, Copy, Debug)]
struct AacStreamConfig {
    /// AAC core sample rate (pre-SBR/PS upsampling).
    sample_rate: u32,
    /// Number of audio channels (post-PS, what the decoder outputs).
    channels: u8,
    /// MPEG-4 Audio Object Type (1=Main, 2=LC, 5=SBR, 29=PS, …).
    object_type: u8,
    /// Index into [`Consts::AAC_SAMPLE_RATES`](Consts::AAC_SAMPLE_RATES).
    sample_rate_index: u8,
}

impl TryFrom<&[u8]> for AacStreamConfig {
    type Error = Error;

    fn try_from(extra: &[u8]) -> Result<Self> {
        if extra.len() < 2 {
            return decode_error("aac: AudioSpecificConfig too short");
        }
        let mut bs = BitReaderLtr::new(extra);
        let mut object_type = read_object_type(&mut bs)?;
        let sample_rate = read_sample_rate(&mut bs)?;
        let mut channels = read_channel_config(&mut bs)?;
        if object_type == 5 || object_type == 29 {
            if object_type == 29 {
                channels = 2;
            }
            read_sample_rate(&mut bs)?;
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
            channels,
            sample_rate_index: sample_rate_index(sample_rate),
        })
    }
}

impl TryFrom<&AudioCodecParameters> for AacStreamConfig {
    type Error = Error;

    fn try_from(params: &AudioCodecParameters) -> Result<Self> {
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
            sample_rate,
            channels,
            object_type: 2,
            sample_rate_index: sample_rate_index(sample_rate),
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

fn audio_specific_config(cfg: AacStreamConfig) -> Result<[u8; 2]> {
    if cfg.sample_rate_index >= 13 || cfg.channels == 0 || cfg.channels > 7 {
        return unsupported_error("aac: invalid ADTS stream configuration");
    }
    Ok([
        (cfg.object_type << 3) | (cfg.sample_rate_index >> 1),
        (cfg.sample_rate_index << 7) | (cfg.channels << 3),
    ])
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
#[derive(derive_more::Debug)]
pub(crate) struct AacDecoder {
    config: AacStreamConfig,
    #[debug(skip)]
    buf: AudioBuffer<i16>,
    #[debug(skip)]
    codec_params: AudioCodecParameters,
    #[debug(skip)]
    decoder: Decoder,
    #[debug(skip)]
    reset_error: Option<Error>,
    #[debug(skip)]
    pcm: [i16; Consts::MAX_SAMPLES],
    /// First-decode-only refresh: rebuild [`Self::buf`] and capture
    /// `outputDelay` once the decoder reports authoritative metadata.
    metadata_validated: bool,
    /// Algorithmic-delay frames still to drop from the head of the
    /// PCM stream. Initialised from `stream_info.outputDelay` on the
    /// first successful decode, decremented as each chunk consumes it.
    /// Always applied — the decoder is the sole owner of its own
    /// algorithmic delay (`outputDelay`); container-level gapless
    /// trim (`elst`, iTunSMPB) operates on top of the time-aligned
    /// PCM stream this adapter produces.
    delay_remaining: u32,
}

impl AacDecoder {
    fn configure_metadata(&mut self) -> Result<()> {
        let info = self.decoder.stream_info();
        let core_rate = u32::try_from(info.aacSampleRate).unwrap_or(self.config.sample_rate);
        let output_rate = u32::try_from(info.sampleRate).unwrap_or(core_rate);
        let channels = u8::try_from(info.numChannels).unwrap_or(self.config.channels);
        let samples_per_frame =
            self.decoder.decoded_frame_size().max(channels as usize) / channels.max(1) as usize;

        self.config = AacStreamConfig {
            channels,
            object_type: u8::try_from(info.aot).unwrap_or(self.config.object_type),
            sample_rate: core_rate,
            sample_rate_index: sample_rate_index(core_rate),
        };
        let layout = channel_layout(channels)
            .ok_or(Error::Unsupported("aac: unsupported number of channels"))?;
        if *self.buf.spec() != AudioSpec::new(output_rate, layout)
            || self.buf.capacity() < samples_per_frame
        {
            self.buf = audio_buffer(channels, output_rate, samples_per_frame)?;
        }
        self.codec_params.sample_rate = Some(output_rate);
        self.delay_remaining = info.outputDelay;
        self.metadata_validated = true;
        tracing::debug!(
            target: "kithara_decode::symphonia::fdk",
            core_rate,
            output_rate,
            channels,
            samples_per_frame,
            output_delay = self.delay_remaining,
            "AAC stream metadata refreshed from decoder",
        );
        Ok(())
    }

    fn try_new(params: &AudioCodecParameters, _opts: AudioDecoderOptions) -> Result<Self> {
        let config = if let Some(extra) = &params.extra_data {
            AacStreamConfig::try_from(&extra[..])?
        } else {
            AacStreamConfig::try_from(params)?
        };
        let mut decoder = Decoder::new(Transport::Raw);
        let generated;
        let extra = if let Some(extra) = params.extra_data.as_deref() {
            extra
        } else {
            generated = audio_specific_config(config)?;
            &generated
        };
        decoder
            .config_raw(extra)
            .map_err(|error| Error::DecodeError(error.message()))?;
        let info = decoder.stream_info();
        let core_rate = u32::try_from(info.aacSampleRate)
            .map_err(|_| Error::DecodeError("aac: invalid configured core rate"))?;
        let output_rate = if info.extSamplingRate > 0 {
            u32::try_from(info.extSamplingRate)
                .map_err(|_| Error::DecodeError("aac: invalid configured output rate"))?
        } else {
            core_rate
        };
        let frames = u64::try_from(info.aacSamplesPerFrame)
            .ok()
            .and_then(|frames| frames.checked_mul(u64::from(output_rate)))
            .and_then(|frames| frames.checked_div(u64::from(core_rate)))
            .filter(|frames| *frames > 0)
            .ok_or(Error::DecodeError("aac: invalid configured frame size"))?;
        let capacity = usize::try_from(frames)
            .map_err(|_| Error::DecodeError("aac: configured frame size overflow"))?;
        let buf = audio_buffer(config.channels, output_rate, capacity)?;
        let mut codec_params = params.clone();
        codec_params.max_frames_per_packet = Some(frames);
        Ok(Self {
            decoder,
            config,
            buf,
            codec_params,
            pcm: [0; Consts::MAX_SAMPLES],
            delay_remaining: 0,
            metadata_validated: false,
            reset_error: None,
        })
    }
}

impl AudioDecoder for AacDecoder {
    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()
            .first()
            .expect("invariant: supported_codecs() always returns exactly one entry")
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.codec_params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Some(error) = self.reset_error.take() {
            return Err(error);
        }
        let mut reader = packet.as_buf_reader();
        let payload = reader.read_buf_bytes_available_ref();
        let consumed = self
            .decoder
            .fill(payload)
            .map_err(|error| Error::DecodeError(error.message()))?;
        if consumed != payload.len() {
            return decode_error("aac: incomplete packet accepted by decoder");
        }

        match self.decoder.decode_frame(&mut self.pcm) {
            Ok(()) => {}
            Err(e) if e == DecoderError::TRANSPORT_SYNC_ERROR => {
                tracing::warn!(
                    target: "kithara_decode::symphonia::fdk",
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
        self.buf.render_uninit(Some(frames_in_chunk));
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

    fn reset(&mut self) {
        match Self::try_new(&self.codec_params, AudioDecoderOptions::default()) {
            Ok(codec) => *self = codec,
            Err(error) => self.reset_error = Some(error),
        }
        self.buf.clear();
        self.delay_remaining = self.decoder.stream_info().outputDelay;
    }
}

impl RegisterableAudioDecoder for AacDecoder {
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

    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(Self::try_new(params, *opts)?))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use symphonia::core::{
        audio::Audio,
        codecs::audio::{AudioCodecParameters, AudioDecoderOptions},
    };

    use super::{AacDecoder, AacStreamConfig};

    #[kithara::test]
    fn explicit_he_v2_prepares_stereo_output_at_extension_rate() {
        let extra = [0xeb, 0x8a, 0x08, 0x00];
        let config = AacStreamConfig::try_from(extra.as_slice()).expect("HE-AAC v2 config");
        assert_eq!(config.sample_rate, 22_050);
        assert_eq!(config.channels, 2);
        let mut params = AudioCodecParameters::new();
        params.extra_data = Some(Box::from(extra));
        let decoder = AacDecoder::try_new(&params, AudioDecoderOptions::default())
            .expect("prepare HE-AAC v2");
        assert_eq!(decoder.buf.spec().rate(), 44_100);
        assert_eq!(decoder.buf.spec().channels().count(), 2);
        assert_eq!(decoder.buf.capacity(), 2048);
        assert_eq!(decoder.codec_params.max_frames_per_packet, Some(2048));
    }
}
