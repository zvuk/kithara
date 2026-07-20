use std::collections::{BTreeMap, BTreeSet};

use crate::{
    error::UiDocError,
    expand::ControlSite,
    ids::{EndpointId, NodeId, SourceUri},
    layout::{LayoutDoc, LayoutNode},
    module::{BindingRef, ControlNode, ModuleDoc, TrackColumn},
    registry::{EndpointCategory, EndpointRegistry, ValueKind},
};

#[derive(Clone, Debug, Default)]
pub(crate) struct NodePath(Vec<String>);

impl NodePath {
    pub(crate) fn push(&self, segment: impl Into<String>) -> Self {
        let mut next = self.0.clone();
        next.push(segment.into());
        Self(next)
    }

    pub(crate) fn render(&self) -> String {
        if self.0.is_empty() {
            "root".to_owned()
        } else {
            format!("root/{}", self.0.join("/"))
        }
    }
}

pub(crate) fn check_layout_instances(
    doc: &LayoutDoc,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    let mut seen = BTreeSet::new();
    walk_layout(&doc.root, &NodePath::default(), origin, &mut seen)
}

fn walk_layout(
    node: &LayoutNode,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    match node {
        LayoutNode::Split { children, .. } => {
            for (index, child) in children.iter().enumerate() {
                let child_path = path.push(format!("Split[{index}]"));
                let weight = child.weight;
                if !weight.is_finite() || weight <= 0.0 {
                    return Err(UiDocError::InvalidWeight {
                        origin: origin.clone(),
                        path: child_path.render(),
                        value: format!("{weight}"),
                    });
                }
                walk_layout(&child.node, &child_path, origin, seen)?;
            }
            Ok(())
        }
        LayoutNode::Module { instance, .. } => {
            check_id(&instance.0, origin)?;
            if !seen.insert(instance.0.clone()) {
                return Err(UiDocError::DuplicateId {
                    origin: origin.clone(),
                    id: instance.0.clone(),
                    path: path.push(format!("Module({instance})")).render(),
                });
            }
            Ok(())
        }
    }
}

