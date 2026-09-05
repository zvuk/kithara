use thiserror::Error;

use crate::{
    error::UiDocError,
    registry::EndpointRegistry,
    render::{Reads, Skin, UiEvent, custom::CustomKinds},
    source::{SourceResolver, UiConfig},
    text::TextDoc,
    view::ViewState,
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

    /// The skin to paint the state the application is in now.
    ///
    /// A skin is the application's, not the host's: which one it wears is part
    /// of what it is showing, and it can turn to another one whenever it likes.
    /// The host follows that the same way it follows a change of document.
    fn skin(&self) -> &Skin;

    /// Advances anything that moves on its own. Called once per frame.
    fn tick(&mut self) {}

    /// Applies one event the document published.
    fn update(&mut self, event: UiEvent);

    /// Told what the document turned for itself, after every turn.
    ///
    /// The host owns the screen's own state, so an application that feeds the
    /// page on screen has no other way to learn which page that is. Reading
    /// it is all this is for: writing it back is the host's business.
    fn turned(&mut self, view: &ViewState) {
        let _ = view;
    }
}

/// Everything a host needs besides the application itself.
#[derive(bon::Builder, Clone, Copy)]
#[non_exhaustive]
pub struct Config<'a> {
    /// The caption catalog every `@key` in the document resolves against.
    pub text: &'a TextDoc,
    /// Which endpoints the document may bind to.
    pub endpoints: &'a dyn EndpointRegistry,
    /// Where document sources are read from.
    pub resolver: &'a dyn SourceResolver,
    /// Window title. Ignored by a host that does not own a window.
    #[builder(default = "")]
    pub title: &'a str,
    /// The extensions this application registers, which is what a `Custom`
    /// control resolves its kind against. The same set is what the document is
    /// compiled against, so a kind nothing registered is refused there rather
    /// than mounted as a blank box.
    pub kinds: Option<&'a CustomKinds>,
    /// The configuration this host compiles its document against. Absent
    /// defaults to [`UiConfig::default`]. `custom_kinds` on the value here is
    /// ignored either way: [`Ui::new`](super::Ui::new) always overwrites it
    /// from [`Self::kinds`], because registering an extension kind is code's
    /// business, not a passed configuration's.
    pub settings: Option<&'a UiConfig>,
    /// Smallest window the document is laid out for, in logical points.
    /// Ignored by a host that does not own a window.
    pub min_size: Option<(u32, u32)>,
    /// Whether the system draws the window frame. A document that carries its
    /// own title bar, drag region and window buttons wants this off, or the two
    /// frames are drawn one inside the other.
    ///
    /// Ignored by a host that does not own a window.
    #[builder(default = true)]
    pub decorations: bool,
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
