use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub use kithara::assets::{AssetReader, ReadSide};
use kithara::{
    assets::{
        AcquisitionResult, AssetResource, AssetScope, AssetSource, AssetStore, StorageBackend,
        WriteSide,
    },
    encode::EncodeConfig,
    record::{RecordingConfig, RecordingCore},
    warp::BeatGridSnapshot,
};
use kithara_app::recording::AssetPartSink;
use serde::Serialize;
use serde_json::Value;

use crate::{
    artifact_timeline::ArtifactTimeline,
    bufpool_ext::{TestPools, pools},
    underrun_ledger::UnderrunLedger,
    usdt_trace,
};

const ARTIFACT_DIR_ENV: &str = "KITHARA_AUDIO_ARTIFACT_DIR";
static ATTEMPT: AtomicU64 = AtomicU64::new(0);
const TRACK_GAIN: f32 = 0.8;
const METRONOME_GAIN: f32 = 0.2;
const CLICK_FRAMES: usize = 480;
const BEAT_HZ: f32 = 1_760.0;
const DOWNBEAT_HZ: f32 = 2_200.0;
const BEAT_AMPLITUDE: f32 = 0.45;
const DOWNBEAT_AMPLITUDE: f32 = 0.72;

/// Decaying saw clicks at each Host beat, louder and higher on downbeats, shaped like `pcm`.
fn metronome(
    beats: &BTreeMap<u64, bool>,
    pcm: &[f32],
    channels: u16,
    sample_rate: u32,
) -> Vec<f32> {
    let channels = usize::from(channels);
    let frames = pcm.len() / channels;
    let mut clicks = vec![0.0; pcm.len()];
    for (&frame, &downbeat) in beats {
        let Ok(start) = usize::try_from(frame) else {
            continue;
        };
        let (amplitude, frequency) = if downbeat {
            (DOWNBEAT_AMPLITUDE, DOWNBEAT_HZ)
        } else {
            (BEAT_AMPLITUDE, BEAT_HZ)
        };
        for offset in 0..CLICK_FRAMES.min(frames.saturating_sub(start)) {
            let phase = (offset as f32 * frequency / sample_rate as f32).fract();
            let envelope = 1.0 - offset as f32 / CLICK_FRAMES as f32;
            let sample = amplitude * phase.mul_add(2.0, -1.0) * envelope;
            let index = (start + offset) * channels;
            clicks[index..index + channels].fill(sample);
        }
    }
    clicks
}

pub type AudioArtifactRecording = RecordingCore<AssetPartSink<TestPools>>;

/// One opt-in disk `AssetStore` scope for related listening artifacts.
pub struct AudioArtifactSet {
    channels: u16,
    sample_rate: u32,
    case: String,
    root: PathBuf,
    scope: AssetScope<TestPools>,
}

/// One listening artifact per harness: PCM pushed by the render funnel,
/// markers stamped by control calls, published on drop.
pub struct AudioArtifactTap {
    set: AudioArtifactSet,
    recording: Option<AudioArtifactRecording>,
    markers: Vec<Marker>,
    frames: u64,
    channels: u16,
    timeline: ArtifactTimeline,
    source_grids: BTreeMap<u64, BeatGridSnapshot>,
    evidence: BTreeMap<String, Value>,
    pcm: Vec<f32>,
    host_beats: BTreeMap<u64, bool>,
}

#[derive(Serialize)]
struct Marker {
    frame: u64,
    label: String,
}

impl AudioArtifactTap {
    /// Build a tap only when `KITHARA_AUDIO_ARTIFACT_DIR` is set.
    pub fn from_env(case: &str, sample_rate: u32, channels: u16) -> io::Result<Option<Self>> {
        let Some(set) = AudioArtifactSet::from_env(case, sample_rate, channels)? else {
            return Ok(None);
        };
        Self::over(set).map(Some)
    }

    /// Open the tap over an artifact set that is already placed on disk.
    fn over(set: AudioArtifactSet) -> io::Result<Self> {
        let channels = set.channels;
        let recording = set.recording("output", None)?;
        Ok(Self {
            set,
            recording: Some(recording),
            markers: Vec::new(),
            frames: 0,
            channels,
            timeline: ArtifactTimeline::default(),
            source_grids: BTreeMap::new(),
            evidence: BTreeMap::new(),
            pcm: Vec::new(),
            host_beats: BTreeMap::new(),
        })
    }

