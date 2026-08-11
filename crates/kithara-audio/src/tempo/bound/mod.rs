mod core;
mod error;
#[cfg(all(test, feature = "stretch-signalsmith"))]
mod mixing;
mod slot;
#[cfg(all(test, feature = "stretch-signalsmith"))]
mod tests;

pub(crate) use core::BoundRenderer;

pub(crate) use error::BoundError;
#[cfg(test)]
pub(crate) use slot::bound_slot;
pub(crate) use slot::{rate_supported, render_span_frames, resident_slot};
