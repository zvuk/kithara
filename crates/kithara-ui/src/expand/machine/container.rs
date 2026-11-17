use super::expander::{Context, Expander, child_path};
use crate::{
    error::UiDocError,
    expand::{
        Binding, ColorSpec, ControlSite, ExpandedNode, SlotWrites, SurfaceSpec,
        binding_subst::{intern_binding, intern_optional_binding},
        site::substitute_color,
        spec::intern_optional_color,
        structural::walk_children,
    },
    ids::{InternId, Interner, NodeId, SourceUri},
    module::{BindingRef, ColorSource, ControlNode},
};

/// What a container binds, as the document declared it.
#[derive(Clone, Copy)]
struct Declared<'a> {
    active: Option<&'a BindingRef>,
    active_background: Option<&'a ColorSource>,
    entry: Option<&'a BindingRef>,
    reset: Option<&'a BindingRef>,
    write: Option<&'a BindingRef>,
}

struct SubstitutedContainer {
    active: Option<BindingRef>,
    active_background: Option<ColorSource>,
    entry: Option<BindingRef>,
    reset: Option<BindingRef>,
    write: Option<BindingRef>,
}

struct ContainerBound {
    active: Option<Binding>,
    active_background: Option<ColorSpec>,
    surface: Option<SurfaceSpec>,
}

impl Declared<'_> {
    const fn has_site_bindings(self, color_read: Option<&BindingRef>) -> bool {
        self.write.is_some()
            || self.reset.is_some()
            || self.active.is_some()
            || color_read.is_some()
    }

    fn substitute(
        self,
        context: &Context<'_>,
        path: &str,
        active_background: Option<ColorSource>,
    ) -> Result<SubstitutedContainer, UiDocError> {
        let sub = |slot: Option<&BindingRef>| {
            slot.map(|binding| context.substitute(binding, path))
                .transpose()
        };
        Ok(SubstitutedContainer {
            active: sub(self.active)?,
            active_background,
            entry: sub(self.entry)?,
            reset: sub(self.reset)?,
            write: sub(self.write)?,
        })
    }
}

fn intern_surface(
    interner: &mut Interner,
    origin: &SourceUri,
    path: &str,
    write: Option<&BindingRef>,
    entry: Option<&BindingRef>,
) -> Result<Option<SurfaceSpec>, UiDocError> {
    let Some(write) = write else {
        return Ok(None);
    };
    Ok(Some(SurfaceSpec {
        path: interner.intern(path, origin)?,
        write: intern_binding(interner, write, origin)?,
        entry: intern_optional_binding(interner, entry, origin)?,
    }))
}

fn container_bindings(
    context: &Context<'_>,
    node: &ControlNode,
    id: Option<&NodeId>,
    declared: Declared<'_>,
    machine: &mut Expander<'_, '_>,
) -> Result<ContainerBound, UiDocError> {
    let path = id.map_or_else(
        || context.prefix.clone(),
        |id| child_path(&context.prefix, id),
    );
    let active_background = declared
        .active_background
        .map(|color| substitute_color(color, context, &path))
        .transpose()?;
    let color_read = active_background.as_ref().and_then(ColorSource::binding);
    if !declared.has_site_bindings(color_read) {
        return Ok(ContainerBound {
            active: None,
            active_background: intern_optional_color(
                machine.interner,
                active_background.as_ref(),
                &context.origin,
            )?,
            surface: None,
        });
    }
    let sub = declared.substitute(context, &path, active_background)?;
    machine.visit(
        ControlSite {
            write: sub.write.as_ref(),
            read: sub.entry.as_ref(),
            active: sub.active.as_ref(),
            color_read: sub
                .active_background
                .as_ref()
                .and_then(ColorSource::binding),
            writes: SlotWrites {
                reset: sub.reset.as_ref(),
                ..SlotWrites::default()
            },
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    Ok(ContainerBound {
        surface: intern_surface(
            machine.interner,
            &context.origin,
            &path,
            sub.write.as_ref(),
            sub.entry.as_ref(),
        )?,
        active: intern_optional_binding(machine.interner, sub.active.as_ref(), &context.origin)?,
        active_background: intern_optional_color(
            machine.interner,
            sub.active_background.as_ref(),
            &context.origin,
        )?,
    })
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
    let ContainerBound {
        active,
        active_background,
        surface,
    } = container_bindings(
        context,
        node,
        id.as_ref(),
        Declared {
            active: active.as_ref(),
            active_background: active_background.as_ref(),
            entry: entry.as_ref(),
            reset: reset.as_ref(),
            write: write.as_ref(),
        },
        machine,
    )?;
    Ok(ExpandedNode::Row {
        active,
        active_background,
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
    let ContainerBound { surface, .. } = container_bindings(
        context,
        node,
        id.as_ref(),
        Declared {
            active: None,
            active_background: None,
            entry: entry.as_ref(),
            reset: reset.as_ref(),
            write: write.as_ref(),
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
