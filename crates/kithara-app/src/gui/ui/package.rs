use std::{path::Path, rc::Rc};

use kithara::ui::{
    builtin,
    error::UiDocError,
    ids::{ScreenRole, SourceUri},
    package::{PackageDoc, load_package},
    render::Skin,
    skin::load_skin,
    source::{Limits, MemResolver, OverlayResolver, SourceResolver},
    text::{TextDoc, parse_text},
};

use super::{cache::DeckLayout, endpoints::Registry};
use crate::gui::library::SourceAdditions;

include!(concat!(env!("OUT_DIR"), "/ui_documents.rs"));

/// What this application asks a package for, one role per deck arrangement.
///
/// The package decides which file stands behind each, so a package may rename
/// its screens without this having to know.
fn role(layout: DeckLayout) -> ScreenRole {
    ScreenRole(
        match layout {
            DeckLayout::Single => "deck-single",
            DeckLayout::Dual => "deck-dual",
        }
        .to_owned(),
    )
}

/// One loaded UI package: the documents it is read through, the screens it
/// answered for, and the skin and catalog it dresses them in.
///
/// Everything a host needs to draw this application comes from here, so the
/// window and the pages it shows are never read through two different packages.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Package {
    resolver: Box<dyn SourceResolver>,
    #[field(get, vis = "pub(in crate::gui)")]
    registry: Registry,
    screens: Screens,
    /// The skin this package dresses its pages in, resolved once.
    #[field(get, vis = "pub(in crate::gui)")]
    skin: Skin,
    /// The captions every `@key` in this package's documents resolves against.
    #[field(get, vis = "pub(in crate::gui)")]
    text: TextDoc,
}