    /// Build a tap in the opt-in directory, falling back to `fallback` when unset.
    ///
    /// A harness whose product is the recording itself must have somewhere to
    /// write in every ordinary run, not only under the opt-in variable.
    pub fn from_env_or(
        fallback: &Path,
        case: &str,
        sample_rate: u32,
        channels: u16,
    ) -> io::Result<Self> {
        if let Some(tap) = Self::from_env(case, sample_rate, channels)? {
            return Ok(tap);
        }
        Self::over(AudioArtifactSet::new(
            fallback,
            case,
            sample_rate,
            channels,
        )?)
    }

    pub fn push(&mut self, pcm: &[f32]) {
        if let Some(recording) = self.recording.as_mut() {
            recording
                .push(pcm)
                .unwrap_or_else(|error| panic!("listening tap push: {error}"));
            self.pcm.extend_from_slice(pcm);
            self.frames += (pcm.len() / usize::from(self.channels)) as u64;
        }
    }

    pub fn mark(&mut self, label: &str) {
        self.markers.push(Marker {
            frame: self.frames,
            label: label.to_owned(),
        });
        self.timeline
            .point("control", self.frames, "command", label);
    }

    pub fn timeline(&mut self) -> &mut ArtifactTimeline {
        &mut self.timeline
    }

    pub fn source_grid(&mut self, track: u64, grid: BeatGridSnapshot) {
        self.source_grids.insert(track, grid);
    }

    /// Attach test-owned observed evidence to this artifact's manifest.
    pub fn evidence(&mut self, key: &str, value: Value) {
        self.evidence.insert(key.to_owned(), value);
    }

    /// Per-track ledger of every starved render this capture observed.
    ///
    /// Each entry carries the output interval the feeder silenced and the source
    /// frontier it stopped at, so a starved capture is read at its own frames
    /// instead of through a bare counter.
    #[must_use]
    pub fn underrun_ledger(&self) -> UnderrunLedger {
        UnderrunLedger::from_probes(&usdt_trace::events())
    }

    /// Mark a Host beat at an output frame for the published metronome.
    pub fn host_beat(&mut self, frame: u64, downbeat: bool) {
        self.host_beats.insert(frame, downbeat);
    }

    /// Output frames of the Host beats marked inside `frames`.
    pub fn host_beats_in(&self, frames: std::ops::Range<u64>) -> Vec<u64> {
        self.host_beats
            .range(frames)
            .map(|(frame, _)| *frame)
            .collect()
    }

    /// The captured output with the Host metronome laid over it, and how many samples clipped.
    pub fn metronome_mix(&self) -> (Vec<f32>, usize) {
        self.mix_with(&self.clicks())
    }

    fn clicks(&self) -> Vec<f32> {
        metronome(
            &self.host_beats,
            &self.pcm,
            self.channels,
            self.set.sample_rate,
        )
    }

    fn mix_with(&self, clicks: &[f32]) -> (Vec<f32>, usize) {
        let mut clipped = 0;
        let mix = self
            .pcm
            .iter()
            .zip(clicks)
            .map(|(track, click)| {
                let sample = track * TRACK_GAIN + click * METRONOME_GAIN;
                if sample.abs() > 1.0 {
                    clipped += 1;
                }
                sample.clamp(-1.0, 1.0)
            })
            .collect();
        (mix, clipped)
    }

    fn publish_pcm(&self, label: &str, pcm: &[f32]) -> Option<PathBuf> {
        let frames = u64::try_from(pcm.len() / usize::from(self.channels)).ok()?;
        self.set
            .recording(label, Some(frames))
            .and_then(|mut recording| {
                recording.push(pcm).map_err(io::Error::other)?;
                AudioArtifactSet::finish(recording)
            })
            .and_then(|reader| audio_artifact_path(&reader))
            .map_err(|error| eprintln!("KITHARA_AUDIO_ARTIFACT {label} not published: {error}"))
            .ok()
    }
}

