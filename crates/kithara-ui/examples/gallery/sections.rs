use std::sync::LazyLock;

use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    source::UiConfig,
    view::{PageStanding, ViewState},
};

use crate::fixture;

/// What a page's nav item, the screen's tabs and a photograph of it call it.
pub(super) type Page = &'static str;

/// The page whose demos the modules list offers.
pub(super) const MODULES: Page = "modules";
/// The state the nav turns. It is named at the top of the screen, so the nav
/// item in one module turns the tabs in another.
pub(super) const PAGE: &str = "page";
/// The state the modules page turns between its demos.
pub(super) const MODULE: &str = "module";

/// What the gallery's package calls the screen this module reads.
mod consts {
    /// The one screen the package declares, which every page lives in.
    pub(super) const SCREEN: &str = "gallery";
}

/// The file the package puts behind the gallery's screen.
pub(super) fn entry() -> &'static str {
    fixture::document(consts::SCREEN)
}

/// The page the gallery opens on, which is the one its screen calls initial.
pub(super) fn first() -> Page {
    declared().first
}

/// Every page the nav lists, in the order the screen offers them.
pub(super) fn pages() -> &'static [Page] {
    &declared().pages
}

/// The demos the modules page offers.
pub(super) fn modules() -> &'static [Page] {
    &declared().modules
}

/// The page named `slug`, or nothing when the screen offers no such page.
pub(super) fn named(slug: &str) -> Option<Page> {
    pages().iter().copied().find(|page| *page == slug)
}

/// What the shipped screen says about its own pages.
struct Declared {
    first: Page,
    modules: Vec<Page>,
    pages: Vec<Page>,
}

/// The pages the screen offers, read off the screen itself.
///
/// A screen compiles the page it stands at and no other, so opening it names
/// every page while building only the first, and standing it at the modules
/// page names that page's demos while building only the demo it opens on.
///
/// # Panics
/// Panics when the shipped screen does not compile or offers no pages, which
/// is a broken checkout rather than a runtime condition.
fn declared() -> &'static Declared {
    static DECLARED: LazyLock<Declared> = LazyLock::new(|| {
        let opened = screen(&ViewState::default());
        let mut at = ViewState::default();
        at.stand(PAGE, MODULES);
        Declared {
            first: leak(standing(&opened, PAGE).initial.clone()),
            modules: offered(&screen(&at), MODULE),
            pages: offered(&opened, PAGE),
        }
    });

    &DECLARED
}

/// The gallery's screen as it stands for `view`.
fn screen(view: &ViewState) -> CompiledUi {
    compile(
        entry(),
        &fixture::resolver(),
        &crate::demo::registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        view,
    )
    .unwrap_or_else(|error| panic!("the gallery screen must compile: {error}"))
}

fn offered(ui: &CompiledUi, state: &str) -> Vec<Page> {
    standing(ui, state)
        .offered
        .iter()
        .cloned()
        .map(leak)
        .collect()
}

fn standing<'a>(ui: &'a CompiledUi, state: &str) -> &'a PageStanding {
    ui.views()
        .pages()
        .get(state)
        .unwrap_or_else(|| panic!("the gallery screen turns a state {state}"))
}

/// The page list is read once and lives as long as the program, which is what
/// lets a page stay the name every harness passes around by value.
fn leak(page: String) -> Page {
    page.leak()
}