pub(crate) fn check_id(id: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    let reason = if id.is_empty() {
        Some("id must not be empty")
    } else if id.contains('/') {
        Some("id must not contain '/'")
    } else if id.starts_with('$') {
        Some("id must not start with '$'")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(UiDocError::InvalidId {
            origin: origin.clone(),
            id: id.to_owned(),
            reason: reason.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn check_module_node_ids(doc: &ModuleDoc, origin: &SourceUri) -> Result<(), UiDocError> {
    let mut seen = BTreeSet::new();
    walk_module(&doc.root, &NodePath::default(), origin, &mut seen)
}

fn record(
    id: &str,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    check_id(id, origin)?;
    if !seen.insert(id.to_owned()) {
        return Err(UiDocError::DuplicateId {
            origin: origin.clone(),
            id: id.to_owned(),
            path: path.render(),
        });
    }
    Ok(())
}

fn walk_module(
    node: &ControlNode,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    match node {
        ControlNode::Row { id, children, .. } | ControlNode::Column { id, children, .. } => {
            let here = match id {
                Some(id) => {
                    let here = path.push(format!("Group({id})"));
                    record(&id.0, &here, origin, seen)?;
                    here
                }
                None => path.clone(),
            };
            for (index, child) in children.iter().enumerate() {
                walk_module(child, &here.push(format!("[{index}]")), origin, seen)?;
            }
            Ok(())
        }
        ControlNode::Include { id, .. } => {
            record(&id.0, &path.push(format!("Include({id})")), origin, seen)
        }
        ControlNode::Slot { id, default, .. } => {
            let here = path.push(format!("Slot({id})"));
            record(&id.0, &here, origin, seen)?;
            for (index, child) in default.iter().enumerate() {
                walk_module(child, &here.push(format!("[{index}]")), origin, seen)?;
            }
            Ok(())
        }
        control => {
            if let Some(id) = control_id(control) {
                record(&id.0, &path.push(format!("Control({id})")), origin, seen)?;
            }
            Ok(())
        }
    }
}

fn control_id(node: &ControlNode) -> Option<&NodeId> {
    match node {
        ControlNode::Row { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Slot { .. } => None,
        ControlNode::DeckSummary { id, .. }
        | ControlNode::Brand { id, .. }
        | ControlNode::Spacer { id, .. }
        | ControlNode::PresetSelector { id, .. }
        | ControlNode::SettingsButton { id, .. }
        | ControlNode::Text { id, .. }
        | ControlNode::Glyph { id, .. }
        | ControlNode::NavItem { id, .. }
        | ControlNode::TabLarge { id, .. }
        | ControlNode::Button { id, .. }
        | ControlNode::Bpm { id, .. }
        | ControlNode::Time { id, .. }
        | ControlNode::Scalar { id, .. }
        | ControlNode::Fader { id, .. }
        | ControlNode::Wave { id, .. }
        | ControlNode::TrackList { id, .. }
        | ControlNode::Tree { id, .. }
        | ControlNode::ContextBar { id, .. }
        | ControlNode::Toggle { id, .. }
        | ControlNode::Checkbox { id, .. }
        | ControlNode::Segmented { id, .. }
        | ControlNode::Select { id, .. }
        | ControlNode::StatusDot { id, .. }
        | ControlNode::Cell { id, .. }
        | ControlNode::Readout { id, .. }
        | ControlNode::Chip { id, .. }
        | ControlNode::Knob { id, .. }
        | ControlNode::VuStereo { id, .. }
        | ControlNode::VuVertical { id, .. } => Some(id),
    }
}

pub(crate) fn check_controls(
    site: ControlSite<'_>,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    if let ControlNode::TrackList { columns, .. } = site.control {
        check_track_list(columns, site.columns_state, site.path, origin, endpoints)?;
    }
    if let Some(query) = site.query {
        check_binding(
            query,
            BindingSide::Read,
            Some(ValueKind::Text),
            site.path,
            origin,
            endpoints,
        )?;
    }
    let (read_kind, write_kind) = value_kinds(site.control);
    if let Some(binding) = site.read {
        check_binding(
            binding,
            BindingSide::Read,
            read_kind,
            site.path,
            origin,
            endpoints,
        )?;
    }
    if let Some(binding) = site.write {
        check_binding(
            binding,
            BindingSide::Write,
            write_kind,
            site.path,
            origin,
            endpoints,
        )?;
    }
    Ok(())
}

fn check_track_list(
    columns: &[TrackColumn],
    columns_state: Option<&BindingRef>,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    if !columns.contains(&TrackColumn::Title) {
        return Err(UiDocError::MissingTrackTitleColumn {
            origin: origin.clone(),
            path: path.to_owned(),
        });
    }
    let Some(binding) = columns_state else {
        return Ok(());
    };
    let (category, id, with) = binding_parts(binding);
    if !matches!(
        category,
        EndpointCategory::Parameter | EndpointCategory::Telemetry | EndpointCategory::Model
    ) {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: format!("{category} endpoint is not allowed on this side"),
        });
    }
    for column in columns {
        let derived = EndpointId(format!("{}.{}", id.0, column.endpoint_name()));
        let Some(endpoint) = endpoints.endpoint(category, &derived) else {
            continue;
        };
        if endpoint.value != ValueKind::Bool {
            return Err(UiDocError::BindingType {
                origin: origin.clone(),
                id: derived.0,
                path: path.to_owned(),
                expected: ValueKind::Bool.to_string(),
                got: endpoint.value.to_string(),
            });
        }
        check_scopes(&derived, with, endpoint, path, origin)?;
    }
    Ok(())
}

fn check_scopes(
    id: &EndpointId,
    with: &BTreeMap<String, String>,
    endpoint: &crate::registry::EndpointDesc,
    path: &str,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    for scope in &endpoint.scopes {
        if !with.contains_key(scope) {
            return Err(UiDocError::MissingScope {
                origin: origin.clone(),
                id: id.0.clone(),
                scope: scope.clone(),
                path: path.to_owned(),
            });
        }
    }
    for scope in with.keys() {
        if !endpoint.scopes.contains(scope) {
            return Err(UiDocError::UnknownScope {
                origin: origin.clone(),
                id: id.0.clone(),
                scope: scope.clone(),
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

pub(crate) fn check_module_footer(
    doc: &ModuleDoc,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    let Some(binding) = doc.footer.as_ref() else {
        return Ok(());
    };
    check_binding(
        binding,
        BindingSide::Read,
        Some(ValueKind::Text),
        "root/footer",
        origin,
        endpoints,
    )
}

#[derive(Clone, Copy)]
enum BindingSide {
    Read,
    Write,
}

pub(crate) fn value_kinds(control: &ControlNode) -> (Option<ValueKind>, Option<ValueKind>) {
    match control {
        ControlNode::Bpm { .. } => (Some(ValueKind::Waveform), None),
        ControlNode::DeckSummary { .. }
        | ControlNode::Text { .. }
        | ControlNode::Readout { .. }
        | ControlNode::ContextBar { .. } => (Some(ValueKind::Text), None),
        ControlNode::Button { .. }
        | ControlNode::NavItem { .. }
        | ControlNode::TabLarge { .. }
        | ControlNode::Toggle { .. }
        | ControlNode::Checkbox { .. }
        | ControlNode::Chip { .. } => (Some(ValueKind::Bool), Some(ValueKind::Trigger)),
        ControlNode::Segmented { .. } => (Some(ValueKind::Scalar), Some(ValueKind::Scalar)),
        ControlNode::Time { .. } | ControlNode::Scalar { .. } => (Some(ValueKind::Scalar), None),
        ControlNode::Fader { .. } | ControlNode::Knob { .. } => {
            (Some(ValueKind::Scalar), Some(ValueKind::Scalar))
        }
        ControlNode::Wave { .. } => (Some(ValueKind::Waveform), Some(ValueKind::Scalar)),
        ControlNode::TrackList { .. } => (Some(ValueKind::TrackList), None),
        ControlNode::Tree { .. } => (Some(ValueKind::Tree), None),
        ControlNode::VuStereo { .. } | ControlNode::VuVertical { .. } => {
            (Some(ValueKind::Stereo), Some(ValueKind::Scalar))
        }
        ControlNode::Row { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Slot { .. }
        | ControlNode::Brand { .. }
        | ControlNode::Spacer { .. }
        | ControlNode::PresetSelector { .. }
        | ControlNode::SettingsButton { .. }
        | ControlNode::Glyph { .. }
        | ControlNode::Select { .. }
        | ControlNode::StatusDot { .. }
        | ControlNode::Cell { .. } => (None, None),
    }
}

fn binding_parts(
    binding: &BindingRef,
) -> (EndpointCategory, &EndpointId, &BTreeMap<String, String>) {
    match binding {
        BindingRef::Command { id, with } => (EndpointCategory::Command, id, with),
        BindingRef::Parameter { id, with } => (EndpointCategory::Parameter, id, with),
        BindingRef::Telemetry { id, with } => (EndpointCategory::Telemetry, id, with),
        BindingRef::Model { id, with } => (EndpointCategory::Model, id, with),
    }
}

fn check_binding(
    binding: &BindingRef,
    side: BindingSide,
    expected_kind: Option<ValueKind>,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    let (category, id, with) = binding_parts(binding);
    let allowed = match side {
        BindingSide::Read => matches!(
            category,
            EndpointCategory::Parameter | EndpointCategory::Telemetry | EndpointCategory::Model
        ),
        BindingSide::Write => matches!(
            category,
            EndpointCategory::Command | EndpointCategory::Parameter
        ),
    };
    if !allowed {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: format!("{category} endpoint is not allowed on this side"),
        });
    }
    let Some(endpoint) = endpoints.endpoint(category, id) else {
        return Err(UiDocError::UnknownEndpoint {
            origin: origin.clone(),
            category: category.to_string(),
            id: id.0.clone(),
            path: path.to_owned(),
        });
    };
    let Some(expected_kind) = expected_kind else {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: "control does not support this side".to_owned(),
        });
    };
    if expected_kind != endpoint.value {
        return Err(UiDocError::BindingType {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            expected: expected_kind.to_string(),
            got: endpoint.value.to_string(),
        });
    }
    check_scopes(id, with, endpoint, path, origin)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ids::{EndpointId, SourceUri},
        layout::parse_layout,
        module::{BindingRef, parse_module},
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    };

    #[derive(Default)]
    struct TestRegistry {
        endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
    }

    impl TestRegistry {
        fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
            self.endpoints
                .insert((category, EndpointId(id.to_owned())), description);
        }
    }

    impl EndpointRegistry for TestRegistry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            self.endpoints.get(&(category, id.clone()))
        }
    }

    fn origin() -> SourceUri {
        SourceUri("dup.ron".into())
    }

    #[kithara::test]
    fn duplicate_instance_reports_path() {
        let text = r#"(schema: "kithara.layout", version: 1, id: "dup",
            root: Split(axis: Horizontal, children: [
                (node: Module(instance: "deck-a", source: "m.ron")),
                (node: Module(instance: "deck-a", source: "m.ron")),
            ]))"#;
        let doc = parse_layout(text, &origin()).unwrap();
        let error = check_layout_instances(&doc, &origin()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("deck-a"), "{message}");
        assert!(message.contains("Split[1]"), "{message}");
    }

    #[kithara::test]
    fn layout_instance_with_path_separator_is_rejected() {
        let text = r#"(schema: "kithara.layout", version: 1, id: "invalid",
            root: Module(instance: "deck/a", source: "m.ron"))"#;
        let doc = parse_layout(text, &origin()).unwrap();
        let error = check_layout_instances(&doc, &origin()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::InvalidId { id, reason, .. }
                if id == "deck/a" && reason.contains('/')
        ));
    }

    #[kithara::test]
    fn negative_split_weight_is_rejected() {
        let text = r#"(schema: "kithara.layout", version: 1, id: "invalid",
            root: Split(axis: Horizontal, children: [
                (weight: -1.0, node: Module(instance: "deck-a", source: "m.ron")),
            ]))"#;
        let doc = parse_layout(text, &origin()).unwrap();
        let error = check_layout_instances(&doc, &origin()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::InvalidWeight { path, value, .. }
                if path == "root/Split[0]" && value == "-1"
        ));
    }

    #[kithara::test]
    fn zero_split_weight_is_rejected() {
        let text = r#"(schema: "kithara.layout", version: 1, id: "invalid",
            root: Split(axis: Horizontal, children: [
                (weight: 0.0, node: Module(instance: "deck-a", source: "m.ron")),
            ]))"#;
        let doc = parse_layout(text, &origin()).unwrap();
        let error = check_layout_instances(&doc, &origin()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::InvalidWeight { path, value, .. }
                if path == "root/Split[0]" && value == "0"
        ));
    }

    #[kithara::test]
    fn empty_and_parameter_like_ids_are_rejected() {
        for id in ["", "$deck"] {
            assert!(matches!(
                check_id(id, &origin()),
                Err(UiDocError::InvalidId { id: invalid, .. }) if invalid == id
            ));
        }
    }

    #[kithara::test]
    fn duplicate_control_id_reports_path() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Row(children: [
                Button(id: "play", label: "PLAY"),
                Button(id: "play", label: "PLAY"),
            ]))"#;
        let doc = parse_module(text, &origin()).unwrap();
        let error = check_module_node_ids(&doc, &origin()).unwrap_err();
        assert!(error.to_string().contains("Control(play)"));
    }

    #[kithara::test]
    fn control_id_with_path_separator_is_rejected() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Button(id: "transport/play", label: "PLAY"))"#;
        let doc = parse_module(text, &origin()).unwrap();
        let error = check_module_node_ids(&doc, &origin()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::InvalidId { id, reason, .. }
                if id == "transport/play" && reason.contains('/')
        ));
    }

    #[kithara::test]
    fn unique_ids_pass() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Row(children: [
                Button(id: "play", label: "PLAY"),
                Slot(id: "extra"),
            ]))"#;
        let doc = parse_module(text, &origin()).unwrap();
        check_module_node_ids(&doc, &origin()).unwrap();
    }

    fn check_control(body: &str, path: &str, write: Option<&BindingRef>) -> Result<(), UiDocError> {
        let text = format!(r#"(schema: "kithara.module", version: 1, id: "test", root: {body})"#);
        let document = parse_module(&text, &origin())?;
        check_controls(
            ControlSite {
                path,
                control: &document.root,
                read: None,
                write,
                columns_state: None,
                query: None,
            },
            &origin(),
            &registry(),
        )
    }

    fn registry() -> TestRegistry {
        let mut registry = TestRegistry::default();
        registry.insert(
            EndpointCategory::Command,
            "deck.transport.toggle_play",
            EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
        );
        registry.insert(
            EndpointCategory::Parameter,
            "player.output.volume",
            EndpointDesc::new(ValueKind::Scalar),
        );
        registry
    }

    fn with_deck() -> BTreeMap<String, String> {
        std::iter::once(("deck".to_owned(), "a".to_owned())).collect()
    }

    #[kithara::test]
    fn valid_command_binding_passes() {
        let binding = BindingRef::Command {
            id: EndpointId("deck.transport.toggle_play".into()),
            with: with_deck(),
        };
        check_control(
            r#"Button(id: "play", label: "PLAY")"#,
            "play",
            Some(&binding),
        )
        .unwrap();
    }

    #[kithara::test]
    fn tree_query_binding_must_be_text() {
        let document = parse_module(
            r#"(schema: "kithara.module", version: 1, id: "tree",
                root: Tree(
                    id: "browser",
                    query: Parameter(id: "player.output.volume"),
                ))"#,
            &origin(),
        )
        .unwrap();
        let ControlNode::Tree { query, .. } = &document.root else {
            panic!("expected tree");
        };

        let error = check_controls(
            ControlSite {
                path: "tree/browser",
                control: &document.root,
                read: None,
                write: None,
                columns_state: None,
                query: query.as_ref(),
            },
            &origin(),
            &registry(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            UiDocError::BindingType {
                expected,
                got,
                path,
                ..
            } if expected == "Text" && got == "Scalar" && path == "tree/browser"
        ));
    }

    #[kithara::test]
    fn missing_scope_is_reported() {
        let binding = BindingRef::Command {
            id: EndpointId("deck.transport.toggle_play".into()),
            with: BTreeMap::new(),
        };
        let error = check_control(
            r#"Button(id: "play", label: "PLAY")"#,
            "play",
            Some(&binding),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            UiDocError::MissingScope { scope, .. } if scope == "deck"
        ));
    }

    #[kithara::test]
    fn undeclared_command_scope_is_reported() {
        let mut with = with_deck();
        with.insert("sidechain".to_owned(), "1".to_owned());
        let binding = BindingRef::Command {
            id: EndpointId("deck.transport.toggle_play".into()),
            with,
        };
        let error = check_control(
            r#"Button(id: "play", label: "PLAY")"#,
            "play",
            Some(&binding),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnknownScope {
                id,
                scope,
                path,
                ..
            } if id == "deck.transport.toggle_play" && scope == "sidechain" && path == "play"
        ));
    }

    #[kithara::test]
    fn scope_on_unscoped_parameter_is_reported() {
        let binding = BindingRef::Parameter {
            id: EndpointId("player.output.volume".into()),
            with: with_deck(),
        };
        let error = check_control(r#"Fader(id: "volume")"#, "volume", Some(&binding)).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnknownScope {
                id,
                scope,
                path,
                ..
            } if id == "player.output.volume" && scope == "deck" && path == "volume"
        ));
    }

    #[kithara::test]
    fn model_binding_on_write_side_is_direction_error() {
        let binding = BindingRef::Model {
            id: EndpointId("library.visible_tracks".into()),
            with: BTreeMap::new(),
        };
        let error = check_control(
            r#"Button(id: "play", label: "PLAY")"#,
            "play",
            Some(&binding),
        )
        .unwrap_err();
        assert!(matches!(error, UiDocError::BindingDirection { .. }));
    }
}
