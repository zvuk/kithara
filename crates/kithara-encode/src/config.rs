use bon::Builder;
use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{EncodeError, EncodeResult};

/// Audio format and packetization for one continuous encoding session.
#[kithara_config::config(builder = false)]
#[derive(Clone, Debug, Builder)]
#[non_exhaustive]
pub struct EncodeConfig {
    /// Codec written by the encoder session.
    #[builder(default = AudioCodec::Pcm)]
    #[config(value)]
    pub codec: AudioCodec,
    /// Container written by the container session.
    #[builder(default = ContainerFormat::Wav)]
    #[config(value)]
    pub container: ContainerFormat,
    /// Number of interleaved source channels.
    #[config(value)]
    pub channels: u16,
    /// Source sample rate in Hz.
    #[config(value)]
    pub sample_rate: u32,
    /// PCM frames carried by each portable access unit.
    #[builder(default = 1_024)]
    #[config(value)]
    pub packet_frames: usize,
}

impl EncodeConfig {
    pub(crate) fn validate_audio(&self) -> EncodeResult<()> {
        if self.sample_rate == 0 {
            return Err(EncodeError::InvalidInput(
                "sample_rate must be > 0, got 0".to_owned(),
            ));
        }
        if self.channels == 0 {
            return Err(EncodeError::InvalidInput(
                "channels must be > 0, got 0".to_owned(),
            ));
        }
        if self.packet_frames == 0 {
            return Err(EncodeError::InvalidInput(
                "packet_frames must be > 0, got 0".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_pcm_wav(&self) -> EncodeResult<()> {
        self.validate_audio()?;
        if self.codec != AudioCodec::Pcm {
            return Err(EncodeError::UnsupportedCodec(self.codec));
        }
        if self.container != ContainerFormat::Wav {
            return Err(EncodeError::UnsupportedContainer(self.container));
        }
        Ok(())
    }
}
