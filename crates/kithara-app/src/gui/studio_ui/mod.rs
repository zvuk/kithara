mod cache;
mod endpoints;
mod events;
mod reads;

use iced::Element;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::tree,
    source::UiConfig,
};

pub(crate) use self::{cache::StudioCache, events::translate};
use self::reads::StudioReads;
use super::{app::Kithara, message::Message};

const DOCS: &[(&str, &str)] = &[
    (
        "studio.klayout.ron",
        include_str!("../../../assets/ui/studio.klayout.ron"),
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
        "modules/studio-mixer.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-mixer.kmodule.ron"),
    ),
    (
        "modules/studio-library.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-library.kmodule.ron"),
    ),
];

/// The compiled studio UI plus the host-owned view state it reads back.
pub(crate) struct StudioUi {
    compiled: CompiledUi,
    pub(crate) cache: StudioCache,
}

impl StudioUi {
    /// Compile the embedded studio documents. Panicking here is sanctioned:
    /// the documents are compile-time assets validated by unit tests, so a
    /// failure is a build defect, not a runtime condition.
    pub(crate) fn new() -> Self {
        Self {
            compiled: compile_studio().expect("embedded studio documents must compile"),
            cache: StudioCache::default(),
        }
    }
}

fn compile_studio() -> Result<CompiledUi, kithara_ui::error::UiDocError> {
    let mut resolver = builtin::resolver();
    for (path, text) in DOCS {
        resolver.insert(path, text);
    }
    compile(
        "studio.klayout.ron",
        &resolver,
        &endpoints::StudioRegistry::new(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let reads = StudioReads::new(state);
    tree::render(
        &state.studio.compiled.root,
        &state.studio.compiled,
        &reads,
        builtin::skin(),
    )
    .map(Message::Ui)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn studio_documents_compile_against_the_registry() {
        compile_studio().unwrap();
    }
}
