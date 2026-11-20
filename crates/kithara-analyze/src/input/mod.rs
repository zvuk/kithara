mod parse;

pub use parse::{Args, Batch};
pub(crate) use parse::{Artifact, OutputFormat, Track};

#[cfg(test)]
mod tests;
