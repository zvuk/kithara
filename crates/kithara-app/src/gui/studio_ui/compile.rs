use iced::Element;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::tree,
    source::UiConfig,
};

use super::{
    cache::{DeckLayout, StudioCache},
    endpoints::StudioRegistry,
    reads::StudioReads,
};
use crate::gui::{app::Kithara, message::Message};

const COMPILES: &str = "embedded studio documents must compile";

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
        "modules/studio-strip.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip.kmodule.ron"),
    ),
    (
        "modules/studio-library.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-library.kmodule.ron"),
    ),
];

/// The compiled studio UI plus the host-owned view state it reads back. Both
/// deck layouts are compiled once; the top bar picks which one renders.
pub(crate) struct StudioUi {
    single: CompiledUi,
    dual: CompiledUi,
    pub(crate) cache: StudioCache,
}

impl StudioUi {
    /// Compile the embedded studio documents. Panicking here is sanctioned:
    /// the documents are compile-time assets validated by unit tests, so a
    /// failure is a build defect, not a runtime condition.
    pub(crate) fn new() -> Self {
        Self {
            single: compile_studio(DeckLayout::Single).expect(COMPILES),
            dual: compile_studio(DeckLayout::Dual).expect(COMPILES),
            cache: StudioCache::default(),
        }
    }

    fn compiled(&self, layout: DeckLayout) -> &CompiledUi {
        match layout {
            DeckLayout::Single => &self.single,
            DeckLayout::Dual => &self.dual,
        }
    }
}

pub(super) fn compile_studio(
    layout: DeckLayout,
) -> Result<CompiledUi, kithara_ui::error::UiDocError> {
    let mut resolver = builtin::resolver();
    for (path, text) in DOCS {
        resolver.insert(path, text);
    }
    let entry = match layout {
        DeckLayout::Single => "studio-single.klayout.ron",
        DeckLayout::Dual => "studio.klayout.ron",
    };
    compile(
        entry,
        &resolver,
        &StudioRegistry::new(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let reads = StudioReads::new(state);
    let compiled = state.studio.compiled(state.studio.cache.layout());
    tree::render(&compiled.root, compiled, &reads, builtin::skin()).map(Message::Ui)
}
