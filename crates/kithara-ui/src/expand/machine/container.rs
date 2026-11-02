use super::expander::{Context, Expander, child_path};
use crate::{
    error::UiDocError,
    expand::{
        Binding, ControlSite, ExpandedNode, SlotWrites, SurfaceSpec,
        binding_subst::{intern_binding, intern_optional_binding},
        structural::walk_children,
    },
    ids::{InternId, NodeId},
    module::{BindingRef, ControlNode},
};

/// What a container binds, as the document declared it.
#[derive(Clone, Copy)]
struct Declared<'a> {
    write: Option<&'a BindingRef>,
    reset: Option<&'a BindingRef>,
    entry: Option<&'a BindingRef>,
    active: Option<&'a BindingRef>,
}

fn container_bindings(
    context: &Context<'_>,
    node: &ControlNode,
    id: Option<&NodeId>,
    declared: Declared<'_>,
    machine: &mut Expander<'_, '_>,
) -> Result<(Option<SurfaceSpec>, Option<Binding>), UiDocError> {
    let Declared {
        write,
        reset,
        entry,
        active,
    } = declared;
    if write.is_none() && reset.is_none() && active.is_none() {
        return Ok((None, None));
    }
    let path = id.map_or_else(
        || context.prefix.clone(),
        |id| child_path(&context.prefix, id),
    );
    let write = write
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    let active = active
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    let reset = reset
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    let entry = entry
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    machine.visit(
        ControlSite {
            write: write.as_ref(),
            read: entry.as_ref(),
            active: active.as_ref(),
            writes: SlotWrites {
                reset: reset.as_ref(),
                ..SlotWrites::default()
            },
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    let surface = write
        .as_ref()
        .map(|write| -> Result<SurfaceSpec, UiDocError> {
            Ok(SurfaceSpec {
                path: machine.interner.intern(&path, &context.origin)?,
                write: intern_binding(machine.interner, write, &context.origin)?,
                entry: intern_optional_binding(machine.interner, entry.as_ref(), &context.origin)?,
            })
        })
        .transpose()?;
    let active = intern_optional_binding(machine.interner, active.as_ref(), &context.origin)?;
    Ok((surface, active))
}

fn intern_node_id(
    id: Option<&NodeId>,
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
) -> Result<Option<InternId>, UiDocError> {
    id.map(|id| machine.interner.intern(&id.0, &context.origin))
        .transpose()
}

pub(super) fn expand_row(
    context: &Context<'_>,
    node: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let ControlNode::Row {
        id,
        size,
        measure,
        gap,
        align,
        pad,
        pad_x,
        pad_y,
        frame,
        background,
        background_alpha,
        active,
        active_background,
        frame_color,
        active_frame_color,
        write,
        reset,
        entry,
        children,
    } = node
    else {
        unreachable!("expand_row is called only for a row")
    };
    machine.budget.charge(&context.origin)?;
    let (surface, active) = container_bindings(
        context,
        node,
        id.as_ref(),
        Declared {
            write: write.as_ref(),
            reset: reset.as_ref(),
            entry: entry.as_ref(),
            active: active.as_ref(),
        },
        machine,
    )?;
    Ok(ExpandedNode::Row {
        active,
        surface,
        id: intern_node_id(id.as_ref(), context, machine)?,
        size: *size,
        measure: *measure,
        gap: *gap,
        align: *align,
        pad: *pad,
        pad_x: *pad_x,
        pad_y: *pad_y,
        frame: *frame,
        background: *background,
        background_alpha: *background_alpha,
        active_background: *active_background,
        frame_color: *frame_color,
        active_frame_color: *active_frame_color,
        children: walk_children(context, children, depth, machine)?,
    })
}

pub(super) fn expand_column(
    context: &Context<'_>,
    node: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let ControlNode::Column {
        id,
        size,
        measure,
        gap,
        align,
        pad,
        pad_x,
        pad_y,
        frame,
        frame_color,
        background,
        background_alpha,
        write,
        reset,
        entry,
        children,
    } = node
    else {
        unreachable!("expand_column is called only for a column")
    };
    machine.budget.charge(&context.origin)?;
    let (surface, _) = container_bindings(
        context,
        node,
        id.as_ref(),
        Declared {
            write: write.as_ref(),
            reset: reset.as_ref(),
            entry: entry.as_ref(),
            active: None,
        },
        machine,
    )?;
    Ok(ExpandedNode::Column {
        surface,
        id: intern_node_id(id.as_ref(), context, machine)?,
        size: *size,
        measure: *measure,
        gap: *gap,
        align: *align,
        pad: *pad,
        pad_x: *pad_x,
        pad_y: *pad_y,
        frame: *frame,
        frame_color: *frame_color,
        background: *background,
        background_alpha: *background_alpha,
        children: walk_children(context, children, depth, machine)?,
    })
}
