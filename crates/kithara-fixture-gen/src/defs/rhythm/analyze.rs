use std::{
    io::Cursor,
    num::{NonZeroU32, NonZeroUsize},
    sync::{Mutex, OnceLock, PoisonError},
};

use futures_lite::future::block_on;
use kithara_analysis::{
    AnalysisDemand, AnalysisOutcome, AnalysisToken, AnalysisWorker, AnalysisWorkerConfig,
    AnalyzerBuilder, BeatArtifact,
};
use kithara_audio::{
    AudioControl, AudioRead, AudioReadError, AudioSession, ChunkOutcome, DecodeError, ReadOutcome,
    SeekOutcome,
};
use kithara_decode::{
    DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, TrackMetadata,
};
use kithara_events::EventBus;
use kithara_fixture_media::reference::AudioReference;
use kithara_platform::{thread, time::Duration};
use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::bufpool::{Pools, TestPools, pools};

use super::assets::analysis_file;

mod consts {
    pub(super) const CHUNK_FRAMES: usize = 4_096;
}

pub(in crate::defs) fn source_format() -> Vec<u8> {
    let mut format = crate::reference::format();
    format.extend_from_slice(
        format!(
            "analysis-source:v1;backend={};gapless=true;resampler=none;version={}",
            DecoderBackend::default(),
            env!("CARGO_PKG_VERSION")
        )
        .as_bytes(),
    );
    format
}

pub(in crate::defs) fn source_pcm(inputs: &[&[u8]], hint: &str) -> Vec<u8> {
    let [track] = inputs else {
        panic!("source PCM requires one encoded track");
    };
    let prepared = PcmReader::decode(track, hint)
        .unwrap_or_else(|error| panic!("native {hint} fixture input: {error}"));
    Vec::try_from(&prepared).unwrap_or_else(|error| panic!("native PCM fixture record: {error}"))
}

/// The beat pass over the complete prepared native source of a whole track.
pub(in crate::defs) fn analysed(inputs: &[&[u8]]) -> Vec<u8> {
    let [track] = inputs else {
        panic!("an analysis requires one prepared source PCM record");
    };
    let prepared = AudioReference::from_bytes(track).expect("prepared analysis source PCM");
    let reader = PcmReader::from(prepared);
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
    let (_results, _producer, completion) = worker()
        .analyze(
            Box::new(reader),
            AnalysisToken::from("rhythm-fixture"),
            rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the rhythm pass opens under the pool budget");
    let progress = match block_on(completion.outcome()) {
        AnalysisOutcome::Complete(progress) | AnalysisOutcome::Incomplete(progress) => progress,
        outcome => panic!("production rhythm analysis did not settle: {outcome:?}"),
    };
    let analysis = progress.analysis();
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
    fn decode(bytes: &[u8], hint: &str) -> Result<AudioReference, String> {
        let config = DecoderConfig::<NoResamplerBackend, TestPools>::builder()
            .gapless(true)
            .pools(pools())
            .build();
        let mut decoder =
            DecoderFactory::create_with_probe(Cursor::new(bytes.to_vec()), Some(hint), config)
                .map_err(|error| format!("open: {error}"))?;
        let spec = decoder.spec();
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
        Ok(AudioReference {
            sample_rate: spec.sample_rate.get(),
            channels: spec.channels,
            samples,
            presentation_frames: None,
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

impl From<AudioReference> for PcmReader {
    fn from(prepared: AudioReference) -> Self {
        Self {
            spec: AudioSpec::new(
                prepared.channels,
                NonZeroU32::new(prepared.sample_rate).expect("validated native fixture rate"),
            ),
            metadata: TrackMetadata::default(),
            samples: prepared.samples,
            bus: EventBus::default(),
            cursor: 0,
            pools: pools(),
        }
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn native_source_record_and_analysis_reader_preserve_every_decoded_bit() {
        let wav = crate::signal::wav(48_000, 2, 17, crate::signal::Wave::Sawtooth);
        let decoded = PcmReader::decode(&wav, "wav").expect("native fixture decode");
        let bytes = source_pcm(&[&wav], "wav");
        let prepared = AudioReference::from_bytes(&bytes).expect("native fixture record");
        assert_eq!((prepared.sample_rate, prepared.channels), (48_000, 2));
        assert_eq!(prepared.presentation_frames, None);
        assert_eq!(prepared.samples.len(), 34);
        let bits = |samples: &[f32]| {
            samples
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>()
        };
        assert_eq!(bits(&prepared.samples), bits(&decoded.samples));
        let mut reader = PcmReader::from(prepared);
        let ChunkOutcome::Chunk(chunk) = reader.next_chunk().expect("analysis input PCM") else {
            panic!("analysis reader retains all prepared frames");
        };
        assert_eq!(chunk.meta.frames, 17);
        assert_eq!(chunk.meta.frame_offset, 0);
        assert_eq!(bits(&chunk.samples), bits(&decoded.samples));
        assert!(matches!(reader.next_chunk(), Ok(ChunkOutcome::Eof { .. })));
    }

    #[kithara::test]
    fn the_build_decoder_profile_keys_both_source_and_analysis_formats() {
        let source = source_format();
        let profile = format!(
            "analysis-source:v1;backend={};gapless=true;resampler=none;version={}",
            DecoderBackend::default(),
            env!("CARGO_PKG_VERSION")
        );
        assert!(source.starts_with(&crate::reference::format()));
        assert!(source.ends_with(profile.as_bytes()));
        let analysis = super::super::assets::analysed_format();
        assert!(analysis.ends_with(&source));
        for (producer, format) in [("source_pcm", source), ("analysis", analysis)] {
            let original = crate::store::formatted_asset_id(producer, "track", &format);
            let mut changed = format;
            changed.extend_from_slice(b";profile-revision=2");
            assert_ne!(
                original,
                crate::store::formatted_asset_id(producer, "track", &changed)
            );
        }
    }

    #[kithara::test]
    fn analysis_depends_on_the_same_prepared_source_as_warp() {
        let find = |name| {
            inventory::iter::<crate::registry::AssetDef>
                .into_iter()
                .find(|definition| definition.accessor_name() == name)
                .expect("registered whole-track fixture dependency")
        };
        let original = find("drain_tone_default");
        let source = find("source_pcm_drain_tone_default");
        let analysis = find("analysis_drain_tone_default");
        assert_eq!(source.dependencies, &["drain_tone_default"]);
        assert_eq!(analysis.dependencies, &["source_pcm_drain_tone_default"]);
        assert_eq!(source.optional, original.optional);
        assert_eq!(analysis.optional, original.optional);
        assert_eq!(source.env, original.env);
        assert_eq!(analysis.env, original.env);
        let source_format = source.format.expect("profile-keyed native source")();
        let analysis_format = analysis.format.expect("profile-keyed analysis")();
        assert!(analysis_format.ends_with(&source_format));
    }
}
