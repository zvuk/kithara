//! The one stage that maps source time onto output time.
//!
//! Separate from `effects` on purpose. An EQ or a limiter transforms the
//! signal and leaves the time axis where it was; this stage decides which
//! source span becomes which output span, which is a different kind of thing
//! wearing the same pipeline-stage trait. `AudioEffect::held_source_frames`
//! already names the distinction — an effect that buffers behind a
//! duration-changing stage cannot express its hold at all.
//!
//! Both forms live here beside the choice between them, because they are
//! exclusive by construction and a deck is timed by exactly one:
//!
//! - [`streaming`] is push-driven. Live speed and key-lock drive the
//!   processor, and the output span is whatever the backend renders.
//! - [`bound`] is its inverse. The producer chooses the output span and the
//!   session's grid determines the source span, which is the only way a
//!   marker lands on a stamped frame.

/// The bound form needs a compiled exact-span engine, which no wasm target
/// carries.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub mod bound;
mod slot;
pub mod streaming;

pub use slot::{TempoSlot, TempoSlotError};
pub use streaming::StretchControls;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use streaming::{StretchBackend, StretchBackendError};
