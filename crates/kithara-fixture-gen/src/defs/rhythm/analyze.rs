use std::{
    io::Cursor,
    num::NonZeroUsize,
    sync::{Mutex, OnceLock, PoisonError},
};

use futures_lite::future::block_on;
use kithara_analysis::{
    AnalysisDemand, AnalysisToken, AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder,
    BeatArtifact,
};
use kithara_audio::{
    AudioControl, AudioRead, AudioReadError, AudioSession, ChunkOutcome, DecodeError, ReadOutcome,
    SeekOutcome,
};
use kithara_decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{thread, time::Duration};
use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::bufpool::{Pools, TestPools, pools};

use super::assets::analysis_file;

mod consts {
    pub(super) const CHUNK_FRAMES: usize = 4_096;
}

/// The analysis file a whole audio track carries: the production beat pass
/// over the track decoded from `inputs[0]`, read against the rate the track
/// was decoded at.
///
/// # Panics
///
/// When the track does not decode: a fixture that is no whole track is
/// declared a `fragment` and never reaches here.
pub(in crate::defs) fn analysed(inputs: &[&[u8]], hint: &str) -> Vec<u8> {
    let track = inputs
        .first()
        .expect("invariant: an analysis depends on the track it analyses");
    let reader = PcmReader::decode(track, hint)
        .unwrap_or_else(|error| panic!("analysed {hint} track: {error}"));
    let rate = reader.spec.sample_rate;
    let (artifact, frames) = analyze(reader);
    analysis_file(artifact, frames, rate)
}

/// One analysis at a time: a session keeps the whole mono track and its
/// detection buffers in the worker's shared pool region, and `build.rs`
/// materialises analyses in parallel batches, so concurrent sessions exceeded
/// the region budget and settled without a beat grid.
///
/// A track the pass hears no beat in carries no beat artifact.
fn analyze(reader: PcmReader) -> (Option<BeatArtifact>, u64) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let rate = reader.spec.sample_rate;
    let (mut results, _producer) = worker()
        .analyze(
            Box::new(reader),
            AnalysisToken::from("rhythm-fixture"),
            rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the rhythm pass opens under the pool budget");
    let progress = block_on(async {
        while results.changed().await.is_ok() {}
        results.borrow().clone()
    })
    .expect("production rhythm analysis produced no result");
    let analysis = progress.analysis();
    assert!(
        analysis.is_settled(),
        "rhythm analysis must cover the source"
    );
    let frames = analysis
        .extent()
        .expect("settled rhythm analysis has a source extent");
    let artifact = analysis.beat().map(|beat| beat.artifact().clone());
    (artifact, frames)
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

struct PcmReader {
    spec: AudioSpec,
    bus: EventBus,
    pools: Pools,
    metadata: TrackMetadata,
    samples: Vec<f32>,
    cursor: usize,
}

impl PcmReader {
    fn decode(bytes: &[u8], hint: &str) -> Result<Self, String> {
        let config = DecoderConfig::<NoResamplerBackend, TestPools>::builder()
            .pools(pools())
            .build();
        let mut decoder =
            DecoderFactory::create_with_probe(Cursor::new(bytes.to_vec()), Some(hint), config)
                .map_err(|error| format!("open: {error}"))?;
        let spec = decoder.spec();
        let metadata = decoder.metadata();
        let mut samples = Vec::new();
        loop {
            match decoder
                .next_chunk()
                .map_err(|error| format!("decode: {error}"))?
            {
                DecoderChunkOutcome::Chunk(chunk) => samples.extend_from_slice(&chunk.samples),
                DecoderChunkOutcome::Pending(reason) => {
                    return Err(format!("in-memory source is pending: {reason:?}"));
                }
                DecoderChunkOutcome::Eof => break,
            }
        }
        Ok(Self {
            spec,
            metadata,
            samples,
            bus: EventBus::default(),
            cursor: 0,
            pools: pools(),
        })
    }

    fn position_at(&self, frame: usize) -> Duration {
        self.spec
            .duration_for(u64::try_from(frame).expect("invariant: fixture frame fits u64"))
            .expect("invariant: fixture duration fits platform duration")
    }

    fn total_frames(&self) -> usize {
        self.samples.len() / usize::from(self.spec.channels)
    }
}

impl AudioSession for PcmReader {
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

impl AudioRead for PcmReader {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        if self.cursor >= self.total_frames() {
            return Ok(ChunkOutcome::Eof {
                position: self.position_at(self.cursor),
            });
        }
        let start = self.cursor;
        let end = start
            .saturating_add(consts::CHUNK_FRAMES)
            .min(self.total_frames());
        let channels = usize::from(self.spec.channels);
        let sample_start = start * channels;
        let sample_end = end * channels;
        let mut samples = self
            .pools
            .get_with_len::<f32>(sample_end - sample_start)
            .map_err(DecodeError::from)?;
        samples.copy_from_slice(&self.samples[sample_start..sample_end]);
        self.cursor = end;
        Ok(ChunkOutcome::Chunk(Box::new(AudioChunk::new(
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
        ))))
    }

    fn position(&self) -> Duration {
        self.position_at(self.cursor)
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, AudioReadError> {
        Err(DecodeError::InvalidData {
            detail: "rhythm analysis reads whole chunks",
        }
        .into())
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError> {
        Err(DecodeError::InvalidData {
            detail: "rhythm analysis reads whole chunks",
        }
        .into())
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for PcmReader {
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        let frame = usize::try_from(self.spec.frame_at(target).map_err(DecodeError::from)?)
            .map_err(|_| DecodeError::SeekOutOfRange {
                detail: "fixture seek does not fit usize",
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
