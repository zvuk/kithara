//! HE-AAC v2 encoding through `fdk-aac-sys`. The high-level
//! `fdk-aac` 0.8.0 crate hardcodes `SBR_MODE=0` in its
//! `Encoder::new`, which prevents HE-AAC v1/v2 output — both
//! profiles require SBR. We talk to the C library directly here so
//! tests can produce production-shape AAC v2 fragments without
//! depending on a system `ffmpeg` build with `libfdk_aac` linked
//! in.

pub(crate) mod aac_he_v2;
