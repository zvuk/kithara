/// Errors surfaced by ABR state mutations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AbrError {
    #[error("variant index {requested} out of bounds (available: {available})")]
    VariantOutOfBounds { requested: usize, available: usize },
}
