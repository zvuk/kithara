use iced::{Element, Size};
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    ids::SourceUri,
    render::{Walk, tree},
    source::UiConfig,
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
        "modules/app-bar-micro.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-bar-micro.kmodule.ron"),
    ),
    (
        "modules/app-micro.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-micro.kmodule.ron"),
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
        "modules/app-strip/eq-mode-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/app-strip/eq-mode-row.kmodule.ron"),
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
    dual: CompiledUi,
    single: CompiledUi,
}

impl AppUi {
    pub(crate) fn new() -> Result<Self, UiDocError> {
        Ok(Self {
            single: compile_ui(DeckLayout::Single)?,
            dual: compile_ui(DeckLayout::Dual)?,
            cache: ViewCache::default(),
        })
    }

    /// One window draws whichever layout the menu picks, so it holds the room
    /// the more demanding of the two asks for.
    pub(crate) fn window_min(&self) -> Size {
        Size::new(
            self.single.min.w.min().max(self.dual.min.w.min()),
            self.single.min.h.min().max(self.dual.min.h.min()),
        )
    }

    const fn compiled(&self, layout: DeckLayout) -> &CompiledUi {
        match layout {
            DeckLayout::Single => &self.single,
            DeckLayout::Dual => &self.dual,
        }
    }
}

pub(super) fn compile_ui(layout: DeckLayout) -> Result<CompiledUi, UiDocError> {
    let mut resolver = builtin::resolver();
    for (path, text) in DOCS {
        resolver.insert(path, text);
    }
    let text = app_text()?;
    let entry = match layout {
        DeckLayout::Single => "app-single.klayout.ron",
        DeckLayout::Dual => "app.klayout.ron",
    };
    compile(
        entry,
        &resolver,
        &Registry::default(),
        builtin::skin_doc(),
        &text,
        &UiConfig::default(),
    )
}

fn app_text() -> Result<TextDoc, UiDocError> {
    let origin = SourceUri("app-en.ktext.ron".to_owned());
    let extra = parse_text(include_str!("../../../assets/ui/app-en.ktext.ron"), &origin)?;
    builtin::text_doc().merge(&extra, &origin)
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let root = ReadRoot::new(state);
    let reads = Walk::new(&root);
    let compiled = state.ui.compiled(state.ui.cache.layout());
    tree::render(&compiled.root, compiled, &reads, builtin::skin()).map(Message::Ui)
}
