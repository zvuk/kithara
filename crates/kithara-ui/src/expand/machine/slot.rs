use std::collections::BTreeMap;

use super::expander::{Context, Expander, Frame, child_path, walk};
use crate::{
    consts::CONTENT,
    error::UiDocError,
    expand::{
        Binding, BindingKind, BlockSpec, ControlSite, ExpandedNode, intern_binding,
        structural::{include_at, walk_children},
    },
    ids::{InternId, NodeId, SourceUri},
    module::{BindingRef, ControlNode, Include},
    resolve::{Filled, collection_address},
    size::SizeSpec,
    source::resolve_uri,
};

pub(super) fn expand_slot(
    context: &Context<'_>,
    node: &ControlNode,
    (id, size, default): (&NodeId, Option<SizeSpec>, &[ControlNode]),
    (from, each, select): (&Option<String>, &Option<Include>, &Option<BindingRef>),
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    let select = match select {
        Some(select) => {
            let path = child_path(&context.prefix, id);
            let select = context.substitute(select, &path)?;
            machine.visit(
                ControlSite {
                    read: Some(&select),
                    ..ControlSite::new(node, &path)
                },
                &context.origin,
            )?;
            Some(intern_binding(machine.interner, &select, &context.origin)?)
        }
        None => None,
    };
    let content = context.content.filter(|_| id.0 == CONTENT);
    let fills = match from {
        Some(from) => context
            .set
            .collections
            .get(&collection_address(context.module()?, from))
            .map_or(&[][..], Vec::as_slice),
        None => &[],
    };
    let children = match (each, &select, content) {
        (Some(each), _, _) if !fills.is_empty() => {
            expand_fills(context, id, each, fills, depth, machine)?
        }
        (_, Some(select), _) if !fills.is_empty() => {
            expand_selection(context, id, (select, default), fills, depth, machine)?
        }
        (_, _, Some(content)) => vec![expand_content(context, content, depth, machine)?],
        _ => walk_children(context, default, depth, machine)?,
    };
    Ok(ExpandedNode::Slot {
        size,
        id: machine.interner.intern(&id.0, &context.origin)?,
        select: select.is_some(),
        children,
    })
}

fn expand_selection(
    context: &Context<'_>,
    id: &NodeId,
    (select, default): (&Binding, &[ControlNode]),
    fills: &[Filled],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<Vec<ExpandedNode>, UiDocError> {
    let slot = child_path(&context.prefix, id);
    let mut keys: Vec<InternId> = Vec::with_capacity(fills.len());
    let mut children: Vec<ExpandedNode> = Vec::with_capacity(fills.len() + default.len());
    for (index, fill) in fills.iter().enumerate() {
        let path = format!("{slot}/{}", fill.key);
        let key = machine.interner.intern(&fill.key, &context.origin)?;
        keys.push(key);
        let frame = Frame {
            named: BTreeMap::new(),
            passed: fill_args(context, fill),
            prefix: path.clone(),
            content: None,
        };
        let root = include_at(context, &fill.uri, frame, &[index, 0], depth, machine);
        let block = BlockSpec {
            path: machine.interner.intern(&path, &context.origin)?,
            hidden: selecting(select, Box::new([key]), true),
        };
        children.push(standing(block, root?));
    }
    let keys = keys.into_boxed_slice();
    let path = machine.interner.intern(&slot, &context.origin)?;
    for (index, child) in default.iter().enumerate() {
        let mark = machine.address.len();
        machine.address.extend([fills.len() + index, 0]);
        let node = walk(context, child, depth, machine);
        machine.address.truncate(mark);
        let block = BlockSpec {
            path,
            hidden: selecting(select, keys.clone(), false),
        };
        children.push(standing(block, node?));
    }
    Ok(children)
}

fn selecting(select: &Binding, keys: Box<[InternId]>, invert: bool) -> Binding {
    Binding {
        kind: BindingKind::Selects { keys, invert },
        ..select.clone()
    }
}

fn standing(block: BlockSpec, child: ExpandedNode) -> ExpandedNode {
    ExpandedNode::Optional {
        block,
        child: Box::new(child),
    }
}

fn expand_fills<'a>(
    context: &Context<'a>,
    id: &NodeId,
    each: &Include,
    fills: &'a [Filled],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<Vec<ExpandedNode>, UiDocError> {
    let template = resolve_uri(Some(&context.origin), &each.source)?;
    let slot = child_path(&context.prefix, id);
    let mut children: Vec<ExpandedNode> = Vec::with_capacity(fills.len());
    for (index, fill) in fills.iter().enumerate() {
        let frame = Frame {
            named: BTreeMap::new(),
            passed: fill_args(context, fill),
            prefix: format!("{slot}/{}", fill.key),
            content: Some(&fill.uri),
        };
        children.push(include_at(
            context,
            &template,
            frame,
            &[index],
            depth,
            machine,
        )?);
    }
    Ok(children)
}

fn expand_content(
    context: &Context<'_>,
    fill: &SourceUri,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let frame = Frame {
        named: BTreeMap::new(),
        passed: context.args.clone(),
        prefix: context.prefix.clone(),
        content: None,
    };
    include_at(context, fill, frame, &[0], depth, machine)
}

fn fill_args(context: &Context<'_>, fill: &Filled) -> BTreeMap<String, String> {
    let mut args = context.args.clone();
    args.insert("key".to_owned(), fill.key.clone());
    args.insert("source".to_owned(), fill.key.clone());
    args
}
