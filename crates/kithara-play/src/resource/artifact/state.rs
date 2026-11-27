use kithara_platform::sync::Arc;

/// What a track knows of a document its settings may request.
///
/// A ready document is held behind an [`Arc`]: the state is copied into every
/// snapshot of the track, and the document itself never changes.
#[derive(Debug)]
#[derive_where::derive_where(Clone)]
pub enum ArtifactState<T> {
    /// The track's settings carry no request for the document.
    NotRequested,
    /// The document is being read.
    Loading,
    /// The document was read.
    Ready(Arc<T>),
    /// The read failed. Nothing stands in for the document.
    Failed,
}
