use std::collections::BTreeMap;

use super::{
    ExpandedInclude, ExpandedNode,
    machine::{Context, Expander, Frame, child_path, expand_at, walk},
    substitute_map,
};
use crate::{
    error::UiDocError,
    ids::{NodeId, SourceUri},
    module::ControlNode,
    source::resolve_uri,
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
    let target = resolve_uri(Some(&context.origin), source)?;
    let frame = Frame {
        named: args,
        passed: BTreeMap::new(),
        prefix: path,
        content: None,
    };
    include_at(context, &target, frame, &[], depth, machine)
}

pub(super) fn include_at<'a>(
    context: &Context<'a>,
    uri: &SourceUri,
    frame: Frame<'a>,
    at: &[usize],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let mark = machine.address.len();
    machine.address.extend_from_slice(at);
    let root = expand_at(
        context.set,
        uri,
        frame,
        context.instance.clone(),
        depth + 1,
        machine,
    )
    .and_then(|root| {
        let module = &context.set.def(uri)?.id.0;
        machine.includes.push(ExpandedInclude {
            address: machine.address.clone().into_boxed_slice(),
            module: machine.interner.intern(module, uri)?,
        });
        Ok(root)
    });
    machine.address.truncate(mark);
    root
}
