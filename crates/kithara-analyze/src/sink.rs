use std::{
    fs,
    io::{Error, Write},
    path::PathBuf,
};

use kithara::{analysis::AnalysisOutcome, platform::tokio::sync::mpsc};
use tempfile::NamedTempFile;

use crate::{RunError, batch::TrackResult, encode, input::OutputFormat};

pub(crate) struct Sink<W> {
    directory: Option<PathBuf>,
    writer: W,
}

impl<W: Write> Sink<W> {
    pub(crate) async fn consume(
        mut self,
        mut records: mpsc::Receiver<TrackResult>,
        format: OutputFormat,
    ) -> Result<(bool, W), RunError> {
        let mut success = true;
        while let Some((track, outcome)) = records.recv().await {
            success &= matches!(
                outcome,
                Ok(AnalysisOutcome::Complete(_) | AnalysisOutcome::Incomplete(_))
            );
            let record = match format {
                OutputFormat::Json => Some(("json", encode::json(&track, &outcome)?)),
                OutputFormat::Binary => {
                    encode::binary(&track, &outcome)?.map(|bytes| ("analysis", bytes))
                }
            };
            let Some((extension, bytes)) = record else {
                continue;
            };
            if let Some(directory) = &self.directory {
                let mut temp = NamedTempFile::new_in(directory)?;
                temp.write_all(&bytes)?;
                temp.flush()?;
                temp.persist_noclobber(directory.join(format!("{}.{extension}", track.id)))
                    .map_err(|error| Error::other(error.to_string()))?;
            } else {
                self.writer.write_all(&bytes)?;
                self.writer.write_all(b"\n")?;
                self.writer.flush()?;
            }
        }
        Ok((success, self.writer))
    }

    pub(crate) fn try_new(directory: Option<PathBuf>, writer: W) -> Result<Self, RunError> {
        if let Some(directory) = &directory {
            fs::create_dir_all(directory)?;
        }
        Ok(Self { directory, writer })
    }
}
