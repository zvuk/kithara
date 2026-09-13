use std::collections::BTreeMap;

use bon::Builder;
use kithara_stream::AudioCodec;
use serde::{Deserialize, Serialize};

use crate::{
    fmp4::GaplessEncoding,
    signal::{SweepMode, Wave},
};

/// Complete immutable input identity of one build-time encoded HLS variant.
#[derive(Builder, Debug, Clone)]
#[non_exhaustive]
pub struct VariantInput {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u16,
    pub content_frames: usize,
    pub packets_per_segment: usize,
    pub signal: Wave,
    pub start_frame: usize,
    pub bit_rate: u64,
    pub timescale: u32,
    pub encoder_delay: u32,
    pub trailing_delay: u32,
    pub gapless_encoding: GaplessEncoding,
}

impl VariantInput {
    /// Exact identity shared by the producer and the fixture reader.
    ///
    /// # Panics
    ///
    /// Panics when the codec is not AAC LC, HE-AAC, HE-AAC v2, or FLAC.
    #[must_use]
    pub fn key(&self) -> String {
        let codec = match self.codec {
            AudioCodec::AacLc => "aac-lc",
            AudioCodec::AacHe => "aac-he",
            AudioCodec::AacHeV2 => "aac-he-v2",
            AudioCodec::Flac => "flac",
            _ => panic!("unsupported HLS fixture codec"),
        };
        let signal = match self.signal {
            Wave::Sawtooth => "saw".to_owned(),
            Wave::SawtoothDescending => "descending".to_owned(),
            Wave::SawtoothShifted => "shifted".to_owned(),
            Wave::Silence => "silence".to_owned(),
            Wave::Sine { hz, peak } => format!("sine/{}/{peak}", hz.to_bits()),
            Wave::Sweep {
                start_hz,
                end_hz,
                total_frames,
                mode,
            } => {
                let mode = match mode {
                    SweepMode::Linear => "linear",
                    SweepMode::Log => "log",
                };
                format!(
                    "sweep/{}/{}/{total_frames}/{mode}",
                    start_hz.to_bits(),
                    end_hz.to_bits()
                )
            }
            Wave::Clicks {
                hz,
                period_frames,
                burst_frames,
            } => format!("clicks/{}/{period_frames}/{burst_frames}", hz.to_bits()),
        };
        let gapless = match self.gapless_encoding {
            GaplessEncoding::None => "none",
            GaplessEncoding::Edts => "edts",
            GaplessEncoding::ItunSmpb => "itun-smpb",
            GaplessEncoding::Both => "both",
        };
        format!(
            "v1/{codec}/{}/{}/{}/{}/{}/{}/{}/{}/{}/{gapless}/{signal}",
            self.sample_rate,
            self.channels,
            self.content_frames,
            self.packets_per_segment,
            self.start_frame,
            self.bit_rate,
            self.timescale,
            self.encoder_delay,
            self.trailing_delay
        )
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct VariantArtifact {
    pub(crate) init: String,
    pub(crate) media: Vec<String>,
    pub(crate) durations: Vec<f64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct VariantCatalog {
    pub(crate) frame_samples: BTreeMap<String, usize>,
    pub(crate) variants: BTreeMap<String, VariantArtifact>,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{AudioCodec, GaplessEncoding, VariantInput, Wave};

    #[kithara::fixture]
    fn variant() -> VariantInput {
        VariantInput::builder()
            .codec(AudioCodec::AacLc)
            .sample_rate(44_100)
            .channels(2)
            .content_frames(8 * 22_528)
            .packets_per_segment(22)
            .signal(Wave::Sawtooth)
            .start_frame(0)
            .bit_rate(128_000)
            .timescale(44_100)
            .encoder_delay(0)
            .trailing_delay(0)
            .gapless_encoding(GaplessEncoding::Edts)
            .build()
    }

    #[kithara::test]
    fn variant_key_pins_the_prepared_input_identity(mut variant: VariantInput) {
        let original = variant.key();
        assert_eq!(
            original,
            "v1/aac-lc/44100/2/180224/22/0/128000/44100/0/0/edts/saw"
        );
        variant.start_frame = 1;
        assert_ne!(variant.key(), original);
        variant.start_frame = 0;
        variant.trailing_delay = 1;
        assert_ne!(variant.key(), original);
        variant.trailing_delay = 0;
        variant.signal = Wave::sine(440.0);
        let tone = variant.key();
        variant.signal = Wave::sine(f64::from_bits(440.0f64.to_bits() + 1));
        assert_ne!(variant.key(), tone);
    }
}
