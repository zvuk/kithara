//! Reads the Lottie artwork kithara-ui ships and draws a frame of it into the
//! toolkit-neutral draw list.

mod artwork;
mod consts;
mod emit;
mod error;
mod validate;

pub use artwork::{Artwork, builtin_artwork};
pub use emit::emit;
pub use error::{ArtworkError, LottieError};
