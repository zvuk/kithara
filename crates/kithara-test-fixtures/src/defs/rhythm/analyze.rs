#[cfg(feature = "library")]
use std::io::Cursor;
use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::{Mutex, OnceLock, PoisonError},
};

use futures_lite::future::block_on;
use kithara_analysis::{
    AnalysisToken, AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder, BeatArtifact,
};
use kithara_audio::{
    AudioControl, AudioRead, AudioSession, ChunkOutcome, DecodeError, ReadOutcome, SeekOutcome,
};
use kithara_decode::TrackMetadata;
#[cfg(feature = "library")]
use kithara_decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory};
use kithara_events::EventBus;
use kithara_platform::{thread, time::Duration};
#[cfg(feature = "library")]
use kithara_resampler::NoResamplerBackend;
use kithara_resampler::rubato::RubatoBackend;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::bufpool::{Pools, TestPools, pools};

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
    let reader = PcmReader::parse_wav(wav).unwrap_or_else(|error| panic!("rhythm WAV: {error}"));
    analyze(reader).0
}

#[cfg(feature = "library")]
pub(in crate::defs) fn beat_encoded(bytes: &[u8], hint: &str) -> (BeatArtifact, u64) {
    let reader =
        PcmReader::decode(bytes, hint).unwrap_or_else(|error| panic!("library {hint}: {error}"));
    analyze(reader)
}

/// One analysis at a time: a session keeps the whole mono track and its
/// detection buffers in the worker's shared pool region, and `build.rs`
/// materialises every library sidecar in one batch, so eleven concurrent
/// sessions exceeded the region budget and settled without a beat grid.
fn analyze(reader: PcmReader) -> (BeatArtifact, u64) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
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
    .expect("production rhythm analysis produced no result");
    let analysis = progress.analysis();
    assert!(
        analysis.is_settled(),
        "rhythm analysis must cover the source"
    );
    let frames = analysis
        .extent()
        .expect("settled rhythm analysis has a source extent");
    let artifact = analysis
        .beat()
        .expect("production rhythm analysis produced no beat artifact")
        .artifact()
        .clone();
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
    fn parse_wav(bytes: &[u8]) -> Result<Self, String> {
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
            samples,
            bus: EventBus::default(),
            cursor: 0,
            metadata: TrackMetadata::default(),
            pools: pools(),
            spec: AudioSpec::new(channels, sample_rate),
        })
    }

    #[cfg(feature = "library")]
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
            bus: EventBus::default(),
            cursor: 0,
            metadata,
            pools: pools(),
            samples,
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

impl AudioControl for PcmReader {
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
