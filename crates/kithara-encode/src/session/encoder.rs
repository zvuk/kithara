use std::mem::size_of;

use crate::{EncodeConfig, EncodeError, EncodeResult, EncodedAccessUnit};

/// Continuous encoder from interleaved `f32` PCM to access units.
#[derive(derive_more::Debug)]
pub struct EncoderSession {
    config: EncodeConfig,
    #[debug("{:?}", self.pending_samples.len())]
    pending_samples: Vec<f32>,
    next_frame: u64,
    packet_samples: usize,
}

impl EncoderSession {
    /// Open the encoder selected by `config`.
    ///
    /// This session currently provides portable PCM/WAV float32.
    ///
    /// # Errors
    ///
    /// Returns a typed unsupported-codec error or rejects invalid audio and
    /// packet sizes.
    pub fn new(config: &EncodeConfig) -> EncodeResult<Self> {
        config.validate_pcm_wav()?;
        let channels = usize::from(config.channels);
        u32::try_from(config.packet_frames).map_err(|_| {
            EncodeError::InvalidInput("packet_frames does not fit into u32".to_owned())
        })?;
        let packet_samples = config
            .packet_frames
            .checked_mul(channels)
            .and_then(|samples| samples.checked_mul(size_of::<f32>()).map(|_| samples))
            .ok_or_else(|| EncodeError::InvalidInput("PCM packet size overflow".to_owned()))?;

        Ok(Self {
            config: config.clone(),
            next_frame: 0,
            packet_samples,
            pending_samples: Vec::with_capacity(packet_samples),
        })
    }

    /// Configuration retained by this encoding session.
    #[must_use]
    pub const fn config(&self) -> &EncodeConfig {
        &self.config
    }

    /// Finish the stream and return its final partial access unit.
    ///
    /// # Errors
    ///
    /// Returns invalid input if the final frame count cannot be represented by
    /// the access-unit duration.
    pub fn finish(self) -> EncodeResult<Vec<EncodedAccessUnit>> {
        if self.pending_samples.is_empty() {
            return Ok(Vec::new());
        }
        let frames = self.pending_samples.len() / usize::from(self.config.channels);
        let duration = u32::try_from(frames).map_err(|_| {
            EncodeError::InvalidInput("final PCM packet duration does not fit into u32".to_owned())
        })?;
        Ok(vec![Self::unit(
            &self.pending_samples,
            self.next_frame,
            duration,
        )])
    }

    /// Encode complete interleaved frames and return completed access units.
    ///
    /// # Errors
    ///
    /// Returns invalid input when `samples` ends in a partial frame.
    pub fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        let channels = usize::from(self.config.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(EncodeError::InvalidInput(format!(
                "interleaved sample count {} is not a multiple of {} channels",
                samples.len(),
                channels
            )));
        }
        let packet_frames = u32::try_from(self.config.packet_frames).map_err(|_| {
            EncodeError::InvalidInput("packet_frames does not fit into u32".to_owned())
        })?;
        let total_samples = self
            .pending_samples
            .len()
            .checked_add(samples.len())
            .ok_or_else(|| EncodeError::InvalidInput("PCM input size overflow".to_owned()))?;
        let ready_packets = total_samples / self.packet_samples;
        let ready_frames = u64::try_from(ready_packets)
            .ok()
            .and_then(|packets| packets.checked_mul(u64::from(packet_frames)))
            .ok_or_else(|| EncodeError::InvalidInput("PCM frame count overflow".to_owned()))?;
        let end_frame = self
            .next_frame
            .checked_add(ready_frames)
            .ok_or_else(|| EncodeError::InvalidInput("PCM stream timestamp overflow".to_owned()))?;
        let mut units: Vec<EncodedAccessUnit> = Vec::with_capacity(ready_packets);
        let mut packet_index = 0_usize;
        let mut emit = |packet: &[f32]| -> EncodeResult<()> {
            let offset = u64::try_from(packet_index)
                .ok()
                .and_then(|index| index.checked_mul(u64::from(packet_frames)))
                .ok_or_else(|| {
                    EncodeError::InvalidInput("PCM packet offset overflow".to_owned())
                })?;
            let pts = self.next_frame.checked_add(offset).ok_or_else(|| {
                EncodeError::InvalidInput("PCM packet timestamp overflow".to_owned())
            })?;
            units.push(Self::unit(packet, pts, packet_frames));
            packet_index += 1;
            Ok(())
        };

        let mut remaining = samples;
        if !self.pending_samples.is_empty() {
            let needed = self.packet_samples - self.pending_samples.len();
            let take = needed.min(remaining.len());
            self.pending_samples.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.pending_samples.len() == self.packet_samples {
                emit(&self.pending_samples)?;
                self.pending_samples.clear();
            }
        }

        let ready_samples = remaining.len() / self.packet_samples * self.packet_samples;
        for packet in remaining[..ready_samples].chunks_exact(self.packet_samples) {
            emit(packet)?;
        }
        self.pending_samples
            .extend_from_slice(&remaining[ready_samples..]);
        self.next_frame = end_frame;
        Ok(units)
    }

    fn unit(samples: &[f32], pts: u64, duration: u32) -> EncodedAccessUnit {
        let bytes = Vec::from_iter(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        EncodedAccessUnit {
            bytes,
            is_sync: true,
            duration,
            dts: pts,
            pts,
        }
    }
}
