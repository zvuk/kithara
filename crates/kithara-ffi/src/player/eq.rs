use crate::types::FfiError;

/// Bound externally supplied EQ layouts before they allocate owner resources.
const MAX_EQ_BANDS: usize = if cfg!(target_arch = "wasm32") {
    64
} else {
    128
};

pub(crate) fn validate_eq_band_count(count: usize) -> Result<(), FfiError> {
    if count > MAX_EQ_BANDS {
        return Err(FfiError::InvalidArgument {
            reason: format!("EQ layout exceeds {MAX_EQ_BANDS} bands"),
        });
    }
    Ok(())
}
