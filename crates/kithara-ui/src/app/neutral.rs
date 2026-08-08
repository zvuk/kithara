use thiserror::Error;

use crate::{
    error::UiDocError,
    registry::EndpointRegistry,
    render::{Reads, Skin, UiEvent},
    skin::SkinDoc,
    source::SourceResolver,
};

/// What a host needs from an application to show it and keep it fed.
///
/// Nothing here names a toolkit. Which host picks this up is decided by the
/// feature the crate was built with, not by the application.
pub trait App {
    /// The layout entry to compile for the state the application is in now.
    fn document(&self) -> &str;

    /// Hands the host the values the document binds to, for as long as the
    /// call lasts.
    ///
    /// Scoped rather than returned because an application that answers its
    /// endpoints by walking its own state builds that walk on the stack: it
    /// borrows the application, so there is nothing inside the application to
    /// hand back a reference to.
    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R;

    /// Advances anything that moves on its own. Called once per frame.
    fn tick(&mut self) {}

    /// Applies one event the document published.
    fn update(&mut self, event: UiEvent);
}

/// Everything a host needs besides the application itself.
#[derive(bon::Builder, Clone, Copy)]
#[non_exhaustive]
pub struct Config<'a> {
    /// Which endpoints the document may bind to.
    pub endpoints: &'a dyn EndpointRegistry,
    /// Where document sources are read from.
    pub resolver: &'a dyn SourceResolver,
    /// The resolved skin the host paints with.
    pub skin: &'a Skin,
    /// The skin document, which the layout pass still consults.
    pub skin_doc: &'a SkinDoc,
    /// Whether the system draws the window frame. A document that carries its
    /// own title bar, drag region and window buttons wants this off, or the two
    /// frames are drawn one inside the other.
    ///
    /// Ignored by a host that does not own a window.
    #[builder(default = true)]
    pub decorations: bool,
    /// Smallest window the document is laid out for, in logical points.
    /// Ignored by a host that does not own a window.
    pub min_size: Option<(u32, u32)>,
    /// Window title. Ignored by a host that does not own a window.
    #[builder(default = "")]
    pub title: &'a str,
}

/// Why a UI could not be brought up or kept running.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunError {
    /// The document did not compile.
    #[error(transparent)]
    Document(#[from] UiDocError),
    /// The host could not mount the document, or could not keep running.
    #[error("{0}")]
    Host(String),
}
