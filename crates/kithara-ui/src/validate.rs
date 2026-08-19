use std::collections::{BTreeMap, BTreeSet};

use crate::{
    error::UiDocError,
    expand::ControlSite,
    ids::{EndpointId, NodeId, SourceUri},
    layout::{LayoutDoc, LayoutNode},
    module::{BindingRef, ControlNode, ModuleDoc, Pose, TableColumn},
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
    walk_layout(
        &doc.root,
        &NodePath::default(),
        origin,
        &mut seen,
        Sibling::Only,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Sibling {
    Among,
    Only,
}

fn check_block_position(
    id: &NodeId,
    path: &NodePath,
    origin: &SourceUri,
    sibling: Sibling,
) -> Result<(), UiDocError> {
    if sibling == Sibling::Among {
        return Ok(());
    }
    Err(UiDocError::RootBlock {
        origin: origin.clone(),
        id: id.0.clone(),
        path: path.render(),
    })
}

fn walk_layout(
    node: &LayoutNode,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
    sibling: Sibling,
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
                walk_layout(&child.node, &child_path, origin, seen, Sibling::Among)?;
            }
            Ok(())
        }
        LayoutNode::Optional { id, node, .. } => {
            let here = path.push(format!("Optional({id})"));
            check_block_position(id, &here, origin, sibling)?;
            record_block(id, &here, origin, seen)?;
            walk_layout(node, &here, origin, seen, Sibling::Only)
        }
        LayoutNode::Module { instance, .. } => {
            check_id(&instance.0, origin)?;
            claim(
                &instance.0,
                &path.push(format!("Module({instance})")),
                origin,
                seen,
            )
        }
    }
}

pub(crate) fn check_block_path(path: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    let reason = if path.contains('.') {
        "block address must not contain '.'"
    } else if path.contains('@') {
        "block address must not contain '@'"
    } else {
        return Ok(());
    };
    Err(UiDocError::InvalidId {
        origin: origin.clone(),
        id: path.to_owned(),
        reason: reason.to_owned(),
    })
}

