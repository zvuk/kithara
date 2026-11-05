/// Why a window interpolation or weighted-frame operation refused a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InterpError {
    #[error("interpolation position lies outside the window")]
    OutOfWindow,
    /// Frame offsets, weights or output scratch do not describe complete frames.
    #[error("weighted frame slices have incompatible shapes")]
    InvalidShape,
}