impl Package {
    /// The manifest naming everything this application asks a package for.
    const MANIFEST: &'static str = "package.kpackage.ron";
    /// The writes every screen must declare.
    pub(crate) const REQUIRED: &'static [&'static str] = &[
        "deck.transport.toggle_play@deck=a",
        "deck.transport.seek_normalized@deck=a",
    ];

    pub(in crate::gui) fn document(&self, layout: DeckLayout) -> &str {
        self.screens.document(layout)
    }

    pub(in crate::gui) fn load(
        root: Option<&Path>,
        pages: SourceAdditions,
        limits: &Limits,
    ) -> Result<Rc<Self>, UiDocError> {
        match root {
            Some(root) => Self::read_folder(root, pages, limits),
            None => Self::read(embedded(), pages, limits),
        }
    }

    fn read<R: SourceResolver + 'static>(
        documents: R,
        pages: SourceAdditions,
        limits: &Limits,
    ) -> Result<Rc<Self>, UiDocError> {
        let SourceAdditions {
            fills,
            registry,
            texts,
        } = pages;
        let resolver: Box<dyn SourceResolver> = Box::new(OverlayResolver::new(fills, documents));
        let manifest = load_package(resolver.as_ref(), Self::MANIFEST, limits)?;
        let screens = Screens::resolve(&manifest, resolver.as_ref())?;
        let mut text = catalog(resolver.as_ref(), &manifest)?;
        for document in texts {
            let origin = SourceUri(document.path.to_owned());
            text = text.merge(&parse_text(document.text, &origin)?, &origin)?;
        }
        let skin = dress(resolver.as_ref(), &manifest, &text)?;
        Ok(Rc::new(Self {
            resolver,
            registry,
            screens,
            skin,
            text,
        }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_folder(
        root: &Path,
        pages: SourceAdditions,
        limits: &Limits,
    ) -> Result<Rc<Self>, UiDocError> {
        use kithara::ui::source::FileResolver;

        if !root.exists() {
            return Self::read(embedded(), pages, limits);
        }
        let files = FileResolver::new(root).map_err(|error| UiDocError::Unreadable {
            origin: SourceUri(root.display().to_string()),
            rel: String::new(),
            source: error,
        })?;
        Self::read(OverlayResolver::new(files, embedded()), pages, limits)
    }

    #[cfg(target_arch = "wasm32")]
    fn read_folder(
        root: &Path,
        _pages: SourceAdditions,
        _limits: &Limits,
    ) -> Result<Rc<Self>, UiDocError> {
        use std::io::ErrorKind;

        Err(UiDocError::Unreadable {
            origin: SourceUri(root.display().to_string()),
            rel: String::new(),
            source: ErrorKind::Unsupported.into(),
        })
    }

    pub(in crate::gui) fn resolver(&self) -> &dyn SourceResolver {
        self.resolver.as_ref()
    }
}

/// The screen this application asks a package for, one per deck arrangement.
///
/// Both are resolved once, when the package is read, so a host that has to
/// name the document it draws can hand back a name the package already
/// answered for.
struct Screens {
    dual: String,
    single: String,
}

impl Screens {
    fn document(&self, layout: DeckLayout) -> &str {
        match layout {
            DeckLayout::Single => &self.single,
            DeckLayout::Dual => &self.dual,
        }
    }

    fn resolve(manifest: &PackageDoc, resolver: &dyn SourceResolver) -> Result<Self, UiDocError> {
        Ok(Self {
            dual: manifest.screen(resolver, &role(DeckLayout::Dual))?,
            single: manifest.screen(resolver, &role(DeckLayout::Single))?,
        })
    }
}

/// The caption catalog the documents resolve `@key` against: the built-in one
/// with the entries the package names laid over it.
///
/// A package that names no catalog of its own draws with the built-in words.
fn catalog(resolver: &dyn SourceResolver, manifest: &PackageDoc) -> Result<TextDoc, UiDocError> {
    let Some(rel) = manifest.text.as_deref() else {
        return Ok(builtin::text_doc().clone());
    };
    let loaded = resolver.load(None, rel)?;
    let extra = parse_text(&loaded.text, &loaded.uri)?;
    builtin::text_doc().merge(&extra, &loaded.uri)
}

/// The skin the package names, resolved against that same package: a package
/// carrying its own skin document dresses every page it ships.
///
/// A package that names no skin of its own wears the built-in one.
fn dress(
    resolver: &dyn SourceResolver,
    manifest: &PackageDoc,
    text: &TextDoc,
) -> Result<Skin, UiDocError> {
    let Some(rel) = manifest.skin.as_deref() else {
        return Ok(builtin::skin().clone());
    };
    let document = load_skin(resolver, rel, &Limits::default())?;
    Skin::resolve(document, text, &SourceUri(rel.to_owned()), resolver)
}

/// Where this application's own documents are read from when nothing is laid
/// out on disk: the built-in library with this build's own documents over it.
pub(crate) fn embedded() -> MemResolver {
    let mut resolver = builtin::resolver();
    for (path, text) in DOCS {
        resolver.insert(path, text);
    }
    resolver
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::gui::test_fixture;

    mod consts {
        /// The toolkit documents this build ships its own version of.
        ///
        /// The application's own documents are read over the toolkit's, so one
        /// standing at a path the toolkit also ships replaces it. These two do so
        /// deliberately: the toolkit's menu commands `ui.window.focus`,
        /// `ui.window.open`, `ui.window.close`, `ui.window.cycle_display`,
        /// `ui.window.hidden`, `ui.window.can_open`, `ui.settings.open` and
        /// `ui.modules.title`, which this application answers on nowhere, so
        /// drawing it here would be refused. Everything else the menu is built
        /// from - its module cell, layout row, toggle row and hint row - is
        /// taken from the toolkit rather than copied.
        pub(super) const REPLACED: [&str; 2] = [
            "modules/app-menu.kmodule.ron",
            "modules/app-menu/window-row.kmodule.ron",
        ];
    }

    /// A document copied into this application under a name the toolkit
    /// already ships takes that name over without saying so, and then drifts
    /// where nobody is looking. Only the two above may.
    #[kithara::test]
    fn this_build_replaces_only_the_toolkit_documents_it_means_to() {
        let toolkit = builtin::resolver();

        let replaced: Vec<&str> = DOCS
            .iter()
            .map(|(path, _)| *path)
            .filter(|path| toolkit.load(None, path).is_ok())
            .collect();

        assert_eq!(replaced, consts::REPLACED);
    }

    #[kithara::test]
    fn no_folder_reads_the_documents_this_build_carries() {
        let package = test_fixture::package(None).expect("the embedded package must load");

        for (path, text) in DOCS {
            let loaded = package
                .resolver()
                .load(None, path)
                .unwrap_or_else(|error| panic!("`{path}` must be readable: {error}"));
            assert_eq!(loaded.text, *text, "`{path}` must be the embedded copy");
        }
    }
}
