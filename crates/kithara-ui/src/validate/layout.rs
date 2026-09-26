use std::collections::BTreeSet;

use super::{
    binding::{BindingSide, check_binding, consts::BLOCK_HIDDEN},
    measure::{Sibling, check_block_position, check_measured_box, check_reveal, check_thresholds},
    module::{claim, record_block},
    path::{NodePath, check_id, check_state_id},
};
use crate::{
    error::UiDocError,
    ids::{NodeId, SourceUri},
    layout::{LayoutDoc, LayoutNode},
    module::{BindingRef, MeasureAxis},
    registry::{EndpointRegistry, ValueKind},
    size::SizeSpec,
};

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

/// Two pages never stand at once, so each claims its instances against what stood before the tabs
/// rather than against its sibling pages.
pub(super) fn walk_layout(
    node: &LayoutNode,
    path: &NodePath,
    origin: &SourceUri,
    seen: &mut BTreeSet<String>,
    sibling: Sibling,
) -> Result<(), UiDocError> {
    match node {
        LayoutNode::Split {
            measure,
            size,
            children,
            ..
        } => {
            let among = match measure {
                Some(axis) => {
                    check_measured_box(*axis, *size, &path.push("Split"), origin)?;
                    Sibling::Measured
                }
                None => Sibling::Among,
            };
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
                if child.from != 0.0 || child.until.is_some() {
                    check_reveal(child.from, child.until, &child_path, origin, among)?;
                }
                walk_layout(&child.node, &child_path, origin, seen, among)?;
            }
            Ok(())
        }
        LayoutNode::Optional { id, node, .. } => {
            let here = path.push(format!("Optional({id})"));
            check_block_position(id, &here, origin, sibling)?;
            record_block(id, &here, origin, seen)?;
            walk_layout(node, &here, origin, seen, Sibling::Only)
        }
        LayoutNode::Adaptive {
            id, base, steps, ..
        } => {
            let here = path.push(format!("Adaptive({id})"));
            check_id(&id.0, origin)?;
            check_layout_steps(id, steps, &here, origin)?;
            let taken = seen.clone();
            for (index, branch) in std::iter::once((0, base.as_ref())).chain(
                steps
                    .iter()
                    .enumerate()
                    .map(|(index, step)| (index + 1, &step.node)),
            ) {
                let mut claimed = taken.clone();
                walk_layout(
                    branch,
                    &here.push(format!("[{index}]")),
                    origin,
                    &mut claimed,
                    Sibling::Only,
                )?;
                seen.extend(claimed);
            }
            Ok(())
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
        LayoutNode::Tabs {
            state,
            initial,
            pages,
        } => {
            check_state_id(&state.0, origin)?;
            let here = path.push(format!("Tabs({state})"));
            if !pages.contains_key(initial) {
                return Err(UiDocError::UnknownPage {
                    origin: origin.clone(),
                    id: state.0.clone(),
                    page: initial.clone(),
                    path: here.render(),
                });
            }
            let taken = seen.clone();
            for (page, node) in pages {
                let mut claimed = taken.clone();
                walk_layout(
                    node,
                    &here.push(format!("[{page}]")),
                    origin,
                    &mut claimed,
                    Sibling::Only,
                )?;
                seen.extend(claimed);
            }
            Ok(())
        }
    }
}

pub(crate) fn check_layout_measure(
    id: &NodeId,
    measure: MeasureAxis,
    size: SizeSpec,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    let path = NodePath::default().push(format!("Adaptive({id})"));
    check_measured_box(measure, Some(size), &path, origin)
}

pub(super) fn check_layout_steps(
    id: &NodeId,
    steps: &[crate::layout::AdaptiveStep],
    path: &NodePath,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    check_thresholds(id, steps.iter().map(|step| step.from), path, origin)
}

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
