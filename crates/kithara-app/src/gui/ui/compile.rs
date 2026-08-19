use iced::Element;
use kithara_platform::time::Duration;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    ids::SourceUri,
    render::{Clock, Walk, tree},
    source::{MemResolver, UiConfig},
    text::{TextDoc, parse_text},
};

use super::{
    cache::{DeckLayout, ViewCache},
    endpoints::Registry,
};
use crate::gui::{app::Kithara, message::Message, reads::ReadRoot};

const DOCS: &[(&str, &str)] = &[
    (
        "app.klayout.ron",
        include_str!("../../../assets/ui/app.klayout.ron"),
    ),
    (
        "app-single.klayout.ron",
        include_str!("../../../assets/ui/app-single.klayout.ron"),
    ),
    (
        "modules/app-bar.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-bar.kmodule.ron"),
    ),
    (
        "modules/app-menu.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-menu.kmodule.ron"),
    ),
    (
        "modules/app-menu/window-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-menu/window-row.kmodule.ron"),
    ),
    (
        "modules/app-menu/module-cell.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-menu/module-cell.kmodule.ron"),
    ),
    (
        "modules/app-deck.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-deck.kmodule.ron"),
    ),
    (
        "modules/app-overview.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-overview.kmodule.ron"),
    ),
    (
        "modules/app-overview-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-overview-single.kmodule.ron"),
    ),
    (
        "modules/app-overview-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-overview-row.kmodule.ron"),
    ),
    (
        "modules/app-mixer.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-mixer.kmodule.ron"),
    ),
    (
        "modules/app-mixer-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-mixer-single.kmodule.ron"),
    ),
    (
        "modules/app-select-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-select-row.kmodule.ron"),
    ),
    (
        "modules/app-strip.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-strip.kmodule.ron"),
    ),
    (
        "modules/app-strip/eq-3-band.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-strip/eq-3-band.kmodule.ron"),
    ),
    (
        "modules/app-strip/eq-4-band.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-strip/eq-4-band.kmodule.ron"),
    ),
    (
        "modules/app-library.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-library.kmodule.ron"),
    ),
];

/// The compiled UI plus the host-owned view state it reads back. Both
/// deck layouts are compiled once; the top bar picks which one renders.
pub(crate) struct AppUi {
    pub(crate) cache: ViewCache,
    /// This host's own reading of time, advanced once per tick so a document
    /// bound to it animates without the application keeping a timer of its own.
    clock: Clock,
    dual: CompiledUi,
    single: CompiledUi,
}

impl AppUi {
    pub(crate) fn new() -> Result<Self, UiDocError> {
        Ok(Self {
            single: compile_ui(DeckLayout::Single)?,
            dual: compile_ui(DeckLayout::Dual)?,
            cache: ViewCache::default(),
            clock: Clock::default(),
        })
    }

    /// Moves this host's clock on by one tick of `step`.
    pub(crate) fn advance(&mut self, step: Duration) {
        self.clock = self.clock.advance(step);
    }

    const fn compiled(&self, layout: DeckLayout) -> &CompiledUi {
        match layout {
            DeckLayout::Single => &self.single,
            DeckLayout::Dual => &self.dual,
        }
    }
}

/// Where this application's own documents are read from: the built-in library
/// with its layouts and modules laid over it.
pub(crate) fn resolver() -> MemResolver {
    let mut resolver = builtin::resolver();
    for (path, text) in DOCS {
        resolver.insert(path, text);
    }
    resolver
}

/// The layout entry for a deck arrangement, which is what a host compiles.
pub(crate) const fn entry(layout: DeckLayout) -> &'static str {
    match layout {
        DeckLayout::Single => "app-single.klayout.ron",
        DeckLayout::Dual => "app.klayout.ron",
    }
}

pub(in crate::gui) fn compile_ui(layout: DeckLayout) -> Result<CompiledUi, UiDocError> {
    compile(
        entry(layout),
        &resolver(),
        &Registry::default(),
        builtin::skin_doc(),
        &text()?,
        &UiConfig::default(),
    )
}

/// The caption catalog the documents resolve `@key` against: the built-in one
/// with this application's own entries laid over it.
pub(crate) fn text() -> Result<TextDoc, UiDocError> {
    let origin = SourceUri("app-en.ktext.ron".to_owned());
    let extra = parse_text(include_str!("../../../assets/ui/app-en.ktext.ron"), &origin)?;
    builtin::text_doc().merge(&extra, &origin)
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let root = ReadRoot::new(state);
    let reads = Walk::new(&root);
    let compiled = state.ui.compiled(state.ui.cache.layout());
    tree::render(
        &compiled.root,
        compiled,
        &reads,
        builtin::skin(),
        state.ui.clock,
    )
    .map(Message::Ui)
}
