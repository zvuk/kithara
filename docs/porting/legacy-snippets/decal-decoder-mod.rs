use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dasp::sample::Sample as DaspSample;
use symphonia::core::audio::conv::ConvertibleSample;
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error;
pub use symphonia::core::formats::SeekTo;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{
    FormatOptions, FormatReader, Packet, SeekMode, SeekedTo, Track, TrackType,
};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{Metadata, MetadataOptions};
pub use symphonia::core::units::TimeStamp;
use symphonia::core::units::{Time, TimeBase};
use symphonia::default::codecs;
use tap::TapFallible;
use thiserror::Error;
use tracing::{error, info, warn};

mod resampler;
pub use resampler::*;
mod channel_buffer;
mod source;
pub use source::*;

use crate::{ChannelCount, SampleRate};
mod vec_ext;

#[derive(Error, Debug)]
pub enum DecoderError {
    #[error("No tracks were found")]
    NoTracks,
    #[error("No readable format was discovered: {0}")]
    FormatNotFound(symphonia::core::errors::Error),
    #[error("The codec is unsupported: {0}")]
    UnsupportedCodec(symphonia::core::errors::Error),
    #[error("The format is unsupported: {0}")]
    UnsupportedFormat(String),
    #[error("Error occurred during decoding: {0}")]
    DecodeError(symphonia::core::errors::Error),
    #[error("Recoverable error: {0}")]
    Recoverable(&'static str),
    #[error("Error seeking: {0}")]
    Seek(symphonia::core::errors::Error),
    #[error("Only audio tracks are supported")]
    InvalidTrackType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentPosition {
    pub position: Duration,
    pub retrieval_time: Option<Duration>,
}

const NANOS_PER_SEC: f64 = 1_000_000_000.0;

#[derive(Clone, Debug)]
pub struct DecoderSettings {
    enable_gapless: bool,
}

impl DecoderSettings {
    pub fn new() -> Self {
        Self {
            enable_gapless: true,
        }
    }

    pub fn enable_gapless(mut self, enable_gapless: bool) -> Self {
        self.enable_gapless = enable_gapless;
        self
    }
}

impl Default for DecoderSettings {
    fn default() -> Self {
        Self::new()
    }
}

static CODEC_REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();

    #[cfg(feature = "decoder-fdk-aac")]
    registry.register_audio_decoder::<symphonia_adapter_fdk_aac::AacDecoder>();

    #[cfg(feature = "symphonia-adapter-libopus")]
    registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();

    #[cfg(all(
        not(feature = "decoder-fdk-aac"),
        any(
            feature = "decoder-aac",
            feature = "decoder-all",
            feature = "decoder-all-codecs"
        )
    ))]
    registry.register_audio_decoder::<codecs::AacDecoder>();

    #[cfg(any(
        feature = "decoder-adpcm",
        feature = "decoder-all",
        feature = "decoder-all-codecs"
    ))]
    registry.register_audio_decoder::<codecs::AdpcmDecoder>();

    #[cfg(any(
        feature = "decoder-alac",
        feature = "decoder-all",
        feature = "decoder-all-codecs"
    ))]
    registry.register_audio_decoder::<codecs::AlacDecoder>();

    #[cfg(any(
        feature = "decoder-flac",
        feature = "decoder-all",
        feature = "decoder-all-codecs"
    ))]
    registry.register_audio_decoder::<codecs::FlacDecoder>();

    #[cfg(any(
        feature = "decoder-mp1",
        feature = "decoder-mp2",
        feature = "decoder-mp3",
        feature = "decoder-mpa",
        feature = "decoder-all",
        feature = "decoder-all-codecs"
    ))]
    registry.register_audio_decoder::<codecs::MpaDecoder>();

    #[cfg(any(
        feature = "decoder-pcm",
        feature = "decoder-all",
        feature = "decoder-all-codecs"
    ))]
    registry.register_audio_decoder::<codecs::PcmDecoder>();

    #[cfg(any(
        feature = "decoder-vorbis",
        feature = "decoder-all",
        feature = "decoder-all-codecs"
    ))]
    registry.register_audio_decoder::<codecs::VorbisDecoder>();
    registry
});

