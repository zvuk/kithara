use std::mem::size_of;

use kithara_stream::ContainerFormat;

use crate::{EncodeConfig, EncodeError, EncodeResult, EncodedAccessUnit};

/// One container write at an absolute byte offset.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContainerWrite {
    /// Bytes to write at `offset`.
    pub bytes: Vec<u8>,
    /// Absolute byte offset in the output resource.
    pub offset: u64,
}

/// Final writes and length for a completed container.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContainerFinish {
    /// Final writes produced after the payload length is known.
    pub writes: Vec<ContainerWrite>,
    /// Complete container length in bytes.
    pub final_len: u64,
}

/// Continuous container writer for encoded access units.
#[derive(Debug)]
pub struct ContainerSession {
    config: EncodeConfig,
    block_align: u16,
    byte_rate: u32,
    data_bytes: u64,
    next_frame: u64,
}

impl ContainerSession {
    const BITS_PER_SAMPLE: u16 = 32;
    const FORMAT_CHUNK_BYTES: u32 = 16;
    const HEADER_BYTES: usize = 44;
    const MAX_DATA_BYTES: u64 = Self::RIFF_MAX_BYTES - Self::RIFF_OVERHEAD_BYTES;
    const PCM_FLOAT_FORMAT: u16 = 3;
    const RIFF_MAX_BYTES: u64 = 4_294_967_295;
    const RIFF_OVERHEAD_BYTES: u64 = 36;

    /// Open the container selected by `config`.
    ///
    /// # Errors
    ///
    /// Returns typed unsupported-profile errors or rejects arithmetic that
    /// cannot be represented by the WAV header.
    pub fn new(config: &EncodeConfig) -> EncodeResult<Self> {
        config.validate_pcm_wav()?;
        let bytes_per_sample = u16::try_from(size_of::<f32>())
            .map_err(|_| EncodeError::InvalidInput("f32 byte size overflow".to_owned()))?;
        let block_align = config
            .channels
            .checked_mul(bytes_per_sample)
            .ok_or_else(|| EncodeError::InvalidInput("WAV block alignment overflow".to_owned()))?;
        let byte_rate = config
            .sample_rate
            .checked_mul(u32::from(block_align))
            .ok_or_else(|| EncodeError::InvalidInput("WAV byte rate overflow".to_owned()))?;

        Ok(Self {
            config: config.clone(),
            block_align,
            byte_rate,
            data_bytes: 0,
            next_frame: 0,
        })
    }

    /// Configuration retained by this container session.
    #[must_use]
    pub const fn config(&self) -> &EncodeConfig {
        &self.config
    }

    /// Finish the WAV header and return the complete byte length.
    ///
    /// # Errors
    ///
    /// Returns an error when the final RIFF fields cannot be represented.
    pub fn finish(self) -> EncodeResult<ContainerFinish> {
        let data_bytes = u32::try_from(self.data_bytes).map_err(|_| {
            EncodeError::InvalidInput("WAV data size does not fit into u32".to_owned())
        })?;
        let riff_bytes = u32::try_from(Self::RIFF_OVERHEAD_BYTES + self.data_bytes)
            .map_err(|_| EncodeError::InvalidInput("WAV RIFF size overflow".to_owned()))?;
        let mut header = [0_u8; Self::HEADER_BYTES];
        let mut offset = 0;
        Self::write_header(&mut header, &mut offset, b"RIFF");
        Self::write_header(&mut header, &mut offset, &riff_bytes.to_le_bytes());
        Self::write_header(&mut header, &mut offset, b"WAVEfmt ");
        Self::write_header(
            &mut header,
            &mut offset,
            &Self::FORMAT_CHUNK_BYTES.to_le_bytes(),
        );
        Self::write_header(
            &mut header,
            &mut offset,
            &Self::PCM_FLOAT_FORMAT.to_le_bytes(),
        );
        Self::write_header(
            &mut header,
            &mut offset,
            &self.config.channels.to_le_bytes(),
        );
        Self::write_header(
            &mut header,
            &mut offset,
            &self.config.sample_rate.to_le_bytes(),
        );
        Self::write_header(&mut header, &mut offset, &self.byte_rate.to_le_bytes());
        Self::write_header(&mut header, &mut offset, &self.block_align.to_le_bytes());
        Self::write_header(
            &mut header,
            &mut offset,
            &Self::BITS_PER_SAMPLE.to_le_bytes(),
        );
        Self::write_header(&mut header, &mut offset, b"data");
        Self::write_header(&mut header, &mut offset, &data_bytes.to_le_bytes());
        debug_assert_eq!(offset, header.len());

        let header_bytes = u64::try_from(Self::HEADER_BYTES)
            .map_err(|_| EncodeError::InvalidInput("WAV header size overflow".to_owned()))?;

        Ok(ContainerFinish {
            writes: vec![ContainerWrite {
                offset: 0,
                bytes: header.into(),
            }],
            final_len: header_bytes + self.data_bytes,
        })
    }

