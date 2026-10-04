#![forbid(unsafe_code)]

mod inner;
mod peer;
mod reader;
mod segments;
mod source;
#[cfg(test)]
mod tests;

pub(crate) use inner::{FileAssetCtx, FileInner, FileSourceCtx, sniff_media_info};
pub(crate) use peer::FilePeer;
pub(crate) use source::{FileLocalConfig, FileSource};
