//! The contract between the application's library shell and the sources it
//! mounts: a source's branch and rows, its page, and how the shell builds it
//! from its configuration.
#![forbid(unsafe_code)]

mod access;
mod context;
mod page;
mod playable;
mod secrets;
mod source;

pub use access::{AccessToken, KeyAccess};
pub use context::{
    Cause, Context, Environment, Factory, OpenUrlError, RegisterError, SectionError,
};
pub use page::{Document, Endpoint, Registration, SourcePage};
pub use playable::{InvalidPlayable, Playable, PlaybackChoice};
pub use secrets::{SecretError, Secrets};
pub use source::{BranchNode, LibrarySource, PAGES, PageStatus, SECTIONS, worded};