    /// Maximum complete PCM frames this container can represent.
    #[must_use]
    pub fn max_frames(&self) -> u64 {
        Self::MAX_DATA_BYTES / u64::from(self.block_align)
    }

    /// Place one access unit in the container.
    ///
    /// # Errors
    ///
    /// Rejects discontinuous timestamps, malformed PCM access units, and data
    /// that cannot fit in a RIFF/WAVE container.
    pub fn push(&mut self, unit: EncodedAccessUnit) -> EncodeResult<Vec<ContainerWrite>> {
        if unit.pts != self.next_frame || unit.dts != unit.pts {
            return Err(EncodeError::InvalidInput(format!(
                "PCM access unit starts at {}, expected {}",
                unit.pts, self.next_frame
            )));
        }
        let frame_bytes = usize::from(self.block_align);
        if !unit.bytes.len().is_multiple_of(frame_bytes) {
            return Err(EncodeError::InvalidInput(format!(
                "PCM access unit has {} bytes for a {frame_bytes}-byte frame",
                unit.bytes.len()
            )));
        }
        let frames = unit.bytes.len() / frame_bytes;
        if usize::try_from(unit.duration).ok() != Some(frames) {
            return Err(EncodeError::InvalidInput(format!(
                "PCM access unit duration {} does not describe {frames} frames",
                unit.duration
            )));
        }

        let bytes = u64::try_from(unit.bytes.len())
            .map_err(|_| EncodeError::InvalidInput("PCM byte count overflow".to_owned()))?;
        let next_frame = self
            .next_frame
            .checked_add(u64::from(unit.duration))
            .ok_or_else(|| EncodeError::InvalidInput("PCM stream timestamp overflow".to_owned()))?;
        self.validate_frame_count(next_frame)?;
        let next_data_bytes = self
            .data_bytes
            .checked_add(bytes)
            .ok_or_else(|| EncodeError::InvalidInput("PCM byte count overflow".to_owned()))?;

        let header_bytes = u64::try_from(Self::HEADER_BYTES)
            .map_err(|_| EncodeError::InvalidInput("WAV header size overflow".to_owned()))?;
        let offset = header_bytes + self.data_bytes;
        self.data_bytes = next_data_bytes;
        self.next_frame = next_frame;
        Ok(vec![ContainerWrite {
            offset,
            bytes: unit.bytes,
        }])
    }

    /// Check a known final frame count before encoding starts.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::ContainerLimitExceeded`] when the corresponding
    /// PCM payload cannot fit in the WAV container.
    pub fn validate_frame_count(&self, frames: u64) -> EncodeResult<()> {
        if frames <= self.max_frames() {
            return Ok(());
        }
        Err(EncodeError::ContainerLimitExceeded {
            container: ContainerFormat::Wav,
            attempted_bytes: frames.saturating_mul(u64::from(self.block_align)),
            max_bytes: Self::MAX_DATA_BYTES,
        })
    }

    fn write_header(header: &mut [u8], offset: &mut usize, bytes: &[u8]) {
        let end = *offset + bytes.len();
        header[*offset..end].copy_from_slice(bytes);
        *offset = end;
    }
}
