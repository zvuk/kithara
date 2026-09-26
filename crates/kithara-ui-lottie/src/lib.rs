//! Reads the Lottie artwork kithara-ui ships and draws a frame of it into the
//! toolkit-neutral draw list.

mod artwork;
mod emit;
mod error;

pub use artwork::{Artwork, builtin_artwork};
pub use emit::emit;
pub use error::LottieError;
