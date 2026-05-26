use std::{error::Error as StdError, io};

pub(crate) type SymphoniaError = symphonia::core::errors::Error;

/// Inspect `err` for a Symphonia-wrapped error.
///
/// Returns:
/// - `Some(true)` / `Some(false)` — `err` IS a Symphonia error and was
///   classified via `check_io` (for its wrapped `io::Error`) or
///   `check_leaf` (for other variants).
/// - `None` — `err` is not a Symphonia error; the caller should keep
///   walking its source chain.
pub(crate) fn inspect<I, L>(
    err: &(dyn StdError + 'static),
    check_io: &I,
    check_leaf: &L,
) -> Option<bool>
where
    I: Fn(&io::Error) -> bool,
    L: Fn(&(dyn StdError + 'static)) -> bool,
{
    let sym_err = err.downcast_ref::<SymphoniaError>()?;
    Some(match sym_err {
        SymphoniaError::IoError(io_err) => check_io(io_err),
        _ => check_leaf(err),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Error as IoError;

    use kithara_test_utils::kithara;

    use crate::error::DecodeError;

    #[kithara::test]
    fn test_backend_symphonia_seek_pending_counts_as_interrupted() {
        let decode_err = DecodeError::backend(super::SymphoniaError::IoError(IoError::other(
            kithara_stream::PendingReason::SeekPending,
        )));
        assert!(decode_err.is_interrupted());
    }
}
