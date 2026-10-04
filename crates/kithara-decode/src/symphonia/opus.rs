use kithara_platform::sync::Mutex;
use symphonia_core::{
    audio::{AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef},
    codecs::{
        CodecInfo,
        audio::{
            AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
            well_known::CODEC_ID_OPUS,
        },
        registry::{RegisterableAudioDecoder, SupportedAudioCodec},
    },
    errors::{Error, Result, decode_error, unsupported_error},
    packet::PacketRef,
    support_audio_codec,
};

/// Libopus decodes packets; the composed pipeline owns container head/tail trim.
pub(in crate::symphonia) struct OpusDecoder {
    params: AudioCodecParameters,
    // Symphonia requires Sync; codec operations retain exclusive access.
    decoder: Mutex<opus::Decoder>,
    output: AudioBuffer<f32>,
    pcm: Vec<f32>,
    channels: usize,
    reset_error: Option<Error>,
}

impl OpusDecoder {
    fn new(params: &AudioCodecParameters) -> Result<Self> {
        let channels = params
            .channels
            .as_ref()
            .ok_or(Error::DecodeError("opus: missing channels"))?;
        let native_channels = match channels.count() {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => return unsupported_error("opus: channel mapping requires a multistream decoder"),
        };
        let header = params
            .extra_data
            .as_deref()
            .ok_or(Error::DecodeError("opus: missing identification header"))?;
        if header.len() < 19 || &header[..8] != b"OpusHead" || header[18] != 0 {
            return unsupported_error("opus: unsupported identification header or channel mapping");
        }
        if params.sample_rate != Some(48_000) || usize::from(header[9]) != channels.count() {
            return decode_error("opus: inconsistent output specification");
        }
        let mut decoder = opus::Decoder::new(48_000, native_channels)
            .map_err(|_| Error::DecodeError("opus: decoder construction failed"))?;
        let gain = i16::from_le_bytes([header[16], header[17]]);
        decoder
            .set_gain(i32::from(gain))
            .map_err(|_| Error::DecodeError("opus: output gain rejected"))?;
        // An Opus packet contains at most 120 ms of audio at 48 kHz.
        let frames = 5_760;
        let count = channels.count();
        Ok(Self {
            params: params.clone(),
            decoder: Mutex::new(decoder),
            output: AudioBuffer::new(AudioSpec::new(48_000, channels.clone()), frames),
            pcm: vec![0.0; frames * count],
            channels: count,
            reset_error: None,
        })
    }
}

impl AudioDecoder for OpusDecoder {
    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn reset(&mut self) {
        self.reset_error = self
            .decoder
            .lock()
            .reset_state()
            .err()
            .map(|_| Error::DecodeError("opus: reset failed"));
        self.output.clear();
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Some(error) = self.reset_error.take() {
            return Err(error);
        }
        let frames = self
            .decoder
            .lock()
            .decode_float(packet.data, &mut self.pcm, false)
            .map_err(|_| Error::DecodeError("opus: invalid audio packet"))?;
        self.output.clear();
        self.output.render_uninit(Some(frames));
        self.output
            .copy_from_slice_interleaved(&&self.pcm[..frames * self.channels]);
        Ok(self.output.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.output.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_OPUS, "opus", "Opus")]
    }

    fn try_registry_new(
        params: &AudioCodecParameters,
        _opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(Self::new(params)?))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use symphonia_core::{
        audio::layouts,
        units::{Duration, Timestamp},
    };

    use super::*;

    #[kithara::test]
    fn opus_registry_decodes_pcm_and_resets_without_container_trim() {
        let input: Vec<f32> = (0_u16..960)
            .flat_map(|frame| {
                let sample =
                    (f32::from(frame) * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.5;
                [sample, sample]
            })
            .collect();
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)
                .expect("Opus encoder");
        let mut encoded = [0; 4_000];
        let len = encoder
            .encode_float(&input, &mut encoded)
            .expect("encode packet");
        let packet = PacketRef::new(0, Timestamp::new(0), Duration::new(960), &encoded[..len]);
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48_000)
            .with_channels(layouts::CHANNEL_LAYOUT_STEREO)
            .with_extra_data(Box::from(
                &b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00"[..],
            ));
        let mut decoder = crate::symphonia::registry::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .expect("registered Opus decoder");
        let first = decoder.decode_ref(&packet).expect("decode packet");
        assert_eq!(
            first.frames(),
            960,
            "the pipeline owns the declared 312-frame head trim"
        );
        let mut before = vec![0.0_f32; first.samples_interleaved()];
        first.copy_to_slice_interleaved(&mut before);
        assert!(before.iter().all(|sample| sample.is_finite()));
        assert!(before.iter().any(|sample| sample.abs() > 0.1));
        for _ in 0..10 {
            assert_eq!(
                decoder.decode_ref(&packet).expect("next packet").frames(),
                960
            );
        }
        decoder.reset();
        let after = decoder.decode_ref(&packet).expect("packet after reset");
        let mut reset = vec![0.0_f32; after.samples_interleaved()];
        after.copy_to_slice_interleaved(&mut reset);
        assert_eq!(
            reset, before,
            "reset must restore the same decoder state without a second trim owner"
        );
    }
}
