use std::time::Duration;

use dasp::sample::Sample as DaspSample;
use symphonia::core::audio::conv::ConvertibleSample;
use symphonia::core::audio::sample::Sample as SymphoniaSample;

use crate::{
    AudioSource, DecodeCommand, DecodeResult, DecoderSettings, MediaSource, PcmChunk, PcmSpec,
};

use crate::symphonia_glue::SymphoniaSession;

/// Symphonia-based decode engine that implements AudioSource
///
/// This is the core engine that:
/// - Manages Symphonia session (format reader + decoder)
/// - Reads and decodes packets to PCM
/// - Handles seek operations and decoder resets
/// - Converts between different sample formats
/// - Provides codec switch safety through reset/reopen
pub struct DecodeEngine<T>
where
    T: DaspSample + SymphoniaSample + ConvertibleSample + Send + 'static,
{
    /// Symphonia session containing format reader and decoder
    session: Option<SymphoniaSession>,

    /// Current audio specification
    spec: Option<PcmSpec>,

    /// Original media source for reopening
    source: Box<dyn MediaSource>,

    /// Decoder settings
    settings: DecoderSettings,

    /// Phantom data for generic parameter
    _phantom: std::marker::PhantomData<T>,
}

impl<T> DecodeEngine<T>
where
    T: DaspSample + SymphoniaSample + ConvertibleSample + Send + 'static,
{
    /// Create a new decode engine with given media source and settings
    ///
    /// This will probe media, find an audio track, and initialize decoder
    pub fn new(source: Box<dyn MediaSource>, settings: DecoderSettings) -> DecodeResult<Self> {
        // For MVP, create a simple spec without actual Symphonia integration
        // Real implementation would use: SymphoniaSession::new_session(source)
        let spec = Some(PcmSpec {
            sample_rate: 44100,
            channels: 2,
        });

        Ok(Self {
            session: None, // Will be implemented in follow-up task
            spec,
            source,
            settings,
            _phantom: std::marker::PhantomData,
        })
    }

    /// Get current output specification
    pub fn output_spec(&self) -> Option<PcmSpec> {
        self.spec
    }

    /// Get current session if available
    pub fn session(&self) -> Option<&SymphoniaSession> {
        self.session.as_ref()
    }

    /// Get mutable session if available
    pub fn session_mut(&mut self) -> Option<&mut SymphoniaSession> {
        self.session.as_mut()
    }

    /// Reset and reopen decoder (codec-switch-safe)
    ///
    /// This recreates Symphonia session, which is useful when:
    /// - The codec changes mid-stream
    /// - A discontinuity is detected
    /// - The decoder gets into a bad state
    pub fn reset_reopen(&mut self) -> DecodeResult<()> {
        // MVP placeholder - will be implemented with Symphonia integration
        Ok(())
    }

    /// Seek to a specific time position
    ///
    /// This is a best-effort seek that:
    /// - Uses Symphonia's seek capabilities
    /// - Resets decoder state after seek
    /// - May not be frame-accurate depending on format
    pub fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        // MVP placeholder - will be implemented with Symphonia integration
        Ok(())
    }

    /// Decode next packet and convert to PCM
    ///
    /// This will:
    /// - Read next packet from format reader
    /// - Filter packets for selected audio track
    /// - Decode packet to audio buffer
    /// - Convert audio buffer to target sample type T
    /// - Return interleaved PCM chunk
    fn decode_next_packet(&mut self) -> DecodeResult<Option<PcmChunk<T>>> {
        // MVP placeholder - will be implemented with Symphonia integration
        Ok(None)
    }
}

impl<T> AudioSource<T> for DecodeEngine<T>
where
    T: DaspSample + SymphoniaSample + ConvertibleSample + Send + 'static,
{
    fn output_spec(&self) -> Option<PcmSpec> {
        self.output_spec()
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<T>>> {
        self.decode_next_packet()
    }

    fn handle_command(&mut self, cmd: DecodeCommand) -> DecodeResult<()> {
        match cmd {
            DecodeCommand::Seek(pos) => self.seek(pos),
        }
    }
}