pub struct Decoder<T: Sample + dasp::sample::Sample> {
    buf: Vec<T>,
    sample_buf: Vec<T>,
    decoder: Box<dyn AudioDecoder>,
    reader: Box<dyn FormatReader>,
    time_base: TimeBase,
    buf_len: usize,
    volume: T::Float,
    track_id: u32,
    input_channels: ChannelCount,
    output_channels: ChannelCount,
    timestamp: u64,
    is_paused: bool,
    sample_rate: SampleRate,
    num_frames: Option<u64>,
    seek_required_ts: Option<u64>,
    settings: DecoderSettings,
}

fn create_decoder(
    reader: &dyn FormatReader,
) -> Result<(Box<dyn AudioDecoder>, Track), DecoderError> {
    let track = match reader.default_track(TrackType::Audio) {
        Some(track) => track.to_owned(),
        None => return Err(DecoderError::NoTracks),
    };

    let decode_opts = AudioDecoderOptions { verify: true };
    let Some(CodecParameters::Audio(codec_params)) = &track.codec_params else {
        return Err(DecoderError::InvalidTrackType);
    };
    let symphonia_decoder = match CODEC_REGISTRY.make_audio_decoder(codec_params, &decode_opts) {
        Ok(decoder) => decoder,
        Err(e) => return Err(DecoderError::UnsupportedCodec(e)),
    };

    Ok((symphonia_decoder, track))
}

