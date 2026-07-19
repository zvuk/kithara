use kithara_ui::{
    compile::{CompiledNode, CompiledUi},
    expand::ExpandedNode,
    ids::ControlKind,
    module::BindingRef,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::ControlAction;
use crate::gui::{
    app::Kithara,
    update::{
        handle_next, handle_prev, handle_seek_to, handle_select_track, handle_toggle_play_pause,
        handle_volume_changed,
    },
};

struct ControlRef<'a> {
    kind: &'a ControlKind,
    write: Option<&'a BindingRef>,
}

pub(super) fn apply(state: &mut Kithara, path: &str, action: &ControlAction) {
    let Some((kind, write)) = state
        .modular
        .compiled
        .as_ref()
        .and_then(|compiled| find_control(compiled, path))
        .map(|control| (control.kind.0.clone(), control.write.cloned()))
    else {
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
    binding: &BindingRef,
    action: &ControlAction,
) -> bool {
    match (binding, action) {
        (BindingRef::Command { id, with }, ControlAction::Activate)
            if id.0 == "deck.transport.toggle_play" && deck_is_a(with) =>
        {
            handle_toggle_play_pause(state);
            true
        }
        (BindingRef::Command { id, with }, ControlAction::Activate)
            if id.0 == "deck.transport.prev" && deck_is_a(with) =>
        {
            handle_prev(state);
            true
        }
        (BindingRef::Command { id, with }, ControlAction::Activate)
            if id.0 == "deck.transport.next" && deck_is_a(with) =>
        {
            handle_next(state);
            true
        }
        (BindingRef::Command { id, with }, ControlAction::SetScalar(value))
            if id.0 == "deck.transport.seek_normalized" && deck_is_a(with) =>
        {
            if state.ui_state.duration > 0.0 {
                handle_seek_to(state, value * state.ui_state.duration);
            }
            true
        }
        (BindingRef::Parameter { id, .. }, ControlAction::SetScalar(value))
            if id.0 == "player.output.volume" =>
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

fn deck_is_a(scope: &std::collections::BTreeMap<String, String>) -> bool {
    scope.get("deck").is_some_and(|deck| deck == "a")
}

fn find_control<'a>(compiled: &'a CompiledUi, path: &str) -> Option<ControlRef<'a>> {
    find_compiled_node(&compiled.root, path)
}

fn find_compiled_node<'a>(node: &'a CompiledNode, path: &str) -> Option<ControlRef<'a>> {
    match node {
        CompiledNode::Split { children, .. } => children
            .iter()
            .find_map(|(_, child)| find_compiled_node(child, path)),
        CompiledNode::Module { root, .. } => find_expanded_node(root, path),
        _ => None,
    }
}

fn find_expanded_node<'a>(node: &'a ExpandedNode, path: &str) -> Option<ControlRef<'a>> {
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => children
            .iter()
            .find_map(|child| find_expanded_node(child, path)),
        ExpandedNode::Control {
            path: control_path,
            kind,
            write,
            ..
        } if control_path == path => Some(ControlRef {
            kind,
            write: write.as_ref(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::compile,
        module::BindingRef,
        source::{Limits, MemResolver},
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
            &Limits::default(),
        )
        .unwrap_or_else(|error| panic!("mini layout must compile: {error}"));

        let control = find_control(&compiled, "deck-a/play")
            .unwrap_or_else(|| panic!("compiled control path must resolve"));
        let Some(BindingRef::Command { id, .. }) = control.write else {
            panic!("play control must have a command binding");
        };
        assert_eq!(id.0, "deck.transport.toggle_play");
        assert!(find_control(&compiled, "deck-a/missing").is_none());
    }
}
