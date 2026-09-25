/// A source quantum cannot be admitted to the resident renderer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WarpRenderError {
    /// The previous producer must finish draining or retire outside the render core.
    #[error("renderer requires deferred service or transition drain")]
    NeedsService,
    /// Another source span is already prepared and retains its producer identity.
    #[error("another source quantum is already prepared")]
    OutstandingQuantum,
    /// A future plan needs the published output activation before accepting source.
    #[error("projection awaits its published output activation")]
    PendingActivation,
    /// This target has no renderer capable of applying a projection.
    #[error("projected rendering is unavailable on this target")]
    UnsupportedProjection,
    /// An operation contains no source frames.
    #[error("source quantum is empty")]
    EmptySource,
    /// The selected geometry or engine cannot accept the operation.
    #[cfg(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee",
        feature = "stretch-glide"
    ))]
    #[error(transparent)]
    Engine(#[from] kithara_stretch::ElasticError),
}
