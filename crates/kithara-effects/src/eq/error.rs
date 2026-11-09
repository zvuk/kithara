use kithara_bufpool::PoolError;
use kithara_dsp::filter::FilterError;

/// Why the equaliser cannot prepare a layout or its storage.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EqError {
    #[error(transparent)]
    Pool(#[from] PoolError),
    #[error(transparent)]
    Filter(#[from] FilterError),
}
