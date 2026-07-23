//! HE-AAC v1/v2 encoding through `fdk-aac-sys`; the high-level crate disables
//! the SBR mode required by both profiles.

pub(crate) mod aac_he;
