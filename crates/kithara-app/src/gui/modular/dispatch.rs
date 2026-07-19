use kithara_ui::{
    compile::{CompiledNode, CompiledUi},
    expand::{Binding, ExpandedNode},
    ids::InternId,
    render::ControlAction,
};
use num_traits::ToPrimitive;
use tracing::warn;

use crate::gui::{
    app::Kithara,
    update::{
        handle_next, handle_prev, handle_seek_to, handle_select_track, handle_toggle_play_pause,
        handle_volume_changed,
    },
};

#[derive(Debug)]
enum ResolvedWrite {
    Command { id: String, deck_is_a: bool },
    Parameter { id: String },
}

pub(super) fn apply(state: &mut Kithara, path: &str, action: &ControlAction) {
    let resolved = state.modular.compiled.as_ref().and_then(|compiled| {
        let (kind, write) = find_control(compiled, path)?;
        let kind = compiled.resolve(kind).to_owned();
        let write = write
            .as_ref()
            .and_then(|binding| resolve_write(binding, compiled));
        Some((kind, write))
    });
    let Some((kind, write)) = resolved else {
        warn!(path, ?action, "modular control path not found");
        return;
    };

    if kind == "track_list" {
        apply_track_action(state, path, action);
        return;
    }

    let Some(binding) = write else {
        warn!(path, kind, ?action, "modular control has no write binding");
        return;
    };

    if !apply_binding(state, path, &binding, action) {
        return;
    }

    state.ui_state = state.controller.snapshot();
}

fn apply_track_action(state: &mut Kithara, path: &str, action: &ControlAction) {
    if let ControlAction::SelectIndex(index) = action {
        handle_select_track(state, *index);
        state.ui_state = state.controller.snapshot();
    } else {
        warn!(path, ?action, "modular track list action mismatch");
    }
}

fn apply_binding(
    state: &mut Kithara,
    path: &str,
    binding: &ResolvedWrite,
    action: &ControlAction,
) -> bool {
    match (binding, action) {
        (ResolvedWrite::Command { id, deck_is_a }, ControlAction::Activate)
            if id == "deck.transport.toggle_play" && *deck_is_a =>
        {
            handle_toggle_play_pause(state);
            true
        }
        (ResolvedWrite::Command { id, deck_is_a }, ControlAction::Activate)
            if id == "deck.transport.prev" && *deck_is_a =>
        {
            handle_prev(state);
            true
        }
        (ResolvedWrite::Command { id, deck_is_a }, ControlAction::Activate)
            if id == "deck.transport.next" && *deck_is_a =>
        {
            handle_next(state);
            true
        }
        (ResolvedWrite::Command { id, deck_is_a }, ControlAction::SetScalar(value))
            if id == "deck.transport.seek_normalized" && *deck_is_a =>
        {
            if state.ui_state.duration > 0.0 {
                handle_seek_to(state, value * state.ui_state.duration);
            }
            true
        }
        (ResolvedWrite::Parameter { id }, ControlAction::SetScalar(value))
            if id == "player.output.volume" =>
        {
            let Some(volume) = value.to_f32() else {
                warn!(path, value, "modular scalar cannot be represented as f32");
                return false;
            };
            handle_volume_changed(state, volume);
            true
        }
        _ => {
            warn!(path, ?binding, ?action, "modular control binding mismatch");
            false
        }
    }
}

fn resolve_write(binding: &Binding, ui: &CompiledUi) -> Option<ResolvedWrite> {
    match binding {
        Binding::Command { id, with } => Some(ResolvedWrite::Command {
            id: ui.resolve(*id).to_owned(),
            deck_is_a: super::reads::deck_is_a(with, ui),
        }),
        Binding::Parameter { id, .. } => Some(ResolvedWrite::Parameter {
            id: ui.resolve(*id).to_owned(),
        }),
        _ => None,
    }
}

fn find_control(compiled: &CompiledUi, path: &str) -> Option<(InternId, Option<Binding>)> {
    find_compiled_node(&compiled.root, path, compiled)
}

fn find_compiled_node(
    node: &CompiledNode,
    path: &str,
    ui: &CompiledUi,
) -> Option<(InternId, Option<Binding>)> {
    match node {
        CompiledNode::Split { children, .. } => children
            .iter()
            .find_map(|(_, child)| find_compiled_node(child, path, ui)),
        CompiledNode::Module { root, .. } => find_expanded_node(root, path, ui),
        _ => None,
    }
}

fn find_expanded_node(
    node: &ExpandedNode,
    path: &str,
    ui: &CompiledUi,
) -> Option<(InternId, Option<Binding>)> {
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => children
            .iter()
            .find_map(|child| find_expanded_node(child, path, ui)),
        ExpandedNode::Control {
            path: control_path,
            kind,
            write,
            ..
        } if ui.resolve(*control_path) == path => Some((*kind, write.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::compile,
        expand::Binding,
        source::{MemResolver, UiConfig},
    };

    use super::find_control;
    use crate::gui::modular::endpoints;

    #[kithara::test]
    fn finds_control_write_binding_by_compiled_path() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "mini.klayout.ron",
            r#"(
                schema: "kithara.layout",
                version: 1,
                id: "mini",
                root: Module(
                    instance: "deck-a",
                    source: "control.kmodule.ron",
                ),
            )"#,
        );
        resolver.insert(
            "control.kmodule.ron",
            r#"(
                schema: "kithara.module",
                version: 1,
                id: "control",
                root: Control(
                    id: "play",
                    kind: "button",
                    props: { "label": Text("PLAY") },
                    read: Telemetry(
                        id: "deck.playback.playing",
                        with: { "deck": "a" },
                    ),
                    write: Command(
                        id: "deck.transport.toggle_play",
                        with: { "deck": "a" },
                    ),
                ),
            )"#,
        );
        let compiled = compile(
            "mini.klayout.ron",
            &resolver,
            &endpoints::catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("mini layout must compile: {error}"));

        let control = find_control(&compiled, "deck-a/play")
            .unwrap_or_else(|| panic!("compiled control path must resolve"));
        let Some(Binding::Command { id, .. }) = control.1 else {
            panic!("play control must have a command binding");
        };
        assert_eq!(compiled.resolve(id), "deck.transport.toggle_play");
        assert!(find_control(&compiled, "deck-a/missing").is_none());
    }
}
