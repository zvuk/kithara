use iced::Element;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    render::{Walk, tree},
    source::{MemResolver, UiConfig},
};

use super::{
    cache::{DeckLayout, StudioCache},
    endpoints::StudioRegistry,
};
use crate::gui::{app::Kithara, message::Message, studio_reads::StudioRoot};

const DOCS: &[(&str, &str)] = &[
    (
        "studio.klayout.ron",
        include_str!("../../../assets/ui/studio.klayout.ron"),
    ),
    (
        "studio-single.klayout.ron",
        include_str!("../../../assets/ui/studio-single.klayout.ron"),
    ),
    (
        "modules/studio-bar.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-bar.kmodule.ron"),
    ),
    (
        "modules/studio-deck.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-deck.kmodule.ron"),
    ),
    (
        "modules/studio-overview.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview.kmodule.ron"),
    ),
    (
        "modules/studio-overview-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview-single.kmodule.ron"),
    ),
    (
        "modules/studio-overview-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-overview-row.kmodule.ron"),
    ),
    (
        "modules/studio-mixer.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-mixer.kmodule.ron"),
    ),
    (
        "modules/studio-mixer-single.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-mixer-single.kmodule.ron"),
    ),
    (
        "modules/studio-select-row.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-select-row.kmodule.ron"),
    ),
    (
        "modules/studio-strip.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip.kmodule.ron"),
    ),
    (
        "modules/studio-strip/eq-3-band.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip/eq-3-band.kmodule.ron"),
    ),
    (
        "modules/studio-strip/eq-4-band.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip/eq-4-band.kmodule.ron"),
    ),
];

/// The compiled studio UI plus the host-owned view state it reads back. Both
/// deck layouts are compiled once; the top bar picks which one renders.
pub(crate) struct StudioUi {
    pub(crate) cache: StudioCache,
    dual: CompiledUi,
    single: CompiledUi,
}

impl StudioUi {
    pub(crate) fn new() -> Result<Self, UiDocError> {
        Ok(Self {
            single: compile_studio(DeckLayout::Single)?,
            dual: compile_studio(DeckLayout::Dual)?,
            cache: StudioCache::default(),
        })
    }

    fn compiled(&self, layout: DeckLayout) -> &CompiledUi {
        match layout {
            DeckLayout::Single => &self.single,
            DeckLayout::Dual => &self.dual,
        }
    }
}

/// Where the studio's own documents are read from: the built-in library with
/// this application's layouts and modules laid over it.
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
        DeckLayout::Single => "studio-single.klayout.ron",
        DeckLayout::Dual => "studio.klayout.ron",
    }
}

pub(super) fn compile_studio(layout: DeckLayout) -> Result<CompiledUi, UiDocError> {
    let resolver = resolver();
    compile(
        entry(layout),
        &resolver,
        &StudioRegistry::default(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let root = StudioRoot::new(state);
    let reads = Walk::new(&root);
    let compiled = state.studio.compiled(state.studio.cache.layout());
    tree::render(&compiled.root, compiled, &reads, builtin::skin()).map(Message::Ui)
}