pub(crate) fn check_block_id(id: &str, origin: &SourceUri) -> Result<(), UiDocError> {
    check_id(id, origin)?;
    check_block_path(id, origin)
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

pub(crate) fn check_module_id(doc: &ModuleDoc, origin: &SourceUri) -> Result<(), UiDocError> {
    check_id(&doc.id.0, origin)?;
    if doc.id.0.contains('.') {
        return Err(UiDocError::InvalidId {
            origin: origin.clone(),
            id: doc.id.0.clone(),
            reason: "module id addresses its collapsed state and must not contain '.'".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn check_module_node_ids(doc: &ModuleDoc, origin: &SourceUri) -> Result<(), UiDocError> {
    let mut seen = BTreeSet::new();
    walk_module(
        &doc.root,
        &NodePath::default(),
        origin,
        &mut seen,
        Sibling::Only,
    )
}

fn claim(
    id: &str,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    if !seen.insert(id.to_owned()) {
        return Err(UiDocError::DuplicateId {
            origin: origin.clone(),
            id: id.to_owned(),
            path: path.render(),
        });
    }
    Ok(())
}

fn record(
    id: &str,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    check_id(id, origin)?;
    claim(id, path, origin, seen)
}

fn record_block(
    id: &NodeId,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    check_block_id(&id.0, origin)?;
    claim(&id.0, path, origin, seen)
}

fn walk_module(
    node: &ControlNode,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
    sibling: Sibling,
) -> Result<(), UiDocError> {
    match node {
        ControlNode::Row {
            id,
            write,
            children,
            ..
        }
        | ControlNode::Column {
            id,
            write,
            children,
            ..
        } => {
            if id.is_none() && write.is_some() {
                return Err(UiDocError::UnaddressedSurface {
                    origin: origin.clone(),
                    path: path.render(),
                });
            }
            let here = match id {
                Some(id) => {
                    let here = path.push(format!("Group({id})"));
                    record(&id.0, &here, origin, seen)?;
                    here
                }
                None => path.clone(),
            };
            for (index, child) in children.iter().enumerate() {
                walk_module(
                    child,
                    &here.push(format!("[{index}]")),
                    origin,
                    seen,
                    Sibling::Among,
                )?;
            }
            Ok(())
        }
        ControlNode::Include { id, .. } => {
            record(&id.0, &path.push(format!("Include({id})")), origin, seen)
        }
        ControlNode::Optional { id, child, .. } => {
            let here = path.push(format!("Optional({id})"));
            check_block_position(id, &here, origin, sibling)?;
            record_block(id, &here, origin, seen)?;
            walk_module(child, &here, origin, seen, Sibling::Only)
        }
        ControlNode::Popover {
            id,
            anchor,
            content,
            ..
        } => {
            let here = path.push(format!("Popover({id})"));
            record(&id.0, &here, origin, seen)?;
            walk_module(anchor, &here, origin, seen, Sibling::Only)?;
            walk_module(content, &here, origin, seen, Sibling::Only)
        }
        ControlNode::Pressable { id, child, .. } => {
            let here = path.push(format!("Pressable({id})"));
            record(&id.0, &here, origin, seen)?;
            walk_module(child, &here, origin, seen, Sibling::Only)
        }
        ControlNode::Scroll { id, child, .. } => {
            let here = path.push(format!("Scroll({id})"));
            record(&id.0, &here, origin, seen)?;
            walk_module(child, &here, origin, seen, Sibling::Only)
        }
        ControlNode::Object {
            id,
            transform,
            to,
            phase,
            motion,
            child,
        } => {
            let here = path.push(format!("Object({id})"));
            record(&id.0, &here, origin, seen)?;
            one_driver(phase.is_some(), motion.is_some(), &here, origin)?;
            single_box(transform, to.as_ref(), child, &here, origin)?;
            walk_module(child, &here, origin, seen, Sibling::Only)
        }
        ControlNode::Stage { id, children, .. } => {
            let here = path.push(format!("Stage({id})"));
            record(&id.0, &here, origin, seen)?;
            for (index, child) in children.iter().enumerate() {
                walk_module(
                    child,
                    &here.push(format!("[{index}]")),
                    origin,
                    seen,
                    Sibling::Among,
                )?;
            }
            Ok(())
        }
        ControlNode::Slot { id, default, .. } => {
            let here = path.push(format!("Slot({id})"));
            record(&id.0, &here, origin, seen)?;
            for (index, child) in default.iter().enumerate() {
                walk_module(
                    child,
                    &here.push(format!("[{index}]")),
                    origin,
                    seen,
                    Sibling::Among,
                )?;
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

/// One pose, one thing driving it.
///
/// A motion is not an alternative to a phase, it is a way of computing one, so
/// an object carrying both would leave two answers for a single scalar with no
/// honest rule for choosing between them. Refusing here is what keeps the
/// render pass from having to invent one.
fn one_driver(
    phase: bool,
    motion: bool,
    path: &NodePath,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    if phase && motion {
        return Err(UiDocError::ObjectDrivenTwice {
            origin: origin.clone(),
            path: path.render(),
        });
    }
    Ok(())
}

/// What a pose can reach.
///
/// A move applies to any subtree, because every box in it shifts by the same
/// vector. A turn or a scale does not: each box would turn about its own
/// corner, and a group would come apart, so a turning object has to hold
/// something laid out as one box. And nothing at all reaches a control that
/// paints a native pass or hands back a list it already finished — the box
/// would move and the picture would stay.
fn single_box(
    transform: &Pose,
    to: Option<&Pose>,
    child: &ControlNode,
    path: &NodePath,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    // A track is judged by both ends: an object that starts still and travels
    // to a turn still turns.
    let travels = to.is_some_and(|to| !to.is_still());
    if transform.is_still() && !travels {
        return Ok(());
    }
    if let Some(child) = native_pass(child) {
        return Err(UiDocError::ObjectNative {
            origin: origin.clone(),
            path: path.render(),
            child,
        });
    }
    let turns = transform.turns() || to.is_some_and(Pose::turns);
    let group = match child {
        _ if !turns => return Ok(()),
        ControlNode::Row { .. } => "Row",
        ControlNode::Column { .. } => "Column",
        ControlNode::Stage { .. } => "Stage",
        ControlNode::Slot { .. } => "Slot",
        ControlNode::Scroll { .. } => "Scroll",
        ControlNode::Popover { .. } => "Popover",
        ControlNode::Include { .. } => "Include",
        _ => return Ok(()),
    };
    Err(UiDocError::ObjectGroup {
        origin: origin.clone(),
        path: path.render(),
        child: group,
    })
}

const fn native_pass(child: &ControlNode) -> Option<&'static str> {
    match child {
        ControlNode::Shader { .. } => Some("Shader"),
        ControlNode::Vis { .. } => Some("Vis"),
        ControlNode::Table { .. } => Some("Table"),
        ControlNode::Tree { .. } => Some("Tree"),
        _ => None,
    }
}

const fn control_id(node: &ControlNode) -> Option<&NodeId> {
    match node {
        ControlNode::Row { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Object { .. }
        | ControlNode::Optional { .. }
        | ControlNode::Popover { .. }
        | ControlNode::Pressable { .. }
        | ControlNode::Scroll { .. }
        | ControlNode::Stage { .. }
        | ControlNode::Slot { .. } => None,
        ControlNode::DeckSummary { id, .. }
        | ControlNode::Brand { id, .. }
        | ControlNode::Spacer { id, .. }
        | ControlNode::Divider { id, .. }
        | ControlNode::PresetSelector { id, .. }
        | ControlNode::SettingsButton { id, .. }
        | ControlNode::WindowDrag { id, .. }
        | ControlNode::TitleBar { id, .. }
        | ControlNode::WindowControls { id, .. }
        | ControlNode::Text { id, .. }
        | ControlNode::Glyph { id, .. }
        | ControlNode::NavItem { id, .. }
        | ControlNode::TabLarge { id, .. }
        | ControlNode::Button { id, .. }
        | ControlNode::Bpm { id, .. }
        | ControlNode::Time { id, .. }
        | ControlNode::Scalar { id, .. }
        | ControlNode::Crossfader { id, .. }
        | ControlNode::Fader { id, .. }
        | ControlNode::Wave { id, .. }
        | ControlNode::Vis { id, .. }
        | ControlNode::Sprite { id, .. }
        | ControlNode::Lottie { id, .. }
        | ControlNode::Shader { id, .. }
        | ControlNode::PortalMap { id, .. }
        | ControlNode::Range { id, .. }
        | ControlNode::Table { id, .. }
        | ControlNode::Tree { id, .. }
        | ControlNode::ContextBar { id, .. }
        | ControlNode::Toggle { id, .. }
        | ControlNode::Checkbox { id, .. }
        | ControlNode::Segmented { id, .. }
        | ControlNode::Select { id, .. }
        | ControlNode::StatusDot { id, .. }
        | ControlNode::Swatch { id, .. }
        | ControlNode::Cell { id, .. }
        | ControlNode::Readout { id, .. }
        | ControlNode::Chip { id, .. }
        | ControlNode::Knob { id, .. }
        | ControlNode::VuStereo { id, .. }
        | ControlNode::VuVertical { id, .. }
        | ControlNode::Meter { id, .. } => Some(id),
    }
}

pub(crate) fn check_controls(
    site: ControlSite<'_>,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    check_context_scope(site, origin)?;
    if matches!(site.control, ControlNode::Table { .. }) {
        check_table(
            site.columns,
            site.columns_state,
            site.path,
            origin,
            endpoints,
        )?;
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
    if let Some(scope) = site.scope {
        check_binding(
            scope,
            BindingSide::Read,
            Some(ValueKind::Scalar),
            site.path,
            origin,
            endpoints,
        )?;
    }
    if let Some(zoom) = site.zoom {
        check_binding(
            zoom,
            BindingSide::Read,
            Some(ValueKind::Scalar),
            site.path,
            origin,
            endpoints,
        )?;
    }
    if let Some(active) = site.active {
        check_binding(
            active,
            BindingSide::Read,
            Some(ValueKind::Bool),
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
        let side = if matches!(site.control, ControlNode::ContextBar { .. }) {
            BindingSide::ModelWrite
        } else {
            BindingSide::Write
        };
        check_binding(binding, side, write_kind, site.path, origin, endpoints)?;
    }
    Ok(())
}

pub(crate) fn shader_uniform_kind(
    name: &str,
    binding: &BindingRef,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<ValueKind, UiDocError> {
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
    let Some(endpoint) = endpoints.endpoint(category, id) else {
        return Err(UiDocError::UnknownEndpoint {
            origin: origin.clone(),
            category: category.to_string(),
            id: id.0.clone(),
            path: path.to_owned(),
        });
    };
    if !matches!(
        endpoint.value,
        ValueKind::Bool | ValueKind::Scalar | ValueKind::Stereo
    ) {
        return Err(UiDocError::Shader {
            origin: origin.clone(),
            path: path.to_owned(),
            detail: format!(
                "uniform {name:?} binds {kind} endpoint {id:?}; expected Bool, Scalar, or Stereo",
                kind = endpoint.value,
                id = id.0,
            ),
        });
    }
    check_scopes(id, with, endpoint, path, origin)?;
    Ok(endpoint.value)
}

fn check_context_scope(site: ControlSite<'_>, origin: &SourceUri) -> Result<(), UiDocError> {
    let ControlNode::ContextBar { scope_items, .. } = site.control else {
        return Ok(());
    };
    let enabled = !scope_items.is_empty();
    if enabled == site.scope.is_some() && enabled == site.write.is_some() {
        return Ok(());
    }
    Err(UiDocError::InvalidContextScope {
        origin: origin.clone(),
        path: site.path.to_owned(),
    })
}

fn check_table(
    columns: &[TableColumn],
    columns_state: Option<&BindingRef>,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
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
        let derived = EndpointId(format!("{}.{}", id.0, column.id()));
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

const BLOCK_HIDDEN: ValueKind = ValueKind::Bool;

pub(crate) fn check_layout_block(
    hidden: &BindingRef,
    path: &str,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    check_binding(
        hidden,
        BindingSide::Read,
        Some(BLOCK_HIDDEN),
        path,
        origin,
        endpoints,
    )
}

pub(crate) fn check_layout_dragged(
    doc: &LayoutDoc,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    let Some(binding) = doc.dragged.as_ref() else {
        return Ok(());
    };
    check_binding(
        binding,
        BindingSide::Read,
        Some(ValueKind::Text),
        "root/dragged",
        origin,
        endpoints,
    )
}

pub(crate) fn check_module_drop(
    doc: &ModuleDoc,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    let Some(drop) = doc.drop.as_ref() else {
        return Ok(());
    };
    check_binding(
        &drop.write,
        BindingSide::Write,
        Some(ValueKind::Trigger),
        "root/drop",
        origin,
        endpoints,
    )?;
    check_binding(
        &drop.read,
        BindingSide::Read,
        Some(ValueKind::Bool),
        "root/drop",
        origin,
        endpoints,
    )
}

#[derive(Clone, Copy)]
enum BindingSide {
    Read,
    Write,
    ModelWrite,
}

pub(crate) const fn value_kinds(control: &ControlNode) -> (Option<ValueKind>, Option<ValueKind>) {
    match control {
        ControlNode::Bpm { .. } => (Some(ValueKind::Waveform), None),
        ControlNode::DeckSummary { .. }
        | ControlNode::Text { .. }
        | ControlNode::Readout { .. } => (Some(ValueKind::Text), None),
        ControlNode::ContextBar { .. } => (Some(ValueKind::Text), Some(ValueKind::Scalar)),
        ControlNode::Optional { .. } => (Some(BLOCK_HIDDEN), None),
        ControlNode::Popover { .. } => (Some(ValueKind::Bool), None),
        ControlNode::Pressable { .. } => (None, Some(ValueKind::Trigger)),
        ControlNode::Button { .. }
        | ControlNode::NavItem { .. }
        | ControlNode::TabLarge { .. }
        | ControlNode::Toggle { .. }
        | ControlNode::Checkbox { .. }
        | ControlNode::Chip { .. } => (Some(ValueKind::Bool), Some(ValueKind::Trigger)),
        // A sprite reads how far its sheet has run, an artwork how far its pass
        // has, and an object how far its motion has, and nothing writes back
        // through any of them.
        ControlNode::Time { .. }
        | ControlNode::Scalar { .. }
        | ControlNode::Meter { .. }
        | ControlNode::Sprite { .. }
        | ControlNode::Lottie { .. }
        | ControlNode::Object { .. } => (Some(ValueKind::Scalar), None),
        ControlNode::Crossfader { .. }
        | ControlNode::Fader { .. }
        | ControlNode::Knob { .. }
        | ControlNode::Segmented { .. }
        | ControlNode::Vis { .. } => (Some(ValueKind::Scalar), Some(ValueKind::Scalar)),
        ControlNode::Wave { .. } => (Some(ValueKind::Waveform), Some(ValueKind::Scalar)),
        ControlNode::PortalMap { .. } => (Some(ValueKind::PortalMap), None),
        ControlNode::Range { .. } => (Some(ValueKind::Range), Some(ValueKind::Scalar)),
        ControlNode::Table { .. } => (Some(ValueKind::Table), None),
        ControlNode::Tree { .. } => (Some(ValueKind::Tree), None),
        ControlNode::VuStereo { .. } | ControlNode::VuVertical { .. } => {
            (Some(ValueKind::Stereo), Some(ValueKind::Scalar))
        }
        ControlNode::Row { .. } | ControlNode::Column { .. } => (None, Some(ValueKind::Scalar)),
        ControlNode::Include { .. }
        | ControlNode::Scroll { .. }
        | ControlNode::Stage { .. }
        | ControlNode::Slot { .. }
        | ControlNode::Brand { .. }
        | ControlNode::Spacer { .. }
        | ControlNode::Divider { .. }
        | ControlNode::PresetSelector { .. }
        | ControlNode::SettingsButton { .. }
        | ControlNode::WindowDrag { .. }
        | ControlNode::TitleBar { .. }
        | ControlNode::WindowControls { .. }
        | ControlNode::Glyph { .. }
        | ControlNode::Select { .. }
        | ControlNode::StatusDot { .. }
        | ControlNode::Swatch { .. }
        | ControlNode::Cell { .. }
        | ControlNode::Shader { .. } => (None, None),
    }
}

const fn binding_parts(
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
        BindingSide::ModelWrite => {
            matches!(
                category,
                EndpointCategory::Command | EndpointCategory::Parameter | EndpointCategory::Model
            )
        }
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
    fn an_object_may_move_a_whole_row() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "shift", transform: (position: (8.0, 0.0)),
                child: Row(children: [Button(id: "play", label: "PLAY")])))"#;
        let doc = parse_module(text, &origin()).unwrap();

        assert!(check_module_node_ids(&doc, &origin()).is_ok());
    }

    #[kithara::test]
    fn an_object_may_not_turn_a_row() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "spin", transform: (rotation: 30.0),
                child: Row(children: [Button(id: "play", label: "PLAY")])))"#;
        let doc = parse_module(text, &origin()).unwrap();

        let error = check_module_node_ids(&doc, &origin()).unwrap_err();

        assert!(matches!(
            error,
            UiDocError::ObjectGroup { child: "Row", .. }
        ));
    }

    #[kithara::test]
    fn an_object_may_not_scale_a_row_either() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "grow", transform: (scale: (2.0, 2.0)),
                child: Row(children: [Button(id: "play", label: "PLAY")])))"#;
        let doc = parse_module(text, &origin()).unwrap();

        assert!(check_module_node_ids(&doc, &origin()).is_err());
    }

    /// A visualiser paints its own pass, so an object over it would move the
    /// box and leave the picture. Refusing beats drawing the wrong answer.
    #[kithara::test]
    fn an_object_may_not_even_move_a_native_pass() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "shift", transform: (position: (8.0, 0.0)),
                child: Vis(id: "scope")))"#;
        let doc = parse_module(text, &origin()).unwrap();

        let error = check_module_node_ids(&doc, &origin()).unwrap_err();

        assert!(matches!(
            error,
            UiDocError::ObjectNative { child: "Vis", .. }
        ));
    }

    /// A still object is the identity, and the identity reaches everything
    /// because it does nothing.
    #[kithara::test]
    fn a_still_object_may_wrap_anything() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "still", child: Vis(id: "scope")))"#;
        let doc = parse_module(text, &origin()).unwrap();

        assert!(check_module_node_ids(&doc, &origin()).is_ok());
    }

    /// The walk ends in a catch-all that records an id and stops, so a
    /// container the walk does not name is validated as a leaf and its children
    /// are never looked at. This test fails the moment `Stage` falls into it.
    #[kithara::test]
    fn a_stage_walks_its_children() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Stage(id: "scene", children: [
                Button(id: "play", label: "PLAY"),
                Button(id: "play", label: "AGAIN"),
            ]))"#;
        let doc = parse_module(text, &origin()).unwrap();

        let error = check_module_node_ids(&doc, &origin()).unwrap_err();

        assert!(matches!(
            error,
            UiDocError::DuplicateId { id, .. } if id == "play"
        ));
    }

    /// Every child of a stage gets the whole box, so a stage is several boxes,
    /// and a turn about one origin would take them apart.
    #[kithara::test]
    fn an_object_may_not_turn_a_stage() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "spin", transform: (rotation: 30.0),
                child: Stage(id: "scene", children: [Button(id: "play", label: "PLAY")])))"#;
        let doc = parse_module(text, &origin()).unwrap();

        let error = check_module_node_ids(&doc, &origin()).unwrap_err();

        assert!(matches!(
            error,
            UiDocError::ObjectGroup { child: "Stage", .. }
        ));
    }

    /// A move carries every box by the same vector, so it reaches a stage the
    /// way it reaches a row.
    #[kithara::test]
    fn an_object_may_move_a_whole_stage() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "shift", transform: (position: (8.0, 0.0)),
                child: Stage(id: "scene", children: [Button(id: "play", label: "PLAY")])))"#;
        let doc = parse_module(text, &origin()).unwrap();

        assert!(check_module_node_ids(&doc, &origin()).is_ok());
    }

    /// A motion computes the phase, so an object carrying both leaves one pose
    /// with two answers. There is no honest rule for ranking them, and inventing
    /// one is what refusing here avoids.
    #[kithara::test]
    fn an_object_may_not_be_driven_twice() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "spin", to: (rotation: 360.0),
                phase: Model(id: "app.phase"),
                motion: (clock: Model(id: "app.time"), duration: 4.0),
                child: Button(id: "play", label: "PLAY")))"#;
        let doc = parse_module(text, &origin()).unwrap();

        let error = check_module_node_ids(&doc, &origin()).unwrap_err();

        assert!(matches!(error, UiDocError::ObjectDrivenTwice { .. }));
    }

    #[kithara::test]
    fn an_object_may_be_driven_by_a_motion_alone() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Object(id: "spin", to: (rotation: 360.0),
                motion: (clock: Model(id: "app.time"), duration: 4.0, repeat: Loop),
                child: Button(id: "play", label: "PLAY")))"#;
        let doc = parse_module(text, &origin()).unwrap();

        assert!(check_module_node_ids(&doc, &origin()).is_ok());
    }

    #[kithara::test]
    fn module_id_with_an_address_separator_is_rejected() {
        let text = r#"(schema: "kithara.module", version: 1, id: "studio.strip",
            root: Button(id: "play", label: "PLAY"))"#;
        let doc = parse_module(text, &origin()).unwrap();
        let error = check_module_id(&doc, &origin()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::InvalidId { id, reason, .. }
                if id == "studio.strip" && reason.contains("'.'")
        ));
    }

    #[kithara::test]
    fn a_container_that_writes_without_an_id_is_rejected() {
        let text = r#"(schema: "kithara.module", version: 1, id: "m",
            root: Row(write: Parameter(id: "deck.tempo.rate"), children: [
                Button(id: "play", label: "PLAY"),
            ]))"#;
        let doc = parse_module(text, &origin()).unwrap();
        let error = check_module_node_ids(&doc, &origin()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnaddressedSurface { path, .. } if path == "root"
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
        let scope = match &document.root {
            ControlNode::ContextBar { scope, .. } => scope.as_ref(),
            _ => None,
        };
        let zoom = match &document.root {
            ControlNode::Wave { zoom, .. } => zoom.as_ref(),
            _ => None,
        };
        let active = match &document.root {
            ControlNode::Text { active, .. } => active.as_ref(),
            _ => None,
        };
        check_controls(
            ControlSite {
                path,
                write,
                scope,
                zoom,
                active,
                control: &document.root,
                read: None,
                columns: &[],
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
        registry.insert(
            EndpointCategory::Model,
            "library.breadcrumb",
            EndpointDesc::new(ValueKind::Text),
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
                columns: &[],
                columns_state: None,
                query: query.as_ref(),
                scope: None,
                zoom: None,
                active: None,
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
    fn wave_zoom_binding_must_be_scalar() {
        let error = check_control(
            r#"Wave(id: "wave", zoom: Model(id: "library.breadcrumb"))"#,
            "deck/wave",
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            UiDocError::BindingType {
                expected,
                got,
                path,
                ..
            } if expected == "Scalar" && got == "Text" && path == "deck/wave"
        ));
    }

    #[kithara::test]
    fn context_scope_items_require_scope_binding() {
        let error = check_control(
            r#"ContextBar(id: "context", scope_items: ["LOCAL"])"#,
            "library/context",
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            UiDocError::InvalidContextScope { path, .. } if path == "library/context"
        ));
    }

    #[kithara::test]
    fn context_scope_binding_must_be_scalar() {
        let write = BindingRef::Parameter {
            id: EndpointId("player.output.volume".into()),
            with: BTreeMap::new(),
        };
        let error = check_control(
            r#"ContextBar(
                id: "context",
                scope_items: ["LOCAL"],
                scope: Model(id: "library.breadcrumb"),
            )"#,
            "library/context",
            Some(&write),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            UiDocError::BindingType {
                expected,
                got,
                path,
                ..
            } if expected == "Scalar" && got == "Text" && path == "library/context"
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
            with,
            id: EndpointId("deck.transport.toggle_play".into()),
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
    fn crossfader_requires_scalar_read_and_write_endpoints() {
        let document = parse_module(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Crossfader(
                    id: "xfade",
                    read: Model(id: "library.breadcrumb"),
                    write: Parameter(id: "player.output.volume"),
                ))"#,
            &origin(),
        )
        .unwrap();
        let ControlNode::Crossfader { read, write, .. } = &document.root else {
            panic!("expected crossfader");
        };

        let error = check_controls(
            ControlSite {
                path: "mixer/xfade",
                control: &document.root,
                read: read.as_ref(),
                write: write.as_ref(),
                columns: &[],
                columns_state: None,
                query: None,
                scope: None,
                zoom: None,
                active: None,
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
            } if expected == "Scalar" && got == "Text" && path == "mixer/xfade"
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
