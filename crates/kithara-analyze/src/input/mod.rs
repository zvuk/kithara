mod parse;

pub use parse::{Args, Batch};
pub(crate) use parse::{Artifact, OutputFormat, Track, source};

#[cfg(test)]
mod tests;
