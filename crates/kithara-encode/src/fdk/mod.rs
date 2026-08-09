//! AAC encoding through `fdk-aac-sys`; the high-level crate disables the SBR
//! mode the HE profiles require.

pub(crate) mod aac_he;
pub(crate) mod aac_lc;
pub(crate) mod encoder;
