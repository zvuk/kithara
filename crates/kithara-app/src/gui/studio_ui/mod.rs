mod cache;
mod endpoints;
mod events;
mod reads;
mod scope;

use iced::Element;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::tree,
    source::UiConfig,
};

use self::reads::StudioReads;
pub(crate) use self::{cache::StudioCache, events::translate};
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
        "modules/studio-strip.kmodule.ron",
        include_str!("../../../assets/ui/modules/studio-strip.kmodule.ron"),
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
    use kithara_ui::{compile::CompiledNode, expand::ExpandedNode};

    use super::*;

    /// Every control in the compiled studio, as `(path, scoped binding keys)`.
    fn controls(ui: &CompiledUi) -> Vec<(&str, Vec<&str>)> {
        fn walk<'a>(
            node: &'a ExpandedNode,
            ui: &'a CompiledUi,
            out: &mut Vec<(&'a str, Vec<&'a str>)>,
        ) {
            match node {
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. } => {
                    for child in children {
                        walk(child, ui, out);
                    }
                }
                ExpandedNode::Control {
                    path, read, write, ..
                } => {
                    let keys = [read.as_ref(), write.as_ref()]
                        .into_iter()
                        .flatten()
                        .map(|binding| ui.resolve(binding.key()))
                        .collect();
                    out.push((ui.resolve(*path), keys));
                }
                _ => {}
            }
        }

        let mut out = Vec::new();
        let mut stack = vec![&ui.root];
        while let Some(node) = stack.pop() {
            match node {
                CompiledNode::Split { children, .. } => {
                    stack.extend(children.iter().map(|(_, child)| child));
                }
                CompiledNode::Module { root, .. } => walk(root, ui, &mut out),
                _ => {}
            }
        }
        out
    }

    #[kithara::test]
    fn studio_documents_compile_against_the_registry() {
        compile_studio().unwrap();
    }

    /// A deck-scoped control is routed by the letter in its path, and read by
    /// the letter in its binding scope. The two must be the same letter, or
    /// the control silently reads one deck and writes another.
    #[kithara::test]
    fn deck_scoped_controls_are_routed_to_the_deck_they_read() {
        let ui = compile_studio().unwrap();
        for (path, keys) in controls(&ui) {
            for key in keys {
                let Some(letter) = key
                    .split_once('@')
                    .and_then(|(_, scope)| scope.strip_prefix("deck="))
                else {
                    continue;
                };
                assert!(
                    path.starts_with(&format!("deck-{letter}/"))
                        || path.starts_with(&format!("mixer/{letter}/")),
                    "control `{path}` is bound to `{key}` but is not addressed by deck `{letter}`",
                );
            }
        }
    }

    #[kithara::test]
    fn every_channel_strip_carries_the_full_control_set() {
        let ui = compile_studio().unwrap();
        let paths: Vec<&str> = controls(&ui).into_iter().map(|(path, _)| path).collect();
        for letter in ["a", "b"] {
            for name in ["trim", "low", "mid", "high", "tempo", "mute"] {
                let want = format!("mixer/{letter}/{name}");
                assert!(paths.contains(&want.as_str()), "missing control `{want}`");
            }
        }
    }
}
