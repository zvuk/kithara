use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use kithara_ui::{
    builtin,
    ids::ScreenRole,
    package::load_package,
    source::{FileResolver, FillDocument, MemResolver, OverlayResolver},
};

use crate::custom;

pub mod consts {
    pub const HEIGHT: f32 = 720.0;
    /// The smallest window the gallery opens to, so a page can be dragged
    /// down to the room its adaptive and revealed cells answer.
    pub const MIN_HEIGHT: f32 = 320.0;
    pub const MIN_WIDTH: f32 = 400.0;
    /// The scale a photograph is taken at unless a run asks for another.
    pub const SCALE: f32 = 1.0;
    pub const STRESS_TICK_MS: u64 = 16;
    pub const WIDTH: f32 = 1300.0;
    /// Collection used by the fill demo.
    pub const FILLED: &str = "gallery-fill/items";
    pub const FILL: &str = "modules/tabs/fill/caption.kmodule.ron";
}

/// The gallery's documents on disk, laid over the ones this build embeds.
pub type Resolver = OverlayResolver<FileResolver, MemResolver>;

/// Where the gallery's own documents live, so editing one and opening the
/// gallery again shows the edit.
#[must_use]
pub fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Gallery documents from disk over the embedded library, with two demo fills.
///
/// # Panics
/// Panics when the folder the gallery ships its documents in cannot be read.
#[must_use]
pub fn resolver() -> Resolver {
    let files = FileResolver::new(package_root()).expect("the gallery ships its own documents");
    let mut library = builtin::resolver();
    for key in ["first", "second"] {
        library.fill(
            consts::FILLED,
            key,
            FillDocument::Path(consts::FILL.to_owned()),
        );
    }
    OverlayResolver::new(files, library)
}

/// The file the gallery's package puts behind `role`.
///
/// A page states which screen it is; the manifest states which file that
/// screen lives in. Keeping the mapping in the package is what lets a page be
/// renamed, or replaced by another file, without touching this example.
///
/// # Panics
/// Panics when the package answers for no such role.
#[must_use]
pub fn document(role: &str) -> &'static str {
    pages()
        .get(role)
        .unwrap_or_else(|| panic!("the gallery package answers for no screen {role}"))
}

/// Every role the gallery's package declares, and the file behind each.
///
/// Each file is read once here and checked against the role the manifest put
/// it behind, so a manifest that names the wrong file is refused where it is
/// read rather than drawn as the wrong page.
///
/// # Panics
/// Panics when the shipped manifest is unreadable or disagrees with a
/// document, which is a broken checkout rather than a runtime condition.
#[must_use]
pub fn pages() -> &'static BTreeMap<ScreenRole, String> {
    static PAGES: LazyLock<BTreeMap<ScreenRole, String>> = LazyLock::new(|| {
        let resolver = resolver();
        let package = load_package(&resolver, "package.kpackage.ron", &custom::config().limits)
            .unwrap_or_else(|error| panic!("the gallery ships a package it fills: {error}"));
        package
            .screens
            .keys()
            .map(|role| {
                let file = package.screen(&resolver, role).unwrap_or_else(|error| {
                    panic!("the gallery package must answer for {role}: {error}")
                });
                (role.clone(), file)
            })
            .collect()
    });

    &PAGES
}
