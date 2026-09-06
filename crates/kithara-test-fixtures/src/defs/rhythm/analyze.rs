use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::OnceLock,
};

use futures_lite::future::block_on;
use kithara_analysis::{
    AnalysisToken, AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder, BeatArtifact,
};
use kithara_audio::{
    AudioControl, AudioRead, AudioSession, ChunkOutcome, DecodeError, ReadOutcome, SeekOutcome,
};
use kithara_bufpool::testing::{Pools, TestPools, pools};
use kithara_decode::TrackMetadata;
use kithara_events::EventBus;
use kithara_platform::{thread, time::Duration};
use kithara_resampler::rubato::RubatoBackend;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};

struct Consts;

impl Consts {
    const BITS_PER_SAMPLE: u16 = 16;
    const BITS_PER_SAMPLE_OFFSET: usize = 34;
    const CHANNELS_OFFSET: usize = 22;
    const CHUNK_FRAMES: usize = 4_096;
    const DATA_BYTES_OFFSET: usize = 40;
    const HEADER_BYTES: usize = 44;
    const PCM_FORMAT: u16 = 1;
    const PCM_FORMAT_OFFSET: usize = 20;
    const SAMPLE_BYTES: usize = 2;
    const SAMPLE_RATE_OFFSET: usize = 24;
    const SAMPLE_SCALE: f32 = 32_768.0;
}

pub(super) fn beat(wav: &[u8]) -> BeatArtifact {
    let reader = WavReader::parse(wav).unwrap_or_else(|error| panic!("rhythm WAV: {error}"));
    let rate = reader.spec.sample_rate;
    let (mut results, _producer) = worker().analyze(
        Box::new(reader),
        AnalysisToken::from("rhythm-fixture"),
        rate,
        0,
    );
    let progress = block_on(async {
        while results.changed().await.is_ok() {}
        results.borrow().clone()
    })
    .unwrap_or_else(|| panic!("production rhythm analysis produced no result"));
    let analysis = progress.analysis();
    assert!(analysis.is_settled(), "rhythm analysis must cover the WAV");
    analysis
        .beat()
        .unwrap_or_else(|| panic!("production rhythm analysis produced no beat artifact"))
        .artifact()
        .clone()
}

fn worker() -> &'static AnalysisWorker {
    static WORKER: OnceLock<AnalysisWorker> = OnceLock::new();
    WORKER.get_or_init(|| {
        let builder = AnalyzerBuilder::<RubatoBackend, TestPools>::new(pools()).with_beat();
        let compute_tasks = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
        AnalysisWorker::new(
            AnalysisWorkerConfig::for_builder(builder)
                .max_compute_tasks(compute_tasks)
                .build(),
        )
    })
}

struct WavReader {
    bus: EventBus,
    cursor: usize,
    metadata: TrackMetadata,
    pools: Pools,
    samples: Vec<f32>,
    spec: AudioSpec,
}

impl WavReader {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.get(..4) != Some(b"RIFF")
            || bytes.get(8..12) != Some(b"WAVE")
            || bytes.get(36..40) != Some(b"data")
        {
            return Err("expected a canonical RIFF/WAVE PCM file".to_owned());
        }
        let format = u16_field(bytes, Consts::PCM_FORMAT_OFFSET)?;
        let bits = u16_field(bytes, Consts::BITS_PER_SAMPLE_OFFSET)?;
        let channels = u16_field(bytes, Consts::CHANNELS_OFFSET)?;
        let sample_rate = NonZeroU32::new(u32_field(bytes, Consts::SAMPLE_RATE_OFFSET)?)
            .ok_or_else(|| "sample rate is zero".to_owned())?;
        if format != Consts::PCM_FORMAT || bits != Consts::BITS_PER_SAMPLE || channels == 0 {
            return Err(format!(
                "unsupported WAV format={format}, bits={bits}, channels={channels}"
            ));
        }
        let data_bytes = usize::try_from(u32_field(bytes, Consts::DATA_BYTES_OFFSET)?)
            .map_err(|error| format!("WAV data size: {error}"))?;
        let payload = bytes
            .get(Consts::HEADER_BYTES..Consts::HEADER_BYTES.saturating_add(data_bytes))
            .ok_or_else(|| "WAV data chunk is truncated".to_owned())?;
        if !payload
            .len()
            .is_multiple_of(usize::from(channels) * Consts::SAMPLE_BYTES)
        {
            return Err("WAV data does not contain complete frames".to_owned());
        }
        let samples = payload
            .chunks_exact(Consts::SAMPLE_BYTES)
            .map(|bytes| f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / Consts::SAMPLE_SCALE)
            .collect();
        Ok(Self {
            bus: EventBus::default(),
            cursor: 0,
            metadata: TrackMetadata::default(),
            pools: pools(),
            samples,
            spec: AudioSpec::new(channels, sample_rate),
        })
    }

    fn total_frames(&self) -> usize {
        self.samples.len() / usize::from(self.spec.channels)
    }

    fn position_at(&self, frame: usize) -> Duration {
        self.spec
            .duration_for(u64::try_from(frame).expect("invariant: fixture frame fits u64"))
            .expect("invariant: fixture duration fits platform duration")
    }
}

impl AudioSession for WavReader {
    fn duration(&self) -> Option<Duration> {
        Some(self.position_at(self.total_frames()))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl AudioRead for WavReader {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        if self.cursor >= self.total_frames() {
            return Ok(ChunkOutcome::Eof {
                position: self.position_at(self.cursor),
            });
        }
        let start = self.cursor;
        let end = start
            .saturating_add(Consts::CHUNK_FRAMES)
            .min(self.total_frames());
        let channels = usize::from(self.spec.channels);
        let sample_start = start * channels;
        let sample_end = end * channels;
        let mut samples = self.pools.get_with_len::<f32>(sample_end - sample_start)?;
        samples.copy_from_slice(&self.samples[sample_start..sample_end]);
        self.cursor = end;
        Ok(ChunkOutcome::Chunk(AudioChunk::new(
            AudioChunkInfo {
                end_timestamp: self.position_at(end),
                frame_offset: u64::try_from(start).map_err(|_| DecodeError::InvalidData {
                    detail: "fixture frame does not fit u64",
                })?,
                frames: u32::try_from(end - start).map_err(|_| DecodeError::InvalidData {
                    detail: "fixture chunk does not fit u32",
                })?,
                spec: self.spec,
                timestamp: self.position_at(start),
                ..AudioChunkInfo::default()
            },
            samples,
        )))
    }

    fn position(&self) -> Duration {
        self.position_at(self.cursor)
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Err(DecodeError::InvalidData {
            detail: "rhythm analysis reads whole chunks",
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Err(DecodeError::InvalidData {
            detail: "rhythm analysis reads whole chunks",
        })
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for WavReader {
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, DecodeError> {
        let frame = usize::try_from(self.spec.frame_at(target)?).map_err(|_| {
            DecodeError::SeekOutOfRange {
                detail: "fixture seek does not fit usize",
            }
        })?;
        let total = self.total_frames();
        self.cursor = frame.min(total);
        if frame >= total {
            return Ok(SeekOutcome::PastEof {
                target,
                duration: self.position_at(total),
            });
        }
        Ok(SeekOutcome::Landed {
            target,
            landed_at: self.position_at(self.cursor),
        })
    }
}

fn u16_field(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| format!("WAV u16 field at {offset} is truncated"))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_field(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| format!("WAV u32 field at {offset} is truncated"))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
