use kithara_audio::AudioReadError;
use kithara_platform::{
    CancelToken,
    tokio::sync::{oneshot, watch},
};
use kithara_worker::TaskError;

use crate::{AnalysisProgress, BlobError, PassError};

/// The cause of an analysis pass failing to settle.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnalysisError {
    #[error("analysis admission rejected: {0}")]
    Rejected(#[from] TaskError),
    #[error("analysis resume rejected: {0}")]
    Resume(#[from] BlobError),
    #[error("analysis reader failed: {0}")]
    Read(#[from] AudioReadError),
    #[error("analysis analyzer failed: {0}")]
    Analyzer(#[from] PassError),
    #[error("analysis compute pool unavailable")]
    ComputeUnavailable,
    #[error("analysis source produced no publication")]
    Empty,
    #[error("analysis task stopped without a report")]
    Stopped,
}

/// A pass's terminal result and its last available publication.
#[derive(Debug)]
#[non_exhaustive]
pub enum AnalysisOutcome {
    Complete(AnalysisProgress),
    Incomplete(AnalysisProgress),
    Failed {
        error: AnalysisError,
        last: Option<AnalysisProgress>,
    },
    Cancelled {
        last: Option<AnalysisProgress>,
    },
}

/// The terminal report of a started pass, independent of progressive subscribers.
/// Dropping this handle does not cancel the pass.
#[non_exhaustive]
pub struct AnalysisCompletion {
    report: oneshot::Receiver<Result<AnalysisProgress, AnalysisError>>,
    results: watch::Receiver<Option<AnalysisProgress>>,
    cancel: CancelToken,
}

impl AnalysisCompletion {
    pub(crate) fn new(
        report: oneshot::Receiver<Result<AnalysisProgress, AnalysisError>>,
        results: watch::Receiver<Option<AnalysisProgress>>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            report,
            results,
            cancel,
        }
    }

    /// Waits for the pass to end and returns its terminal outcome.
    pub async fn outcome(self) -> AnalysisOutcome {
        let report = self.report.await;
        let last = self.results.borrow().clone();
        match report {
            Ok(Ok(progress)) if progress.analysis().is_complete() => {
                AnalysisOutcome::Complete(progress)
            }
            Ok(Ok(progress)) => AnalysisOutcome::Incomplete(progress),
            Ok(Err(error)) => AnalysisOutcome::Failed { error, last },
            Err(_) if self.cancel.is_cancelled() => AnalysisOutcome::Cancelled { last },
            Err(_) => AnalysisOutcome::Failed {
                last,
                error: AnalysisError::Stopped,
            },
        }
    }
}