impl Drop for AudioArtifactTap {
    fn drop(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let output = match AudioArtifactSet::finish(recording) {
            Ok(reader) => audio_artifact_path(&reader).ok(),
            Err(error) => {
                eprintln!("KITHARA_AUDIO_ARTIFACT output not published: {error}");
                None
            }
        };
        let (metronome_output, metronome_clicks, metronome_clipped) = if self.host_beats.is_empty()
        {
            (None, None, 0)
        } else {
            let clicks = self.clicks();
            let (mix, clipped) = self.mix_with(&clicks);
            (
                self.publish_pcm("output-metronome", &mix),
                self.publish_pcm("metronome", &clicks),
                clipped,
            )
        };
        let (probes, probes_truncated) = usdt_trace::recorded();
        let underruns = UnderrunLedger::from_probes(&probes);
        self.timeline.record_probes(&probes);
        self.timeline.record_underruns(&underruns);
        for (&track, grid) in &self.source_grids {
            self.timeline.record_source_grid(track, grid);
        }
        let timeline = if self.timeline.is_empty() {
            None
        } else {
            self.set
                .write_bytes("timeline.svg", self.timeline.svg().as_bytes())
                .and_then(|reader| audio_artifact_path(&reader))
                .map_err(|error| {
                    eprintln!("KITHARA_AUDIO_ARTIFACT timeline not published: {error}");
                    error
                })
                .ok()
        };
        let manifest = serde_json::json!({
            "case": self.set.case,
            "frames": self.frames,
            "channels": self.channels,
            "sample_rate": self.set.sample_rate,
            "markers": self.markers,
            "output": output,
            "timeline": timeline,
            "metronome": {
                "clicks": metronome_clicks,
                "output": metronome_output,
                "track_gain": TRACK_GAIN,
                "metronome_gain": METRONOME_GAIN,
                "clipped_samples": metronome_clipped,
                "host_beats": self.host_beats.len(),
            },
            "timeline_events": self.timeline.events(),
            "underruns": underruns,
            "probes_truncated": probes_truncated,
            "evidence": self.evidence,
        });
        match self
            .set
            .write_manifest(&manifest)
            .and_then(|reader| audio_artifact_path(&reader))
        {
            Ok(path) => {
                if let Err(error) =
                    self.set
                        .append_index(&path, output.as_deref(), timeline.as_deref())
                {
                    eprintln!("KITHARA_AUDIO_ARTIFACT index not extended: {error}");
                }
                eprintln!("KITHARA_AUDIO_ARTIFACT manifest: {}", path.display());
            }
            Err(error) => eprintln!("KITHARA_AUDIO_ARTIFACT manifest not published: {error}"),
        }
    }
}

/// Return the running test's path as an artifact label.
#[must_use]
pub fn artifact_label() -> String {
    let name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

impl AudioArtifactSet {
    /// Build an artifact set only when the absolute opt-in directory is set.
    pub fn from_env(case: &str, sample_rate: u32, channels: u16) -> io::Result<Option<Self>> {
        let Some(root) = env::var_os(ARTIFACT_DIR_ENV).map(PathBuf::from) else {
            return Ok(None);
        };
        Self::new(&root, case, sample_rate, channels).map(Some)
    }

    /// Build an artifact set in the opt-in directory, or in `fallback` when unset.
    ///
    /// A recorder whose whole product is the artifact must run in every
    /// ordinary suite run, so it needs a directory unconditionally.
    pub fn from_env_or(
        fallback: &Path,
        case: &str,
        sample_rate: u32,
        channels: u16,
    ) -> io::Result<Self> {
        if let Some(set) = Self::from_env(case, sample_rate, channels)? {
            return Ok(set);
        }
        Self::new(fallback, case, sample_rate, channels)
    }

    /// Build an artifact set in an explicit absolute directory.
    pub fn new(root: &Path, case: &str, sample_rate: u32, channels: u16) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{ARTIFACT_DIR_ENV} must be an absolute path"),
            ));
        }
        if sample_rate == 0 || channels == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio artifact sample rate and channel count must be non-zero",
            ));
        }
        validate_label(case)?;
        let attempt = ATTEMPT.fetch_add(1, Ordering::Relaxed);
        let source = AssetSource::Local {
            path: root.join(format!("{case}-{}-{attempt}", std::process::id())),
        };
        let store = AssetStore::builder(pools())
            .backend(StorageBackend::Disk {
                root: root.to_path_buf(),
            })
            .build();
        let scope = store.scope::<Self>(&source).map_err(io::Error::other)?;
        Ok(Self {
            channels,
            sample_rate,
            case: case.to_owned(),
            root: root.to_path_buf(),
            scope,
        })
    }

    /// Open one WAV float32 transaction.
    pub fn recording(
        &self,
        label: &str,
        expected_frames: Option<u64>,
    ) -> io::Result<AudioArtifactRecording> {
        validate_label(label)?;
        let key = self.key(&format!("{label}.wav"))?;
        let sink = AssetPartSink::acquire(self.scope.store(), &key).map_err(io::Error::other)?;
        let config = RecordingConfig::builder()
            .encode(
                EncodeConfig::builder()
                    .sample_rate(self.sample_rate)
                    .channels(self.channels)
                    .build(),
            )
            .build();
        RecordingCore::new(&config, sink, expected_frames).map_err(io::Error::other)
    }

    /// Finish and atomically publish one audio artifact.
    pub fn finish(recording: AudioArtifactRecording) -> io::Result<AssetReader<TestPools>> {
        recording.finish().map_err(io::Error::other)
    }

    /// Serialize and atomically publish the set manifest.
    pub fn write_manifest<T: Serialize>(&self, manifest: &T) -> io::Result<AssetReader<TestPools>> {
        let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
        self.write_bytes("manifest.json", &bytes)
    }

    /// Append this artifact to the run-wide index so a reader can find the
    /// WAV and SVG of a named case without opening every hashed directory.
    pub fn append_index(
        &self,
        manifest: &Path,
        output: Option<&Path>,
        timeline: Option<&Path>,
    ) -> io::Result<()> {
        let entry = serde_json::json!({
            "case": self.case,
            "sample_rate": self.sample_rate,
            "channels": self.channels,
            "manifest": manifest,
            "output": output,
            "timeline": timeline,
        });
        let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("index.jsonl"))?;
        file.write_all(&line)
    }

    /// Atomically publish one non-audio artifact in this set.
    pub fn write_bytes(&self, name: &str, bytes: &[u8]) -> io::Result<AssetReader<TestPools>> {
        validate_artifact_name(name)?;
        let key = self.key(name)?;
        let writer = match self
            .scope
            .store()
            .acquire_resource(&key, None)
            .map_err(io::Error::other)?
        {
            AcquisitionResult::Pending(writer) => writer,
            AcquisitionResult::Ready(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "audio artifact manifest is already committed",
                ));
            }
            _ => return Err(io::Error::other("unexpected manifest acquisition phase")),
        };
        writer.write_at(0, bytes).map_err(io::Error::other)?;
        writer
            .commit(Some(
                u64::try_from(bytes.len()).map_err(|_| io::Error::other("manifest too large"))?,
            ))
            .map_err(io::Error::other)
    }

    fn key(&self, name: &str) -> io::Result<kithara::assets::ResourceKey> {
        self.scope
            .key(&AssetResource::Named {
                namespace: "artifacts".to_owned(),
                name: name.to_owned(),
            })
            .map_err(io::Error::other)
    }
}

