//! A whole track in memory that answers every seek exactly: the reader the
//! pass-level tests drive, over silence or over real PCM.

use kithara_audio::{
    AudioControl, AudioRead, AudioReadError, AudioSession, ChunkOutcome, ReadOutcome, SeekOutcome,
};
use kithara_decode::TrackMetadata;
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use num_traits::cast::ToPrimitive;

use crate::test_pools::{Pools, sample_buffer};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct Track {
    spec: AudioSpec,
    bus: EventBus,
    pools: Pools,
    metadata: TrackMetadata,
    pcm: Vec<f32>,
    at: u64,
    chunk_frames: u64,
    claimed: u64,
    first: u64,
    #[field(get, vis = "pub(super)")]
    frames: u64,
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

    pub(super) fn claiming(
        prepared: &[f32],
        pools: Pools,
        spec: AudioSpec,
        chunk_frames: u64,
        seconds: f64,
        claimed: f64,
    ) -> Self {
        let rate = f64::from(spec.sample_rate.get());
        let mut track = Self::silence(prepared, pools, spec, chunk_frames, seconds);
        track.claimed = (claimed * rate).round().to_u64().unwrap_or(0);
        track
    }

    fn duration_for(&self, frames: u64) -> Duration {
        self.spec.duration_for(frames).expect("representable")
    }

    pub(super) fn priming(
        prepared: &[f32],
        pools: Pools,
        spec: AudioSpec,
        chunk_frames: u64,
        seconds: f64,
        priming: u64,
    ) -> Self {
        let mut track = Self::silence(prepared, pools, spec, chunk_frames, seconds);
        track.first = priming;
        track.at = priming;
        track
    }

    pub(super) fn silence(
        prepared: &[f32],
        pools: Pools,
        spec: AudioSpec,
        chunk_frames: u64,
        seconds: f64,
    ) -> Self {
        let rate = f64::from(spec.sample_rate.get());
        let frames = (seconds * rate).round().to_usize().unwrap_or(0);
        let pcm = prepared[..frames * usize::from(spec.channels)].to_vec();
        Self::new(pools, spec, chunk_frames, pcm)
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
    fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
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
        Ok(ChunkOutcome::Chunk(Box::new(AudioChunk::new(
            AudioChunkInfo {
                spec: self.spec,
                frames: u32::try_from(frames).unwrap_or(0),
                frame_offset: at,
                ..Default::default()
            },
            sample_buffer(&self.pools, &self.pcm[from..from + len]),
        ))))
    }

    fn position(&self) -> Duration {
        self.duration_for(self.at)
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, AudioReadError> {
        unreachable!("analysis pulls chunks")
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError> {
        unreachable!("analysis pulls chunks")
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for Track {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
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
