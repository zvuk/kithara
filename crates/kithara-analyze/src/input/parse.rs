use std::{
    collections::HashMap,
    io::BufRead,
    num::{NonZeroU32, NonZeroUsize},
    path::{Path, PathBuf, absolute},
};

use clap::{Error, Parser, ValueEnum, error::ErrorKind};
use kithara::play::ResourceSrc;
use serde::Deserialize;

/// Command-line options for one batch.
#[derive(Debug, Parser)]
#[command(version, about)]
#[non_exhaustive]
pub struct Args {
    /// Native waveform columns folded into each bucket.
    #[arg(long, default_value = "1")]
    pub(crate) waveform_fold: NonZeroU32,
    /// Maximum concurrent source loads and analysis passes.
    #[arg(long)]
    pub(crate) jobs: Option<NonZeroUsize>,
    /// Directory for atomic per-track files; omit for stdout NDJSON.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Record encoding.
    #[arg(long, value_enum, default_value = "json")]
    pub(crate) format: OutputFormat,
    /// Analyzers to enable, separated by commas.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "waveform,beat,key"
    )]
    pub(crate) artifacts: Vec<Artifact>,
    /// Local paths, HTTP(S) URLs, or '-' for NDJSON on stdin.
    #[arg(required = true)]
    sources: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Artifact {
    Waveform,
    Beat,
    Key,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum OutputFormat {
    Json,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Track {
    pub(crate) id: String,
    pub(crate) src: String,
}

/// Validated input, ready to run without reading more standard input.
#[derive(Debug)]
#[non_exhaustive]
pub struct Batch {
    pub(crate) args: Args,
    pub(crate) tracks: Vec<Track>,
}

impl Batch {
    /// Read and validate every source before starting any analysis.
    ///
    /// # Errors
    /// Returns a usage error for malformed records or duplicate identifiers.
    pub fn try_new<R: BufRead>(args: Args, mut input: R) -> Result<Self, Error> {
        let mut tracks: Vec<Track> = Vec::new();
        for src in &args.sources {
            if src == "-" {
                for (index, line) in (&mut input).lines().enumerate() {
                    let line =
                        line.map_err(|error| usage(format!("line {}: {error}", index + 1)))?;
                    let track: Track = serde_json::from_str(&line)
                        .map_err(|error| usage(format!("line {}: {error}", index + 1)))?;
                    validate_id(&track.id)
                        .map_err(|error| usage(format!("line {}: {error}", index + 1)))?;
                    tracks.push(track);
                }
            } else {
                tracks.push(Track::try_from(src.as_str())?);
            }
        }
        let mut sources = HashMap::new();
        for track in &tracks {
            if let Some(previous) = sources.insert(&track.id, &track.src) {
                return Err(usage(format!(
                    "duplicate identifier {:?}: {previous} and {}",
                    track.id, track.src
                )));
            }
        }
        Ok(Self { args, tracks })
    }
}

impl TryFrom<&str> for Track {
    type Error = Error;

    fn try_from(src: &str) -> Result<Self, Self::Error> {
        let id = match source(src).map_err(usage)? {
            ResourceSrc::Url(url) => stem(Path::new(url.path()))?,
            ResourceSrc::Path(path) => stem(&path)?,
        };
        validate_id(&id).map_err(usage)?;
        Ok(Self {
            id,
            src: src.to_owned(),
        })
    }
}

/// Where `src` points: an `http(s)` URL, otherwise a local path made absolute.
pub(crate) fn source(src: &str) -> Result<ResourceSrc, String> {
    if src.starts_with("http://") || src.starts_with("https://") {
        ResourceSrc::parse(src).map_err(|error| format!("{src}: {error}"))
    } else {
        absolute(src)
            .map(ResourceSrc::Path)
            .map_err(|error| format!("{src}: {error}"))
    }
}

fn stem(path: &Path) -> Result<String, Error> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .ok_or_else(|| usage(format!("source has no file stem: {}", path.display())))
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || matches!(id, "." | "..") || id.contains(['/', '\\', '\0']) {
        return Err(format!("identifier must be a single file name: {id:?}"));
    }
    Ok(())
}

fn usage(message: String) -> Error {
    Error::raw(ErrorKind::ValueValidation, message)
}
