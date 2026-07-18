use std::collections::{BTreeMap, BTreeSet};

use crate::{
    error::UiDocError,
    expand::ExpandedNode,
    ids::{ControlKind, EndpointId, SourceUri},
    layout::{LayoutDoc, LayoutNode},
    module::{BindingRef, ControlNode, ModuleDoc, PropValue},
    registry::{ControlCatalog, ControlKindDesc, EndpointCategory, EndpointRegistry, PropKind},
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
        ControlNode::Row { id, children } | ControlNode::Column { id, children } => {
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
        ControlNode::Slot { id, default } => {
            let here = path.push(format!("Slot({id})"));
            record(&id.0, &here, origin, seen)?;
            for (index, child) in default.iter().enumerate() {
                walk_module(child, &here.push(format!("[{index}]")), origin, seen)?;
            }
            Ok(())
        }
        ControlNode::Control { id, .. } => {
            record(&id.0, &path.push(format!("Control({id})")), origin, seen)
        }
    }
}

pub(crate) fn check_controls(
    root: &ExpandedNode,
    origin: &SourceUri,
    catalog: &dyn ControlCatalog,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    match root {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => {
            for child in children {
                check_controls(child, origin, catalog, endpoints)?;
            }
            Ok(())
        }
        ExpandedNode::Control {
            path,
            kind,
            props,
            read,
            write,
            ..
        } => {
            let Some(description) = catalog.kind(kind) else {
                return Err(UiDocError::UnknownControlKind {
                    origin: origin.clone(),
                    kind: kind.0.clone(),
                    path: path.clone(),
                });
            };
            check_props(description, kind, props, path, origin)?;
            if let Some(binding) = read {
                check_binding(
                    binding,
                    BindingSide::Read,
                    description,
                    path,
                    origin,
                    endpoints,
                )?;
            }
            if let Some(binding) = write {
                check_binding(
                    binding,
                    BindingSide::Write,
                    description,
                    path,
                    origin,
                    endpoints,
                )?;
            }
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum BindingSide {
    Read,
    Write,
}

fn prop_kind_of(value: &PropValue) -> PropKind {
    match value {
        PropValue::Bool(_) => PropKind::Bool,
        PropValue::Num(_) => PropKind::Num,
        PropValue::Text(_) => PropKind::Text,
    }
}

fn check_props(
    description: &ControlKindDesc,
    kind: &ControlKind,
    props: &BTreeMap<String, PropValue>,
    path: &str,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    for (name, value) in props {
        let Some(expected) = description.props.get(name) else {
            return Err(UiDocError::UnknownProp {
                origin: origin.clone(),
                kind: kind.0.clone(),
                prop: name.clone(),
                path: path.to_owned(),
            });
        };
        let got = prop_kind_of(value);
        if got != *expected {
            return Err(UiDocError::PropType {
                origin: origin.clone(),
                prop: name.clone(),
                path: path.to_owned(),
                expected: expected.to_string(),
                got: got.to_string(),
            });
        }
    }
    Ok(())
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
    description: &ControlKindDesc,
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
    let control_kind = match side {
        BindingSide::Read => description.read,
        BindingSide::Write => description.write,
    };
    let Some(control_kind) = control_kind else {
        return Err(UiDocError::BindingDirection {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            detail: "control kind does not support this side".to_owned(),
        });
    };
    if control_kind != endpoint.value {
        return Err(UiDocError::BindingType {
            origin: origin.clone(),
            id: id.0.clone(),
            path: path.to_owned(),
            expected: control_kind.to_string(),
            got: endpoint.value.to_string(),
        });
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        expand::ExpandedNode,
        ids::{ControlKind, EndpointId, NodeId, SourceUri},
        layout::parse_layout,
        module::{AdaptivePolicy, BindingRef, parse_module},
        registry::{
            ControlCatalog, ControlKindDesc, EndpointCategory, EndpointDesc, EndpointRegistry,
            PropKind, ValueKind,
        },
    };

    #[derive(Default)]
    struct TestCatalog {
        kinds: BTreeMap<ControlKind, ControlKindDesc>,
    }

    impl TestCatalog {
        fn insert(&mut self, kind: &str, description: ControlKindDesc) {
            self.kinds.insert(ControlKind(kind.to_owned()), description);
        }
    }

    impl ControlCatalog for TestCatalog {
        fn kind(&self, kind: &ControlKind) -> Option<&ControlKindDesc> {
            self.kinds.get(kind)
        }
    }

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
                Control(id: "play", kind: "button"),
                Control(id: "play", kind: "button"),
            ]))"#;
        let doc = parse_module(text, &origin()).unwrap();
        let error = check_module_node_ids(&doc, &origin()).unwrap_err();
        assert!(error.to_string().contains("Control(play)"));
    }

    #[kithara::test]
    fn control_id_with_path_separator_is_rejected() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Control(id: "transport/play", kind: "button"))"#;
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
                Control(id: "play", kind: "button"),
                Slot(id: "extra"),
            ]))"#;
        let doc = parse_module(text, &origin()).unwrap();
        check_module_node_ids(&doc, &origin()).unwrap();
    }

    fn button_control(write: BindingRef) -> ExpandedNode {
        ExpandedNode::Control {
            path: "play".into(),
            id: NodeId("play".into()),
            kind: ControlKind("button".into()),
            props: BTreeMap::new(),
            read: None,
            write: Some(write),
            adaptive: AdaptivePolicy::default(),
        }
    }

    fn catalog() -> TestCatalog {
        let mut catalog = TestCatalog::default();
        catalog.insert(
            "button",
            ControlKindDesc::new(Some(ValueKind::Bool), Some(ValueKind::Trigger))
                .with_prop("label", PropKind::Text),
        );
        catalog.insert(
            "fader",
            ControlKindDesc::new(Some(ValueKind::Scalar), Some(ValueKind::Scalar)),
        );
        catalog
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
        let node = button_control(BindingRef::Command {
            id: EndpointId("deck.transport.toggle_play".into()),
            with: with_deck(),
        });
        check_controls(&node, &origin(), &catalog(), &registry()).unwrap();
    }

    #[kithara::test]
    fn missing_scope_is_reported() {
        let node = button_control(BindingRef::Command {
            id: EndpointId("deck.transport.toggle_play".into()),
            with: BTreeMap::new(),
        });
        let error = check_controls(&node, &origin(), &catalog(), &registry()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::MissingScope { scope, .. } if scope == "deck"
        ));
    }

    #[kithara::test]
    fn undeclared_command_scope_is_reported() {
        let mut with = with_deck();
        with.insert("sidechain".to_owned(), "1".to_owned());
        let node = button_control(BindingRef::Command {
            id: EndpointId("deck.transport.toggle_play".into()),
            with,
        });
        let error = check_controls(&node, &origin(), &catalog(), &registry()).unwrap_err();
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
        let node = ExpandedNode::Control {
            path: "volume".into(),
            id: NodeId("volume".into()),
            kind: ControlKind("fader".into()),
            props: BTreeMap::new(),
            read: None,
            write: Some(BindingRef::Parameter {
                id: EndpointId("player.output.volume".into()),
                with: with_deck(),
            }),
            adaptive: AdaptivePolicy::default(),
        };
        let error = check_controls(&node, &origin(), &catalog(), &registry()).unwrap_err();
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
        let node = button_control(BindingRef::Model {
            id: EndpointId("library.visible_tracks".into()),
            with: BTreeMap::new(),
        });
        let error = check_controls(&node, &origin(), &catalog(), &registry()).unwrap_err();
        assert!(matches!(error, UiDocError::BindingDirection { .. }));
    }

    #[kithara::test]
    fn unknown_kind_is_reported_with_path() {
        let node = ExpandedNode::Control {
            path: "unknown".into(),
            id: NodeId("unknown".into()),
            kind: ControlKind("nope".into()),
            props: BTreeMap::new(),
            read: None,
            write: None,
            adaptive: AdaptivePolicy::default(),
        };
        let error = check_controls(&node, &origin(), &catalog(), &registry()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnknownControlKind { kind, path, .. }
                if kind == "nope" && path == "unknown"
        ));
    }
}
