use std::collections::BTreeSet;

use super::{
    binding::{BindingSide, check_binding},
    measure::{
        Sibling, check_adaptive_measure, check_adaptive_steps, check_block_position,
        check_measured_box, check_reveal, walk_branches,
    },
    path::{NodePath, check_block_id, check_id},
    placed::{check_placement, placements},
};
use crate::{
    error::UiDocError,
    ids::{NodeId, SourceUri},
    module::{ControlNode, ModuleDoc, Pose},
    registry::{EndpointRegistry, ValueKind},
};

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
        Sibling::Among,
    )
}

pub(crate) fn check_module_root(doc: &ModuleDoc, origin: &SourceUri) -> Result<(), UiDocError> {
    let ControlNode::Optional { id, .. } = &doc.root else {
        return Ok(());
    };
    let here = NodePath::default().push(format!("Optional({id})"));
    check_block_position(id, &here, origin, Sibling::Only)
}

pub(super) fn claim(
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

pub(super) fn record(
    id: &str,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    check_id(id, origin)?;
    claim(id, path, origin, seen)
}

pub(super) fn record_block(
    id: &NodeId,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    check_block_id(&id.0, origin)?;
    claim(&id.0, path, origin, seen)
}

/// A stage and what stands in it.
///
/// A placement is named, checked against the scene its magnet may name, and
/// walked; anything else in a stage is walked where the document put it.
fn walk_stage(
    id: &NodeId,
    children: &[ControlNode],
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
) -> Result<(), UiDocError> {
    let here = path.push(format!("Stage({id})"));
    record(&id.0, &here, origin, seen)?;
    let scene = placements(children);
    for (index, child) in children.iter().enumerate() {
        let at = here.push(format!("[{index}]"));
        let ControlNode::Placed {
            id,
            read,
            write,
            magnet,
            child,
            ..
        } = child
        else {
            walk_module(child, &at, origin, seen, Sibling::Among)?;
            continue;
        };
        let here = at.push(format!("Placed({id})"));
        record(&id.0, &here, origin, seen)?;
        check_placement(
            read.as_ref(),
            write.as_ref(),
            magnet.as_ref(),
            &scene,
            &here,
            origin,
        )?;
        walk_module(child, &here, origin, seen, Sibling::Only)?;
    }
    Ok(())
}

pub(super) fn walk_module(
    node: &ControlNode,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
    sibling: Sibling,
) -> Result<(), UiDocError> {
    match node {
        ControlNode::Row {
            id,
            size,
            measure,
            write,
            children,
            ..
        }
        | ControlNode::Column {
            id,
            size,
            measure,
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
            let among = match measure {
                Some(axis) => {
                    check_measured_box(*axis, *size, &here, origin)?;
                    Sibling::Measured
                }
                None => Sibling::Among,
            };
            for (index, child) in children.iter().enumerate() {
                walk_module(child, &here.push(format!("[{index}]")), origin, seen, among)?;
            }
            Ok(())
        }
        ControlNode::Reveal { from, until, child } => {
            let here = path.push("Reveal");
            check_reveal(*from, *until, &here, origin, sibling)?;
            walk_module(child, &here, origin, seen, Sibling::Only)
        }
        ControlNode::Include { id, .. } => {
            record(&id.0, &path.push(format!("Include({id})")), origin, seen)
        }
        ControlNode::Adaptive {
            id,
            measure,
            size,
            base,
            steps,
        } => {
            let here = path.push(format!("Adaptive({id})"));
            record(&id.0, &here, origin, seen)?;
            check_adaptive_measure(id, measure, *size, &here, origin)?;
            check_adaptive_steps(id, steps, &here, origin)?;
            walk_branches(base, steps, &here, origin, seen)
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
        ControlNode::Modal { id, content, .. } => {
            let here = path.push(format!("Modal({id})"));
            record(&id.0, &here, origin, seen)?;
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
        ControlNode::Stage { id, children, .. } => walk_stage(id, children, path, origin, seen),
        ControlNode::Placed { id, .. } => Err(UiDocError::PlacedOutsideStage {
            origin: origin.clone(),
            path: path.push(format!("Placed({id})")).render(),
        }),
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
pub(super) fn one_driver(
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
pub(super) fn single_box(
    transform: &Pose,
    to: Option<&Pose>,
    child: &ControlNode,
    path: &NodePath,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    let travels = to.is_some_and(|to| !to.is_still());
    if transform.is_still() && !travels {
        return Ok(());
    }
    if let Some(child) = native_pass(child) {
        return Err(UiDocError::ObjectNative {
            child,
            origin: origin.clone(),
            path: path.render(),
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
        ControlNode::Modal { .. } => "Modal",
        ControlNode::Include { .. } => "Include",
        _ => return Ok(()),
    };
    Err(UiDocError::ObjectGroup {
        origin: origin.clone(),
        path: path.render(),
        child: group,
    })
}

pub(super) const fn native_pass(child: &ControlNode) -> Option<&'static str> {
    match child {
        ControlNode::Shader { .. } => Some("Shader"),
        ControlNode::Vis { .. } => Some("Vis"),
        ControlNode::Table { .. } => Some("Table"),
        ControlNode::Search { .. } => Some("Search"),
        ControlNode::Tree { .. } => Some("Tree"),
        _ => None,
    }
}

pub(super) const fn control_id(node: &ControlNode) -> Option<&NodeId> {
    match node {
        ControlNode::Row { .. }
        | ControlNode::Adaptive { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Object { .. }
        | ControlNode::Optional { .. }
        | ControlNode::Reveal { .. }
        | ControlNode::Popover { .. }
        | ControlNode::Modal { .. }
        | ControlNode::Placed { .. }
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
        | ControlNode::Custom { id, .. }
        | ControlNode::Lottie { id, .. }
        | ControlNode::Shader { id, .. }
        | ControlNode::PortalMap { id, .. }
        | ControlNode::Range { id, .. }
        | ControlNode::Table { id, .. }
        | ControlNode::Search { id, .. }
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

pub(crate) fn check_module_bindings(
    doc: &ModuleDoc,
    origin: &SourceUri,
    endpoints: &dyn EndpointRegistry,
) -> Result<(), UiDocError> {
    for (binding, side, kind, path) in [
        (
            doc.footer.as_ref(),
            BindingSide::Read,
            ValueKind::Text,
            "root/footer",
        ),
        (
            doc.drop.as_ref().map(|drop| &drop.write),
            BindingSide::Write,
            ValueKind::Record,
            "root/drop",
        ),
        (
            doc.collapse.as_ref(),
            BindingSide::Write,
            ValueKind::Trigger,
            "root/collapse",
        ),
    ] {
        if let Some(binding) = binding {
            check_binding(binding, side, Some(kind), path, origin, endpoints)?;
        }
    }
    Ok(())
}
