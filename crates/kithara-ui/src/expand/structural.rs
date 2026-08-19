use std::collections::BTreeMap;

use super::{
    ExpandedInclude, ExpandedNode,
    machine::{Context, Expander, expand_at, walk},
    substitute_map,
};
use crate::{
    error::UiDocError,
    ids::{NodeId, SourceUri},
    module::ControlNode,
};

pub(super) fn walk_children(
    context: &Context<'_>,
    children: &[ControlNode],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<Vec<ExpandedNode>, UiDocError> {
    children
        .iter()
        .enumerate()
        .map(|(index, child)| walk_child(context, child, index, depth, machine))
        .collect()
}

pub(super) fn walk_child(
    context: &Context<'_>,
    child: &ControlNode,
    index: usize,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.address.push(index);
    let result = walk(context, child, depth, machine);
    machine.address.pop();
    result
}

pub(super) fn expand_include(
    context: &Context<'_>,
    id: &NodeId,
    source: &str,
    with: &BTreeMap<String, String>,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let path = child_path(&context.prefix, id);
    let args = substitute_map(&context.args, &context.origin, with, &path)?;
    let target = crate::source::join_rel(crate::source::base_dir(Some(&context.origin)), source)
        .map(SourceUri)
        .ok_or_else(|| UiDocError::RootEscape {
            origin: context.origin.clone(),
            rel: source.to_owned(),
        })?;
    let module = context
        .set
        .defs
        .get(&target)
        .ok_or_else(|| UiDocError::NotFound {
            origin: target.clone(),
            rel: target.0.clone(),
        })?
        .id
        .0
        .clone();
    let root = expand_at(context.set, &target, args, path.clone(), depth + 1, machine)?;
    machine.includes.push(ExpandedInclude {
        address: machine.address.clone().into_boxed_slice(),
        module: machine.interner.intern(&module, &target)?,
    });
    Ok(root)
}

fn child_path(prefix: &str, id: &NodeId) -> String {
    if prefix.is_empty() {
        id.0.clone()
    } else {
        format!("{prefix}/{}", id.0)
    }
}
