//! Toolkit-neutral input and the recognizers that turn it into control
//! gestures.
//!
//! Every host decodes its toolkit's events into [`Input`] and hands a control
//! the event with a [`Hit`] for the box it was laid out into; the control's
//! recognizer answers with an [`Outcome`]. The `iced` and `masonry` modules are
//! the decoders for those two toolkits.

mod cursor;
#[cfg(feature = "iced")]
pub mod iced;
mod input;
#[cfg(feature = "masonry")]
pub mod masonry;
mod modifiers;
mod outcome;
mod pointer;
pub mod recognizers;
mod text_input;

pub use cursor::{CursorShape, Hover};
pub use input::{Hit, Input, InputMethod, Key, Scroll, ScrollAxis};
pub use modifiers::Modifiers;
pub use outcome::{Outcome, PointerOwnership, Propagation};
pub use pointer::{MOUSE, PointerButton, PointerId, PointerInput, PointerPhase, mouse};
pub use text_input::{InputMethodRequest, PreeditRef, TextInputLayout};
