use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use kithara_ui::{
    builtin,
    ids::ScreenRole,
    package::load_package,
    source::{FileResolver, MemResolver, OverlayResolver},
};

pub(crate) mod consts {
    pub(crate) const HEIGHT: f32 = 720.0;
    /// The smallest window the gallery opens to, so a page can be dragged
    /// down to the room its adaptive and revealed cells answer.
    pub(crate) const MIN_HEIGHT: f32 = 320.0;
    pub(crate) const MIN_WIDTH: f32 = 400.0;
    /// The scale a photograph is taken at unless a run asks for another.
    pub(crate) const SCALE: f32 = 1.0;
    pub(crate) const STRESS_TICK_MS: u64 = 16;
    pub(crate) const WIDTH: f32 = 1300.0;
}

/// The gallery's documents on disk, laid over the ones this build embeds.
pub(crate) type Resolver = OverlayResolver<FileResolver, MemResolver>;

/// Where the gallery's own documents live, so editing one and opening the
/// gallery again shows the edit.
pub(crate) fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/gallery/assets")
}

/// The gallery reads its pages from the folder it ships them in, over the
/// built-in library.
///
/// Nothing about a page is embedded: the folder is part of this checkout, so
/// editing a document and opening the gallery again shows the edit, and a
/// folder that cannot be read is a broken checkout rather than a runtime
/// condition. The library underneath is embedded, because a consumer of the
/// toolkit has no checkout to read it from.
pub(crate) fn resolver() -> Resolver {
    let files = FileResolver::new(package_root()).expect("the gallery ships its own documents");
    OverlayResolver::new(files, builtin::resolver())
}

/// The file the gallery's package puts behind `role`.
///
/// A page states which screen it is; the manifest states which file that
/// screen lives in. Keeping the mapping in the package is what lets a page be
/// renamed, or replaced by another file, without touching this example.
///
/// # Panics
/// Panics when the package answers for no such role.
pub(crate) fn document(role: &str) -> &'static str {
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
pub(crate) fn pages() -> &'static BTreeMap<ScreenRole, String> {
    static PAGES: LazyLock<BTreeMap<ScreenRole, String>> = LazyLock::new(|| {
        let resolver = resolver();
        let package = load_package(&resolver, "package.kpackage.ron")
            .unwrap_or_else(|error| panic!("the gallery ships a package manifest: {error}"));
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
