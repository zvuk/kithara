pub(crate) mod core;
pub(crate) mod event;
pub(crate) mod format;
pub(crate) mod generation;
mod generation_holdback;
#[cfg(test)]
#[path = "../tests/origin.rs"]
mod generation_origin_tests;
pub(crate) mod output;
pub(crate) mod resume;
pub(crate) mod step;
pub(crate) mod transition;
pub(crate) mod transition_promotion;

pub(crate) use generation::DecoderGeneration;
