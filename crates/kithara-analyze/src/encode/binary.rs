use std::io::{Error, ErrorKind};

use kithara::analysis::AnalysisOutcome;
use tracing::warn;

use crate::{RunError, analysis::OpenError, input::Track};

/// The analysis document of a settled pass, complete or not. A failed or
/// cancelled pass has no document: its status and cause go to the log.
pub(crate) fn binary(
    track: &Track,
    result: &Result<AnalysisOutcome, OpenError>,
) -> Result<Option<Vec<u8>>, RunError> {
    match result {
        Ok(AnalysisOutcome::Complete(progress) | AnalysisOutcome::Incomplete(progress)) => {
            let mut bytes = Vec::new();
            progress
                .analysis()
                .write_to(&mut bytes)
                .map_err(Error::other)?;
            Ok(Some(bytes))
        }
        Ok(AnalysisOutcome::Failed { error, .. }) => {
            warn!(id = %track.id, src = %track.src, status = "failed", cause = %error, "no analysis document");
            Ok(None)
        }
        Ok(AnalysisOutcome::Cancelled { .. }) => {
            warn!(id = %track.id, src = %track.src, status = "cancelled", "no analysis document");
            Ok(None)
        }
        Err(error) => {
            warn!(id = %track.id, src = %track.src, status = "failed", cause = %error, "no analysis document");
            Ok(None)
        }
        Ok(_) => Err(Error::new(ErrorKind::Unsupported, "unknown analysis outcome").into()),
    }
}
