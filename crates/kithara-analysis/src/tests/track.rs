//! A whole track in memory that answers every seek exactly: the reader the
//! pass-level tests drive, over silence or over real PCM.

use kithara_audio::{
    AudioControl, AudioRead, AudioSession, ChunkOutcome, DecodeError, ReadOutcome, SeekOutcome,
};
use kithara_decode::TrackMetadata;
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use num_traits::cast::ToPrimitive;

use crate::test_pools::{Pools, sample_buffer};

pub(super) struct Track {
    pools: Pools,
    spec: AudioSpec,
    chunk_frames: u64,
    pcm: Vec<f32>,
    frames: u64,
    /// What the source says it holds. An mp3 claims its frame count, padding
    /// included, and delivers less.
    claimed: u64,
    /// The first frame the source can deliver: an encoder's priming and the
    /// decoder's delay leave nothing to read in front of it.
    first: u64,
    at: u64,
    bus: EventBus,
    metadata: TrackMetadata,
}

impl Track {
    pub(super) fn new(pools: Pools, spec: AudioSpec, chunk_frames: u64, pcm: Vec<f32>) -> Self {
        let frames = (pcm.len() / usize::from(spec.channels))
            .to_u64()
            .unwrap_or(0);
        Self {
            pools,
            spec,
            chunk_frames,
            pcm,
            frames,
            claimed: frames,
            first: 0,
            at: 0,
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
        }
    }

    pub(super) fn silence(pools: Pools, spec: AudioSpec, chunk_frames: u64, seconds: f64) -> Self {
        let rate = f64::from(spec.sample_rate.get());
        let frames = (seconds * rate).round().to_usize().unwrap_or(0);
        let pcm = vec![0.0; frames * usize::from(spec.channels)];
        Self::new(pools, spec, chunk_frames, pcm)
    }

    /// Silence that claims `claimed` seconds and delivers `seconds` of it.
    pub(super) fn claiming(
        pools: Pools,
        spec: AudioSpec,
        chunk_frames: u64,
        seconds: f64,
        claimed: f64,
    ) -> Self {
        let rate = f64::from(spec.sample_rate.get());
        let mut track = Self::silence(pools, spec, chunk_frames, seconds);
        track.claimed = (claimed * rate).round().to_u64().unwrap_or(0);
        track
    }

    /// Silence whose first `priming` frames cannot be delivered.
    pub(super) fn priming(
        pools: Pools,
        spec: AudioSpec,
        chunk_frames: u64,
        seconds: f64,
        priming: u64,
    ) -> Self {
        let mut track = Self::silence(pools, spec, chunk_frames, seconds);
        track.first = priming;
        track.at = priming;
        track
    }

    pub(super) fn frames(&self) -> u64 {
        self.frames
    }

    fn duration_for(&self, frames: u64) -> Duration {
        self.spec.duration_for(frames).expect("representable")
    }
}

impl AudioSession for Track {
    fn duration(&self) -> Option<Duration> {
        Some(self.duration_for(self.claimed))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl AudioRead for Track {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        if self.at >= self.frames {
            return Ok(ChunkOutcome::Eof {
                position: self.position(),
            });
        }
        let at = self.at;
        let frames = self.chunk_frames.min(self.frames - at);
        self.at = at + frames;
        let channels = u64::from(self.spec.channels);
        let from = (at * channels).to_usize().unwrap_or(0);
        let len = (frames * channels).to_usize().unwrap_or(0);
        Ok(ChunkOutcome::Chunk(AudioChunk::new(
            AudioChunkInfo {
                spec: self.spec,
                frames: u32::try_from(frames).unwrap_or(0),
                frame_offset: at,
                ..Default::default()
            },
            sample_buffer(&self.pools, &self.pcm[from..from + len]),
        )))
    }

    fn position(&self) -> Duration {
        self.duration_for(self.at)
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        unreachable!("analysis pulls chunks")
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        unreachable!("analysis pulls chunks")
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for Track {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let target = self.spec.frame_at(position).unwrap_or(0);
        if target >= self.frames {
            return Ok(SeekOutcome::PastEof {
                target: position,
                duration: self.duration_for(self.frames),
            });
        }
        self.at = target.max(self.first);
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: self.duration_for(self.at),
        })
    }
}