impl<T> Decoder<T>
where
    T: Sample + dasp::sample::Sample + ConvertibleSample,
{
    pub fn new(
        source: Box<dyn Source>,
        volume: T::Float,
        output_channels: ChannelCount,
        settings: DecoderSettings,
    ) -> Result<Self, DecoderError> {
        let mut hint = Hint::new();
        if let Some(extension) = source.get_file_ext() {
            hint.with_extension(&extension);
        }
        let mss = MediaSourceStream::new(source.as_media_source(), Default::default());

        let format_opts = FormatOptions {
            enable_gapless: settings.enable_gapless,
            ..FormatOptions::default()
        };
        let metadata_opts = MetadataOptions::default();

        let reader =
            match symphonia::default::get_probe().probe(&hint, mss, format_opts, metadata_opts) {
                Ok(probed) => probed,
                Err(e) => return Err(DecoderError::FormatNotFound(e)),
            };

        let (decoder, track) = create_decoder(&*reader)?;
        let num_frames = track.num_frames;

        // If no time base found, default to a dummy one
        // and attempt to calculate it from the sample rate later
        let time_base = track.time_base.unwrap_or_else(|| TimeBase::new(1, 1));

        let mut decoder = Self {
            decoder,
            reader,
            time_base,
            buf_len: 0,
            input_channels: ChannelCount(0),
            output_channels,
            track_id: track.id,
            buf: vec![],
            sample_buf: vec![],
            volume,
            timestamp: 0,
            is_paused: false,
            sample_rate: SampleRate(0),
            seek_required_ts: None,
            num_frames,
            settings,
        };
        decoder.initialize()?;

        Ok(decoder)
    }

    pub fn metadata(&mut self) -> Metadata<'_> {
        self.reader.metadata()
    }

    pub fn track_id(&self) -> u32 {
        self.track_id
    }

    pub fn duration(&self) -> Option<Duration> {
        let num_frames = self.num_frames?;
        if self.time_base.denom == 1 {
            return None;
        }
        Some(self.timestamp_to_duration(num_frames))
    }

    pub fn set_volume(&mut self, volume: T::Float) {
        self.volume = volume;
    }

    pub fn volume(&self) -> T::Float {
        self.volume
    }

    pub fn pause(&mut self) {
        self.is_paused = true;
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused
    }

    pub fn resume(&mut self) {
        self.is_paused = false;
    }

    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    pub fn seek(&mut self, time: Duration) -> Result<SeekedTo, DecoderError> {
        let position = self.current_position();
        let seek_result = match self.reader_seek(time) {
            Ok(result) => {
                self.seek_required_ts = Some(result.required_ts);
                Ok(result)
            }
            Err(e) => {
                // Seek was probably out of bounds
                warn!("Error seeking: {e:?}. Resetting to previous position");
                match self.reader_seek(position.position) {
                    Ok(seeked_to) => {
                        info!("Reset position to {seeked_to:?}");
                        self.seek_required_ts = Some(seeked_to.required_ts);
                        // Reset succeeded, but send the original error back to the caller since the
                        // intended seek failed
                        Err(e)
                    }
                    err_result @ Err(_) => {
                        error!("Error resetting to previous position: {err_result:?}");
                        err_result
                    }
                }
            }
        };

        // Per the docs, decoders need to be reset after seeking
        self.decoder.reset();
        seek_result
    }

    fn timestamp_to_duration(&self, timestamp: u64) -> Duration {
        let time = self.time_base.calc_time(timestamp);
        let millis = ((time.seconds as f64 + time.frac) * 1000.0) as u64;
        Duration::from_millis(millis)
    }

    pub fn current_position(&self) -> CurrentPosition {
        let retrieval_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .tap_err(|e| {
                warn!(
                    "Unable to get duration from system time. The system clock is probably in a \
                     bad state: {e:?}"
                )
            })
            .ok();

        CurrentPosition {
            position: self.timestamp_to_duration(self.timestamp),
            retrieval_time,
        }
    }

    fn reader_seek(&mut self, time: Duration) -> Result<SeekedTo, DecoderError> {
        let seek_time = Time::new(time.as_secs(), time.subsec_nanos() as f64 / NANOS_PER_SEC);
        let res = self.reader.seek(
            SeekMode::Coarse,
            SeekTo::Time {
                time: seek_time,
                track_id: Some(self.track_id),
            },
        );
        self.decoder.reset();
        if res.is_ok() {
            // Manually set the timestamp here in case it's queried before we decode the next packet
            self.timestamp = self.time_base.calc_timestamp(seek_time);
        }
        res.map_err(DecoderError::Seek)
    }

    fn initialize(&mut self) -> Result<(), DecoderError> {
        let mut samples_skipped = 0;

        loop {
            self.next()?;
            if self.time_base.denom == 1 {
                self.time_base = TimeBase::new(1, self.sample_rate.0);
            }
            if !self.settings.enable_gapless {
                break;
            }
            if let Some(mut index) = self.current().iter().position(|s| *s != T::MID) {
                // Edge case: if the first non-silent sample is on an odd-numbered index, we'll
                // start on the wrong channel.
                // This only matters for stereo outputs.
                if self.output_channels.0 == 2 && index % 2 == 1 {
                    index -= 1;
                }
                self.buf_len -= index;
                samples_skipped += index;
                // Trim all the silent samples
                let buf_no_silence: Vec<T> = self.buf[index..]
                    .iter()
                    .map(|b| (*b).mul_amp(self.volume))
                    .collect();

                // Put the segment without silence at the beginning
                self.buf[..buf_no_silence.len()].copy_from_slice(&buf_no_silence);
                info!("Skipped {samples_skipped} silent samples");
                break;
            } else {
                samples_skipped += self.buf_len;
            }
        }
        Ok(())
    }

    fn adjust_buffer_size(&mut self, samples_length: usize) {
        if samples_length > self.buf.len() {
            self.buf.clear();
            self.buf.resize(samples_length, T::MID);
        }
        self.buf_len = samples_length;
    }

    fn handle_reset(&mut self) -> Result<(), DecoderError> {
        warn!("Decoder reset required");
        let (decoder, track) = create_decoder(&*self.reader)?;
        self.track_id = track.id;
        self.num_frames = track.num_frames;
        if let Some(time_base) = track.time_base {
            self.time_base = time_base;
        }
        self.decoder = decoder;
        self.initialize()?;

        Ok(())
    }

    fn process_output(&mut self, packet: &Packet) -> Result<(), DecoderError> {
        let decoded = match self.decoder.decode(packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(e)) => {
                warn!("Invalid data found during decoding {e:?}. Skipping packet.");
                // Decoder errors are recoverable, try the next packet
                return Err(DecoderError::Recoverable(e));
            }
            Err(e) => {
                return Err(DecoderError::DecodeError(e));
            }
        };

        if self.sample_rate.0 == 0 {
            let spec = decoded.spec();
            let sample_rate = SampleRate(spec.rate());
            self.sample_rate = sample_rate;
            let channels = spec.channels().count();
            self.input_channels = ChannelCount(channels as u16);

            info!("Input channels = {channels}");
            info!("Input sample rate = {}", sample_rate.0);

            if channels > 2 {
                return Err(DecoderError::UnsupportedFormat(
                    "Audio sources with more than 2 channels are not supported".to_owned(),
                ));
            }
        }

        let samples_len = decoded.samples_interleaved();
        self.sample_buf.resize(samples_len, T::MID);
        decoded.copy_to_slice_interleaved(&mut self.sample_buf);

        match (self.input_channels.0, self.output_channels.0) {
            (1, 2) => {
                self.adjust_buffer_size(samples_len * 2);

                let mut i = 0;
                for sample in self.sample_buf.iter() {
                    self.buf[i] = (*sample).mul_amp(self.volume);
                    self.buf[i + 1] = (*sample).mul_amp(self.volume);
                    i += 2;
                }
            }
            (2, 1) => {
                self.adjust_buffer_size(samples_len / 2);

                for (i, sample) in self.sample_buf.chunks_exact(2).enumerate() {
                    self.buf[i] = (sample[0] + sample[1])
                        .mul_amp(0.5.to_sample())
                        .mul_amp(self.volume);
                }
            }
            _ => {
                self.adjust_buffer_size(samples_len);

                for (i, sample) in self.sample_buf.iter().enumerate() {
                    self.buf[i] = (*sample).mul_amp(self.volume);
                }
            }
        }

        Ok(())
    }

    pub(crate) fn current(&self) -> &[T] {
        &self.buf[..self.buf_len]
    }

    pub(crate) fn next(&mut self) -> Result<Option<&[T]>, DecoderError> {
        if self.is_paused {
            self.buf.fill(T::MID);
        } else {
            loop {
                let packet = loop {
                    match self.reader.next_packet() {
                        Ok(Some(packet)) => {
                            if packet.track_id() == self.track_id {
                                if let Some(required_ts) = self.seek_required_ts {
                                    if packet.ts() < required_ts {
                                        continue;
                                    } else {
                                        self.seek_required_ts = None;
                                    }
                                }
                                break packet;
                            }
                        }
                        Ok(None) => {
                            return Ok(None);
                        }
                        Err(Error::ResetRequired) => {
                            self.handle_reset()?;
                            return Ok(Some(self.current()));
                        }
                        Err(e) => {
                            error!("Error reading next packet: {e:?}");
                            return Err(DecoderError::DecodeError(e));
                        }
                    };
                };
                self.timestamp = packet.ts();
                match self.process_output(&packet) {
                    Ok(()) => break,
                    Err(DecoderError::Recoverable(e)) => {
                        warn!("decoder error: {e}");
                        // Just read the next packet on a recoverable error
                    }
                    Err(e) => {
                        error!("Error processing output: {e:?}");
                        return Err(e);
                    }
                }
            }
        }
        Ok(Some(self.current()))
    }
}
