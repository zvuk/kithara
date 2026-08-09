mod encoder;
#[cfg(test)]
mod tests;

pub(crate) use self::encoder::{AacStream, StreamParams};
pub use self::encoder::{StreamBackend, StreamEncoder};
