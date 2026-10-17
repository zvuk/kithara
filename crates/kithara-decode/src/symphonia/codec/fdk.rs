use kithara_fdk::{Decoder, Error as FdkError, StreamInfo};
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

mod consts {
    /// ADTS sample-frequency-index table (ISO/IEC 13818-7).
    pub(super) const AAC_SAMPLE_RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];

    /// Pre-allocated per-frame PCM buffer. AAC frames are at most
    /// 2048 samples × 2 channels for HE-AAC v2.
    pub(super) const MAX_SAMPLES: usize = 8192;
}

fn native_error(error: &FdkError) -> Error {
    tracing::debug!(%error, "FDK AAC decoder operation failed");
    Error::DecodeError("aac: native decoder operation failed")
}

fn sample_rate_index(rate: u32) -> u8 {
    consts::AAC_SAMPLE_RATES
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
    /// Index into [`consts::AAC_SAMPLE_RATES`](consts::AAC_SAMPLE_RATES).
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
        Ok(consts::AAC_SAMPLE_RATES
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

/// Symphonia [`AudioDecoder`] owning one native FDK decoder.
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
    pcm: [i16; consts::MAX_SAMPLES],
    /// Native delay captured when the first decoded geometry is established.
    output_delay: Option<u32>,
    /// Algorithmic-delay frames still to drop from the head of the
    /// PCM stream. Initialised from `stream_info.outputDelay` on the
    /// first successful decode, decremented as each chunk consumes it.
    /// Always applied — the decoder is the sole owner of its own
    /// algorithmic delay (`outputDelay`); container-level gapless
    /// trim (`elst`, iTunSMPB) operates on top of the time-aligned
    /// PCM stream this adapter produces.
    delay_remaining: u32,
    tail_remaining: u32,
}

impl AacDecoder {
    fn configure_metadata(&mut self, info: &StreamInfo) -> Result<u32> {
        let core_rate = u32::try_from(info.aacSampleRate)
            .ok()
            .filter(|rate| *rate > 0)
            .ok_or(Error::DecodeError("aac: invalid decoded core rate"))?;
        let output_rate = u32::try_from(info.sampleRate)
            .ok()
            .filter(|rate| *rate > 0)
            .ok_or(Error::DecodeError("aac: invalid decoded output rate"))?;
        let channels = u8::try_from(info.numChannels)
            .ok()
            .filter(|channels| *channels > 0)
            .ok_or(Error::DecodeError("aac: invalid decoded channel count"))?;
        let frame_size = u32::try_from(info.frameSize)
            .ok()
            .filter(|frames| *frames > 0)
            .ok_or(Error::DecodeError("aac: invalid decoded frame size"))?;
        let object_type = u8::try_from(info.aot)
            .ok()
            .filter(|object_type| *object_type > 0)
            .ok_or(Error::DecodeError("aac: invalid decoded object type"))?;
        let samples_per_frame = usize::try_from(frame_size)
            .map_err(|_| Error::DecodeError("aac: decoded frame size overflow"))?;
        samples_per_frame
            .checked_mul(usize::from(channels))
            .filter(|samples| *samples <= self.pcm.len())
            .ok_or(Error::DecodeError("aac: decoded PCM exceeds capacity"))?;
        let layout = channel_layout(channels)
            .ok_or(Error::Unsupported("aac: unsupported number of channels"))?;
        let spec = AudioSpec::new(output_rate, layout);
        let config = AacStreamConfig {
            channels,
            object_type,
            sample_rate: core_rate,
            sample_rate_index: sample_rate_index(core_rate),
        };
        if self.output_delay.is_some() {
            self.validate_metadata(config, &spec, frame_size, info.outputDelay)?;
            return Ok(frame_size);
        }
        if *self.buf.spec() != spec || self.buf.capacity() < samples_per_frame {
            self.buf = audio_buffer(channels, output_rate, samples_per_frame)?;
        }
        self.config = config;
        self.codec_params.sample_rate = Some(output_rate);
        self.codec_params.max_frames_per_packet = Some(u64::from(frame_size));
        self.delay_remaining = info.outputDelay;
        self.tail_remaining = info.outputDelay;
        self.output_delay = Some(info.outputDelay);
        tracing::debug!(
            target: "kithara_decode::symphonia::fdk",
            core_rate,
            output_rate,
            channels,
            samples_per_frame,
            output_delay = self.delay_remaining,
            "AAC stream metadata refreshed from decoder",
        );
        Ok(frame_size)
    }

    fn validate_metadata(
        &self,
        config: AacStreamConfig,
        spec: &AudioSpec,
        frame_size: u32,
        output_delay: u32,
    ) -> Result<()> {
        if config.sample_rate != self.config.sample_rate
            || config.channels != self.config.channels
            || config.object_type != self.config.object_type
            || self.buf.spec() != spec
            || self.codec_params.max_frames_per_packet != Some(u64::from(frame_size))
            || self.output_delay != Some(output_delay)
        {
            return decode_error("aac: decoded stream geometry changed");
        }
        Ok(())
    }

    fn render_decoded(&mut self, info: &StreamInfo, draining: bool) -> Result<()> {
        self.buf.clear();
        let frame_size = self.configure_metadata(info)?;
        let frames_in_chunk = if draining {
            let frames = frame_size.min(self.tail_remaining);
            self.tail_remaining -= frames;
            frames
        } else {
            frame_size
        };
        let channels = usize::from(self.config.channels);
        let frames_in_chunk = usize::try_from(frames_in_chunk)
            .map_err(|_| Error::DecodeError("aac: decoded frame count overflow"))?;
        let capacity = frames_in_chunk
            .checked_mul(channels)
            .filter(|capacity| *capacity <= self.pcm.len())
            .ok_or(Error::DecodeError("aac: decoded PCM exceeds capacity"))?;
        let pcm = &self.pcm[..capacity];
        let delay_frames = usize::try_from(self.delay_remaining)
            .unwrap_or(usize::MAX)
            .min(frames_in_chunk);
        self.delay_remaining = self
            .delay_remaining
            .saturating_sub(u32::try_from(delay_frames).unwrap_or(u32::MAX));

        self.buf.render_uninit(Some(frames_in_chunk));
        self.buf.copy_from_slice_interleaved(&pcm);
        self.buf.trim(delay_frames, 0);
        Ok(())
    }

    fn try_new(params: &AudioCodecParameters, _opts: AudioDecoderOptions) -> Result<Self> {
        let config = if let Some(extra) = &params.extra_data {
            AacStreamConfig::try_from(&extra[..])?
        } else {
            AacStreamConfig::try_from(params)?
        };
        let generated;
        let extra = if let Some(extra) = params.extra_data.as_deref() {
            extra
        } else {
            generated = audio_specific_config(config)?;
            &generated
        };
        let decoder = Decoder::new(extra).map_err(|error| native_error(&error))?;
        let info = decoder
            .stream_info()
            .map_err(|error| native_error(&error))?;
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
        codec_params.sample_rate = Some(output_rate);
        codec_params.max_frames_per_packet = Some(frames);
        Ok(Self {
            decoder,
            config,
            buf,
            codec_params,
            pcm: [0; consts::MAX_SAMPLES],
            delay_remaining: 0,
            tail_remaining: 0,
            output_delay: None,
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
        self.buf.clear();
        if let Some(error) = self.reset_error.take() {
            return Err(error);
        }
        let draining = packet.data.is_empty();
        if draining && (self.output_delay.is_none() || self.tail_remaining == 0) {
            self.buf.clear();
            return Ok(self.buf.as_generic_audio_buffer_ref());
        }
        if !draining {
            self.decoder
                .fill(packet.data)
                .map_err(|error| native_error(&error))?;
        }
        self.decoder
            .decode_frame(&mut self.pcm, draining)
            .map_err(|error| native_error(&error))?;
        let info = *self
            .decoder
            .stream_info()
            .map_err(|error| native_error(&error))?;
        self.render_decoded(&info, draining)?;

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
    use kithara_test_fixtures::unit_fixtures::{aac_init, aac_segment};
    use kithara_test_utils::kithara;
    use symphonia::core::{
        audio::Audio,
        codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions},
        errors::Error,
        packet::PacketRef,
        units::{Duration, Timestamp},
    };

    use super::{AacDecoder, AacStreamConfig};
    use crate::{
        fmp4::parsing::{parse_init, parse_segment_frames},
        test_pools::pools,
    };

    #[kithara::test]
    fn explicit_he_v2_prepares_stereo_output_at_extension_rate(
        aac_init: Vec<u8>,
        aac_segment: Vec<u8>,
    ) {
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

        let init = parse_init(&aac_init, &pools()).expect("parse native AAC fixture");
        params.extra_data = Some(init.config.as_ref().into());
        let mut decoder = AacDecoder::try_new(&params, AudioDecoderOptions::default())
            .expect("prepare native AAC fixture");
        let frames = parse_segment_frames(&init, &aac_segment).expect("native AAC packets");
        for frame in frames.iter().take(8) {
            let packet = PacketRef::new(
                0,
                Timestamp::ZERO,
                Duration::ZERO,
                &aac_segment[frame.offset..frame.offset + frame.size],
            );
            decoder
                .decode_ref(&packet)
                .expect("decode native AAC fixture");
        }
        assert!(decoder.last_decoded().frames() > 0);
        assert_eq!(decoder.delay_remaining, 0);
        let info = *decoder
            .decoder
            .stream_info()
            .expect("decoded native metadata");
        let budgets = (decoder.delay_remaining, decoder.tail_remaining);
        let mut invalid = [info; 16];
        invalid[0].aacSampleRate = 0;
        invalid[1].aacSampleRate = -1;
        invalid[2].sampleRate = 0;
        invalid[3].sampleRate = -1;
        invalid[4].numChannels = 0;
        invalid[5].numChannels = -1;
        invalid[6].numChannels = 256;
        invalid[7].frameSize = 0;
        invalid[8].frameSize = -1;
        invalid[9].aacSampleRate += 1;
        invalid[10].sampleRate += 1;
        invalid[11].numChannels = if info.numChannels == 1 { 2 } else { 1 };
        invalid[12].frameSize *= 2;
        invalid[13].outputDelay += 1;
        invalid[14].frameSize = i32::MAX;
        invalid[15].aot = 0;
        for (case, changed) in invalid.iter().enumerate() {
            decoder
                .render_decoded(&info, false)
                .expect("retain valid native PCM");
            assert!(decoder.last_decoded().frames() > 0);
            assert!(
                matches!(
                    decoder.render_decoded(changed, true),
                    Err(Error::DecodeError(_))
                ),
                "native geometry case {case} must be rejected before rendering"
            );
            assert_eq!(
                decoder.last_decoded().frames(),
                0,
                "stale PCM in case {case}"
            );
            assert_eq!(
                (decoder.delay_remaining, decoder.tail_remaining),
                budgets,
                "native geometry case {case} must preserve delay budgets"
            );
            assert_eq!(decoder.output_delay, Some(info.outputDelay));
        }
    }
}