/// Return the absolute disk path of a committed artifact.
pub fn audio_artifact_path(reader: &AssetReader<TestPools>) -> io::Result<PathBuf> {
    let path = reader
        .path()
        .ok_or_else(|| io::Error::other("disk audio artifact has no path"))?;
    if !path.is_absolute() {
        return Err(io::Error::other("audio artifact path is not absolute"));
    }
    Ok(path.to_path_buf())
}

/// Write listening WAV files and a manifest when the artifact directory is set.
pub fn write_audio_artifact<T: Serialize>(
    case: &str,
    sample_rate: u32,
    channels: u16,
    audio: &[(&str, &[f32])],
    manifest: &T,
) -> io::Result<Option<PathBuf>> {
    let Some(set) = AudioArtifactSet::from_env(case, sample_rate, channels)? else {
        return Ok(None);
    };
    for (label, samples) in audio {
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio artifact samples must contain complete interleaved frames",
            ));
        }
        let frames = u64::try_from(samples.len() / usize::from(channels))
            .map_err(|_| io::Error::other("audio artifact frame count overflow"))?;
        let mut recording = set.recording(label, Some(frames))?;
        recording.push(samples).map_err(io::Error::other)?;
        let _ = AudioArtifactSet::finish(recording)?;
    }
    let manifest = set.write_manifest(manifest)?;
    let directory = audio_artifact_path(&manifest)?
        .parent()
        .ok_or_else(|| io::Error::other("audio artifact manifest has no parent"))?
        .to_path_buf();
    Ok(Some(directory))
}

fn validate_label(label: &str) -> io::Result<()> {
    if label.is_empty()
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact label must contain only ASCII letters, digits, '-' or '_': {label}"),
        ));
    }
    Ok(())
}

fn validate_artifact_name(name: &str) -> io::Result<()> {
    let mut parts = name.split('.');
    let stem = parts.next().unwrap_or_default();
    let extension = parts.next().unwrap_or_default();
    if parts.next().is_some() || extension.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact name must contain one extension: {name}"),
        ));
    }
    validate_label(stem)?;
    validate_label(extension)
}
