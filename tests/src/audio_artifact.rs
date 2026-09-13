use std::{
    env, io,
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
};
use kithara_app::recording::AssetPartSink;
use serde::Serialize;

use crate::bufpool_ext::{TestPools, pools};

const ARTIFACT_DIR_ENV: &str = "KITHARA_AUDIO_ARTIFACT_DIR";
static ATTEMPT: AtomicU64 = AtomicU64::new(0);

pub type AudioArtifactRecording = RecordingCore<AssetPartSink<TestPools>>;

/// One opt-in disk `AssetStore` scope for related listening artifacts.
pub struct AudioArtifactSet {
    channels: u16,
    sample_rate: u32,
    scope: AssetScope<TestPools>,
}

impl AudioArtifactSet {
    /// Build an artifact set only when the absolute opt-in directory is set.
    pub fn from_env(case: &str, sample_rate: u32, channels: u16) -> io::Result<Option<Self>> {
        let Some(root) = env::var_os(ARTIFACT_DIR_ENV).map(PathBuf::from) else {
            return Ok(None);
        };
        Self::new(&root, case, sample_rate, channels).map(Some)
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
        let key = self.key("manifest.json")?;
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
        writer.write_at(0, &bytes).map_err(io::Error::other)?;
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
///
/// Returns `Ok(None)` when unset and errors on invalid input or `AssetStore` I/O.
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
