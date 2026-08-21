use crate::{
    compile::{CompiledNode, compiled_min},
    error::UiDocError,
    expand::ExpandedNode,
    ids::{NodeId, SourceUri},
    module::MeasureAxis,
    size::{SizeSpec, axis_min, min_size, rooms},
    skin::SkinDoc,
    validate::NodePath,
};

/// Checks every threshold a module names against the room the tree behind it
/// needs.
pub(crate) fn check_module(
    node: &ExpandedNode,
    skin: &SkinDoc,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    walk(node, &NodePath::default(), skin, origin)
}

/// Checks the thresholds of one layout node, whose branches are whole modules.
pub(crate) fn check_layout_steps(
    id: &NodeId,
    axis: MeasureAxis,
    steps: &[(f32, CompiledNode)],
    skin: &SkinDoc,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    let path = NodePath::default().push(format!("Adaptive({id})"));
    check_steps(steps, axis, &path, origin, |branch| {
        compiled_min(branch, skin)
    })
}

fn walk(
    node: &ExpandedNode,
    path: &NodePath,
    skin: &SkinDoc,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    match node {
        ExpandedNode::Adaptive {
            measure,
            base,
            steps,
            ..
        } => {
            let here = path.push("Adaptive");
            if let Some(axis) = measure.axis() {
                check_steps(steps, axis, &here, origin, |branch| min_size(branch, skin))?;
            }
            walk(base, &here.push("base"), skin, origin)?;
            for (index, (_, branch)) in steps.iter().enumerate() {
                walk(branch, &here.push(format!("steps[{index}]")), skin, origin)?;
            }
            Ok(())
        }
        ExpandedNode::Row {
            measure, children, ..
        }
        | ExpandedNode::Column {
            measure, children, ..
        } => {
            if let Some(axis) = measure {
                check_cells(node, *axis, path, skin, origin)?;
            }
            walk_children(children, path, skin, origin)
        }
        ExpandedNode::Slot { children, .. } => walk_children(children, path, skin, origin),
        ExpandedNode::Optional { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Scroll { child, .. } => walk(child, path, skin, origin),
        ExpandedNode::Reveal { child, .. } => walk(child, &path.push("Reveal"), skin, origin),
        ExpandedNode::Popover {
            anchor, content, ..
        } => {
            let here = path.push("Popover");
            walk(anchor, &here, skin, origin)?;
            walk(content, &here, skin, origin)
        }
        ExpandedNode::Control { .. } => Ok(()),
    }
}

fn walk_children(
    children: &[ExpandedNode],
    path: &NodePath,
    skin: &SkinDoc,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    for (index, child) in children.iter().enumerate() {
        walk(child, &path.push(format!("[{index}]")), skin, origin)?;
    }
    Ok(())
}

/// A step draws in the room its threshold promises, so the branch it holds has
/// to live in that number.
fn check_steps<N>(
    steps: &[(f32, N)],
    axis: MeasureAxis,
    path: &NodePath,
    origin: &SourceUri,
    min: impl Fn(&N) -> SizeSpec,
) -> Result<(), UiDocError> {
    for (index, (from, branch)) in steps.iter().enumerate() {
        let needs = axis_min(min(branch), axis);
        if needs > *from {
            return Err(UiDocError::AdaptiveStepRoom {
                origin: origin.clone(),
                path: path.push(format!("steps[{index}]")).render(),
                axis: axis.name(),
                from: *from,
                needs,
            });
        }
    }
    Ok(())
}

/// The cells standing in a given room have to fit in it.
fn check_cells(
    node: &ExpandedNode,
    axis: MeasureAxis,
    path: &NodePath,
    skin: &SkinDoc,
    origin: &SourceUri,
) -> Result<(), UiDocError> {
    for (room, needs) in rooms(node, axis, skin) {
        if needs > room {
            return Err(UiDocError::RevealRoom {
                origin: origin.clone(),
                path: path.render(),
                axis: axis.name(),
                needs,
                room,
            });
        }
    }
    Ok(())
}
