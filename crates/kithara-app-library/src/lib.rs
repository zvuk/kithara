//! The contract between the application's library shell and the sources it
//! mounts: a source's branch and rows, its page, and how the shell builds it
//! from its configuration.
#![forbid(unsafe_code)]

mod context;
mod page;
mod playable;
mod source;

pub use context::{Cause, Context, Environment, Factory, RegisterError, SectionError};
pub use page::{Document, Endpoint, Registration, SourcePage};
pub use playable::{NoSource, Playable};
pub use source::{BranchNode, LibrarySource, PAGES, PageStatus, worded};
