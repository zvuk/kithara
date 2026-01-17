use std::time::Duration;

use crate::{AudioSource, DecodeCommand, DecodeEngine, DecodeResult, DecoderSettings, MediaSource};

/// Low-level decoder state machine around Symphonia (MVP placeholder)
pub struct Decoder<T>
where
    T: Send + 'static,
{
    engine: DecodeEngine<T>,
    track_id: u32,
    current_pos_secs: u64,
}

impl<T> Decoder<T>
where
    T: Send + 'static,
{
    /// Create a new decoder with the given media source and settings
    pub fn new(source: Box<dyn MediaSource>, settings: DecoderSettings) -> DecodeResult<Self> {
        let engine = DecodeEngine::new(source, settings)?;
        let track_id = 0; // TODO: Extract from engine during proper Symphonia integration

        Ok(Self {
            engine,
            track_id,
            current_pos_secs: 0,
        })
    }

    /// Get the current audio specification if known
    pub fn output_spec(&self) -> Option<crate::PcmSpec> {
        self.engine.output_spec()
    }

    /// Get the current track ID
    pub fn track_id(&self) -> u32 {
        self.track_id
    }

    /// Get the current position in seconds
    pub fn current_position_secs(&self) -> u64 {
        self.current_pos_secs
    }

    /// Seek to a specific time position (best-effort)
    pub fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.engine.handle_command(DecodeCommand::Seek(pos))?;
        self.current_pos_secs = pos.as_secs();
        Ok(())
    }

    /// Reset and reopen decoder (codec-switch-safe)
    pub fn reset_reopen(&mut self) -> DecodeResult<()> {
        self.engine.reset_reopen()
    }

    /// Decode next chunk of PCM data
    ///
    /// Returns None when the stream has ended naturally
    pub fn decode_next(&mut self) -> DecodeResult<Option<crate::PcmChunk<T>>> {
        self.engine.next_chunk()
    }

    /// Get mutable reference to the underlying engine for advanced operations
    pub fn engine_mut(&mut self) -> &mut DecodeEngine<T> {
        &mut self.engine
    }

    /// Get reference to the underlying engine for read-only operations
    pub fn engine(&self) -> &DecodeEngine<T> {
        &self.engine
    }
}

impl<T> AudioSource<T> for Decoder<T>
where
    T: Send + 'static,
{
    fn output_spec(&self) -> Option<crate::PcmSpec> {
        self.output_spec()
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<crate::PcmChunk<T>>> {
        self.decode_next()
    }

    fn handle_command(&mut self, cmd: DecodeCommand) -> DecodeResult<()> {
        match cmd {
            DecodeCommand::Seek(pos) => self.seek(pos),
        }
    }
}
