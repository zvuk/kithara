/// A source quantum cannot be admitted to the resident renderer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WarpRenderError {
    /// Rendering stopped after an unrecoverable source mapping failure.
    #[error("renderer terminated after an unrecoverable source mapping failure")]
    Terminated,
    /// The previous producer must finish draining or retire outside the render core.
    #[error("renderer requires deferred service or transition drain")]
    NeedsService,
    /// Another source span is already prepared and retains its producer identity.
    #[error("another source quantum is already prepared")]
    OutstandingQuantum,
    /// This target has no renderer capable of applying a region plan.
    #[error("region plan rendering is unavailable on this target")]
    UnsupportedRegionPlan,
    /// Decoded input already has a nonlinear source law from another render stage.
    #[error("Warp requires affine decoded PCM geometry; nonlinear input is unsupported")]
    UnsupportedSourceGeometry,
    /// A source clock cannot represent the requested physical coordinate.
    #[error("decoded PCM source geometry is unrepresentable")]
    InvalidSourceGeometry,
    /// An operation contains no source frames.
    #[error("source quantum is empty")]
    EmptySource,
    /// The selected geometry or engine cannot accept the operation.
    #[error(transparent)]
    Engine(#[from] kithara_stretch::ElasticError),
}
