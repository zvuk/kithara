use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use super::{
    Binding, BlockSpec, Budget, ControlSite, ControlSpec, ControlVisitor, DropSpec, ExpandedModule,
    ExpandedNode, SurfaceSpec,
    binding_subst::{
        intern_binding, intern_optional_binding, intern_optional_text, intern_text, intern_texts,
        resolve_optional_param, resolve_param, substitute_binding, substitute_map,
    },
    site::{ControlFields, ExtraBindingRefs, ExtraBindings},
};
use crate::{
    error::UiDocError,
    ids::{InternId, Interner, NodeId, SourceUri},
    module::{BindingRef, ControlNode, PopoverAlign, PopoverAt, Tone, TrackColumn, WaveStyle},
    param::Param,
    resolve::ModuleSet,
    size::SizeSpec,
    validate,
};

pub(super) struct Context<'a> {
    pub(super) set: &'a ModuleSet,
    pub(super) args: BTreeMap<String, String>,
    pub(super) origin: SourceUri,
    pub(super) prefix: String,
}

impl Context<'_> {
    fn optional_param<T: Clone + DeserializeOwned>(
        &self,
        param: Option<&Param<T>>,
        path: &str,
    ) -> Result<Option<T>, UiDocError> {
        resolve_optional_param(&self.args, &self.origin, param, path)
    }

    fn param<T: Clone + DeserializeOwned>(
        &self,
        param: &Param<T>,
        path: &str,
    ) -> Result<T, UiDocError> {
        resolve_param(&self.args, &self.origin, param, path)
    }

    pub(super) fn substitute(
        &self,
        binding: &BindingRef,
        path: &str,
    ) -> Result<BindingRef, UiDocError> {
        substitute_binding(&self.args, &self.origin, binding, path)
    }
}

/// Cross-cutting expansion state threaded through the recursion: the include
/// depth cap, the shared node budget, and the per-control validation visitor.
pub(crate) struct Expander<'m, 'v> {
    budget: &'m mut Budget,
    visitor: &'m mut ControlVisitor<'v>,
    interner: &'m mut Interner,
    in_popover: bool,
    max_depth: usize,
}

impl<'m, 'v> Expander<'m, 'v> {
    pub(crate) fn new(
        max_depth: usize,
        budget: &'m mut Budget,
        interner: &'m mut Interner,
        visitor: &'m mut ControlVisitor<'v>,
    ) -> Self {
        Self {
            max_depth,
            budget,
            interner,
            visitor,
            in_popover: false,
        }
    }

    pub(crate) fn expand_module(
        &mut self,
        set: &ModuleSet,
        entry: &SourceUri,
        args: &BTreeMap<String, String>,
        prefix: &str,
    ) -> Result<ExpandedModule, UiDocError> {
        let doc = set.defs.get(entry).ok_or_else(|| UiDocError::NotFound {
            origin: entry.clone(),
            rel: entry.0.clone(),
        })?;
        let root = expand_at(set, entry, args.clone(), prefix.to_owned(), 0, self)?;
        let context = Context {
            set,
            origin: entry.clone(),
            args: args.clone(),
            prefix: prefix.to_owned(),
        };
        let footer = doc
            .footer
            .as_ref()
            .map(|binding| {
                let path = format!("{prefix}/footer");
                let binding = context.substitute(binding, &path)?;
                intern_binding(self.interner, &binding, entry)
            })
            .transpose()?;
        let drop = doc
            .drop
            .as_ref()
            .map(|drop| -> Result<DropSpec, UiDocError> {
                let path = format!("{prefix}/drop");
                Ok(DropSpec {
                    write: intern_binding(
                        self.interner,
                        &context.substitute(&drop.write, &path)?,
                        entry,
                    )?,
                    read: intern_binding(
                        self.interner,
                        &context.substitute(&drop.read, &path)?,
                        entry,
                    )?,
                })
            })
            .transpose()?;
        let module = self.interner.intern(&doc.id.0, entry)?;
        let title = doc
            .title
            .as_deref()
            .map(|title| self.interner.intern(title, entry))
            .transpose()?;
        let chip = doc
            .chip
            .as_deref()
            .map(|chip| self.interner.intern(chip, entry))
            .transpose()?;
        let assign = doc
            .assign
            .iter()
            .map(|label| self.interner.intern(label, entry))
            .collect::<Result<Vec<_>, UiDocError>>()?;
        let collapsed = self
            .interner
            .intern(&format!("ui.module.{}.collapsed", doc.id.0), entry)?;
        Ok(ExpandedModule {
            module,
            title,
            chip,
            assign,
            footer,
            drop,
            collapsed,
            root,
            chrome: doc.chrome,
        })
    }
}

fn expand_at(
    set: &ModuleSet,
    uri: &SourceUri,
    args: BTreeMap<String, String>,
    prefix: String,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    if depth > machine.max_depth {
        return Err(UiDocError::DepthExceeded {
            depth,
            origin: uri.clone(),
            max: machine.max_depth,
        });
    }
    let doc = set.defs.get(uri).ok_or_else(|| UiDocError::NotFound {
        origin: uri.clone(),
        rel: uri.0.clone(),
    })?;
    for name in args.keys() {
        if !doc.parameters.contains(name) {
            return Err(UiDocError::UnknownParam {
                origin: uri.clone(),
                name: name.clone(),
                path: prefix.clone(),
            });
        }
    }
    let context = Context {
        set,
        args,
        prefix,
        origin: uri.clone(),
    };
    walk(&context, &doc.root, depth, machine)
}

fn context_bar_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    scope_items: &[String],
    scope: Option<&BindingRef>,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::ContextBar {
        scope_items: intern_texts(context, interner, scope_items, path, &context.origin)?,
        scope: intern_optional_binding(interner, scope, &context.origin)?,
    })
}

fn status_dot_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    label: &str,
    tone: Tone,
    active_tone: Option<Tone>,
    extra: &ExtraBindings,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::StatusDot {
        label: intern_text(context, machine.interner, label, path, &context.origin)?,
        tone,
        active_tone,
        active: optional_binding(context, machine, extra.active.as_ref())?,
    })
}

fn title_bar_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    label: &str,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::TitleBar {
        label: intern_text(context, machine.interner, label, path, &context.origin)?,
    })
}

fn child_path(prefix: &str, id: &NodeId) -> String {
    if prefix.is_empty() {
        id.0.clone()
    } else {
        format!("{prefix}/{id}")
    }
}

fn begin_control(
    context: &Context<'_>,
    id: &NodeId,
    machine: &mut Expander<'_, '_>,
) -> Result<String, UiDocError> {
    machine.budget.charge(&context.origin)?;
    Ok(child_path(&context.prefix, id))
}

fn finish_control(
    context: &Context<'_>,
    control: &ControlNode,
    fields: ControlFields<'_>,
    path: &str,
    extra: ExtraBindingRefs<'_>,
    spec: ControlSpec,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let read = fields
        .read
        .map(|binding| context.substitute(binding, path))
        .transpose()?;
    let write = fields
        .write
        .map(|binding| context.substitute(binding, path))
        .transpose()?;
    (machine.visitor)(
        ControlSite {
            path,
            control,
            read: read.as_ref(),
            write: write.as_ref(),
            columns_state: extra.columns_state,
            query: extra.query,
            scope: extra.scope,
            zoom: extra.zoom,
            active: extra.active,
        },
        &context.origin,
    )?;
    Ok(ExpandedNode::Control {
        spec,
        path: machine.interner.intern(path, &context.origin)?,
        id: machine.interner.intern(&fields.id.0, &context.origin)?,
        size: fields.size,
        read: read
            .as_ref()
            .map(|binding| intern_binding(machine.interner, binding, &context.origin))
            .transpose()?,
        write: write
            .as_ref()
            .map(|binding| intern_binding(machine.interner, binding, &context.origin))
            .transpose()?,
        adaptive: fields.adaptive.clone(),
    })
}

fn expand_control(
    context: &Context<'_>,
    control: &ControlNode,
    fields: ControlFields<'_>,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let path = begin_control(context, fields.id, machine)?;
    let extra = ExtraBindings::substitute(context, control, &path)?;
    let Some(spec) = control_spec(context, control, &extra, &path, machine)? else {
        return walk(context, control, depth, machine);
    };
    finish_control(
        context,
        control,
        fields,
        &path,
        extra.as_refs(),
        spec,
        machine,
    )
}

/// A container node has no spec of its own; the caller keeps walking it.
fn control_spec(
    context: &Context<'_>,
    control: &ControlNode,
    extra: &ExtraBindings,
    path: &str,
    machine: &mut Expander<'_, '_>,
) -> Result<Option<ControlSpec>, UiDocError> {
    let spec = match control {
        ControlNode::DeckSummary { style, .. } => ControlSpec::DeckSummary { style: *style },
        ControlNode::Brand { .. } => ControlSpec::Brand,
        ControlNode::Spacer { .. } => ControlSpec::Spacer,
        ControlNode::Divider { .. } => ControlSpec::Divider,
        ControlNode::PresetSelector { .. } => ControlSpec::PresetSelector,
        ControlNode::SettingsButton { .. } => ControlSpec::SettingsButton,
        ControlNode::WindowDrag { .. } => ControlSpec::WindowDrag,
        ControlNode::WindowControls { style, .. } => ControlSpec::WindowControls { style: *style },
        ControlNode::Glyph {
            icon,
            active_icon,
            style,
            color,
            active_color,
            ..
        } => ControlSpec::Glyph {
            icon: context.param(icon, path)?,
            active_icon: context.optional_param(active_icon.as_ref(), path)?,
            style: *style,
            color: context.optional_param(color.as_ref(), path)?,
            active_color: context.optional_param(active_color.as_ref(), path)?,
            active: optional_binding(context, machine, extra.active.as_ref())?,
        },
        ControlNode::Time { .. } => ControlSpec::Time,
        ControlNode::Scalar { format, framed, .. } => ControlSpec::Scalar {
            format: *format,
            framed: *framed,
        },
        ControlNode::Crossfader { ticks, .. } => ControlSpec::Crossfader { ticks: *ticks },
        ControlNode::Vis { .. } => ControlSpec::Vis,
        ControlNode::Toggle { .. } => ControlSpec::Toggle,
        ControlNode::Checkbox { .. } => ControlSpec::Checkbox,
        ControlNode::Meter { .. } => ControlSpec::Meter,
        ControlNode::VuStereo { .. } => ControlSpec::VuStereo,
        ControlNode::VuVertical { ticks, .. } => ControlSpec::VuVertical { ticks: *ticks },
        ControlNode::TitleBar { label, .. } => title_bar_spec(context, machine, label, path)?,
        ControlNode::Text {
            style,
            label,
            align,
            color,
            active_color,
            ..
        } => ControlSpec::Text {
            style: *style,
            label: optional_text(context, machine, label.as_deref(), path)?,
            color: *color,
            active_color: *active_color,
            active: optional_binding(context, machine, extra.active.as_ref())?,
            align: *align,
        },
        ControlNode::NavItem { label, icon, .. } => ControlSpec::NavItem {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
            icon: context.param(icon, path)?,
        },
        ControlNode::TabLarge { label, .. } => ControlSpec::TabLarge {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
        },
        ControlNode::Button {
            label,
            icon,
            active_label,
            style,
            frame,
            ..
        } => ControlSpec::Button {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
            icon: context.optional_param(icon.as_ref(), path)?,
            active_label: optional_text(context, machine, active_label.as_deref(), path)?,
            style: *style,
            frame: *frame,
        },
        ControlNode::Bpm { placeholder, .. } => ControlSpec::Bpm {
            placeholder: optional_text(context, machine, placeholder.as_deref(), path)?,
        },
        ControlNode::Fader { style, label, .. } => ControlSpec::Fader {
            style: *style,
            label: optional_text(context, machine, label.as_deref(), path)?,
        },
        ControlNode::Wave { style, badge, .. } => wave_spec(
            context,
            machine,
            *style,
            badge.as_deref(),
            extra.zoom.as_ref(),
            path,
        )?,
        ControlNode::TrackList { columns, .. } => {
            track_list_spec(context, machine, columns, extra)?
        }
        ControlNode::Tree { .. } => ControlSpec::Tree {
            query: optional_binding(context, machine, extra.query.as_ref())?,
        },
        ControlNode::ContextBar { scope_items, .. } => context_bar_spec(
            context,
            machine.interner,
            scope_items,
            extra.scope.as_ref(),
            path,
        )?,
        ControlNode::Segmented { items, .. } => ControlSpec::Segmented {
            items: intern_texts(context, machine.interner, items, path, &context.origin)?,
        },
        ControlNode::Select { label, .. } => ControlSpec::Select {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
        },
        ControlNode::StatusDot {
            label,
            tone,
            active_tone,
            ..
        } => status_dot_spec(context, machine, label, *tone, *active_tone, extra, path)?,
        ControlNode::Swatch { role, label, .. } => ControlSpec::Swatch {
            role: *role,
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
        },
        ControlNode::Cell {
            label, highlighted, ..
        } => ControlSpec::Cell {
            label: optional_text(context, machine, label.as_deref(), path)?,
            highlighted: *highlighted,
        },
        ControlNode::Readout {
            label,
            tone,
            framed,
            ..
        } => ControlSpec::Readout {
            label: optional_text(context, machine, label.as_deref(), path)?,
            tone: *tone,
            framed: *framed,
        },
        ControlNode::Chip { label, style, .. } => ControlSpec::Chip {
            label: intern_text(context, machine.interner, label, path, &context.origin)?,
            style: *style,
        },
        ControlNode::Knob { label, .. } => ControlSpec::Knob {
            label: optional_text(context, machine, label.as_deref(), path)?,
        },
        ControlNode::Row { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Optional { .. }
        | ControlNode::Popover { .. }
        | ControlNode::Pressable { .. }
        | ControlNode::Slot { .. } => return Ok(None),
    };
    Ok(Some(spec))
}

fn optional_text(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    label: Option<&str>,
    path: &str,
) -> Result<Option<InternId>, UiDocError> {
    intern_optional_text(context, machine.interner, label, path, &context.origin)
}

fn optional_binding(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    binding: Option<&BindingRef>,
) -> Result<Option<Binding>, UiDocError> {
    intern_optional_binding(machine.interner, binding, &context.origin)
}

fn wave_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    style: WaveStyle,
    badge: Option<&str>,
    zoom: Option<&BindingRef>,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Wave {
        style,
        badge: intern_optional_text(context, machine.interner, badge, path, &context.origin)?,
        zoom: intern_optional_binding(machine.interner, zoom, &context.origin)?,
    })
}

fn track_list_spec(
    context: &Context<'_>,
    machine: &mut Expander<'_, '_>,
    columns: &[TrackColumn],
    extra: &ExtraBindings,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::TrackList {
        columns: columns.to_vec(),
        columns_state: intern_optional_binding(
            machine.interner,
            extra.columns_state.as_ref(),
            &context.origin,
        )?,
    })
}

fn expand_optional(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    hidden: &BindingRef,
    child: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    validate::check_block_path(&path, &context.origin)?;
    let hidden = context.substitute(hidden, &path)?;
    (machine.visitor)(
        ControlSite {
            path: &path,
            control: node,
            read: Some(&hidden),
            write: None,
            columns_state: None,
            query: None,
            scope: None,
            zoom: None,
            active: None,
        },
        &context.origin,
    )?;
    Ok(ExpandedNode::Optional {
        block: BlockSpec {
            path: machine.interner.intern(&path, &context.origin)?,
            hidden: intern_binding(machine.interner, &hidden, &context.origin)?,
        },
        child: Box::new(walk(context, child, depth, machine)?),
    })
}

fn expand_popover(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    declared: (&BindingRef, PopoverAt, PopoverAlign),
    subtrees: (&ControlNode, &ControlNode),
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let ((open, at, align), (anchor, content)) = (declared, subtrees);
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    if machine.in_popover {
        return Err(UiDocError::InvalidId {
            origin: context.origin.clone(),
            id: path,
            reason: "a popover must not open inside another popover".to_owned(),
        });
    }
    let open = context.substitute(open, &path)?;
    (machine.visitor)(
        ControlSite {
            path: &path,
            control: node,
            read: Some(&open),
            write: None,
            columns_state: None,
            query: None,
            scope: None,
            zoom: None,
            active: None,
        },
        &context.origin,
    )?;
    let anchor = walk(context, anchor, depth, machine)?;
    machine.in_popover = true;
    let content = walk(context, content, depth, machine);
    machine.in_popover = false;
    Ok(ExpandedNode::Popover {
        at,
        align,
        path: machine.interner.intern(&path, &context.origin)?,
        open: intern_binding(machine.interner, &open, &context.origin)?,
        anchor: Box::new(anchor),
        content: Box::new(content?),
    })
}

fn expand_pressable(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    press: &BindingRef,
    child: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    let press = context.substitute(press, &path)?;
    (machine.visitor)(
        ControlSite {
            path: &path,
            control: node,
            read: None,
            write: Some(&press),
            columns_state: None,
            query: None,
            scope: None,
            zoom: None,
            active: None,
        },
        &context.origin,
    )?;
    Ok(ExpandedNode::Pressable {
        path: machine.interner.intern(&path, &context.origin)?,
        press: intern_binding(machine.interner, &press, &context.origin)?,
        child: Box::new(walk(context, child, depth, machine)?),
    })
}

fn walk_children(
    context: &Context<'_>,
    children: &[ControlNode],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<Vec<ExpandedNode>, UiDocError> {
    children
        .iter()
        .map(|child| walk(context, child, depth, machine))
        .collect()
}

fn expand_slot(
    context: &Context<'_>,
    id: &NodeId,
    size: Option<SizeSpec>,
    default: &[ControlNode],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    Ok(ExpandedNode::Slot {
        size,
        id: machine.interner.intern(&id.0, &context.origin)?,
        children: walk_children(context, default, depth, machine)?,
    })
}

fn expand_include(
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
    expand_at(context.set, &target, args, path, depth + 1, machine)
}

fn container_bindings(
    context: &Context<'_>,
    node: &ControlNode,
    declared: (Option<&NodeId>, Option<&BindingRef>, Option<&BindingRef>),
    machine: &mut Expander<'_, '_>,
) -> Result<(Option<SurfaceSpec>, Option<Binding>), UiDocError> {
    let (id, write, active) = declared;
    if write.is_none() && active.is_none() {
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
    (machine.visitor)(
        ControlSite {
            path: &path,
            control: node,
            read: None,
            write: write.as_ref(),
            columns_state: None,
            query: None,
            scope: None,
            zoom: None,
            active: active.as_ref(),
        },
        &context.origin,
    )?;
    let surface = write
        .as_ref()
        .map(|write| -> Result<SurfaceSpec, UiDocError> {
            Ok(SurfaceSpec {
                path: machine.interner.intern(&path, &context.origin)?,
                write: intern_binding(machine.interner, write, &context.origin)?,
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

fn walk(
    context: &Context<'_>,
    node: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match node {
        ControlNode::Row {
            id,
            size,
            gap,
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
            children,
        } => {
            machine.budget.charge(&context.origin)?;
            let declared = (id.as_ref(), write.as_ref(), active.as_ref());
            let (surface, active) = container_bindings(context, node, declared, machine)?;
            Ok(ExpandedNode::Row {
                active,
                surface,
                id: intern_node_id(id.as_ref(), context, machine)?,
                size: *size,
                gap: *gap,
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
        ControlNode::Column {
            id,
            size,
            gap,
            align,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            write,
            children,
        } => {
            machine.budget.charge(&context.origin)?;
            let declared = (id.as_ref(), write.as_ref(), None);
            let (surface, _) = container_bindings(context, node, declared, machine)?;
            Ok(ExpandedNode::Column {
                surface,
                id: intern_node_id(id.as_ref(), context, machine)?,
                size: *size,
                gap: *gap,
                align: *align,
                pad: *pad,
                pad_x: *pad_x,
                pad_y: *pad_y,
                frame: *frame,
                background: *background,
                background_alpha: *background_alpha,
                children: walk_children(context, children, depth, machine)?,
            })
        }
        ControlNode::Slot { id, size, default } => {
            expand_slot(context, id, *size, default, depth, machine)
        }
        ControlNode::Optional { id, hidden, child } => {
            expand_optional(context, node, id, hidden, child, depth, machine)
        }
        ControlNode::Popover {
            id,
            open,
            at,
            align,
            anchor,
            content,
        } => {
            let subtrees = (anchor.as_ref(), content.as_ref());
            expand_popover(
                context,
                node,
                id,
                (open, *at, *align),
                subtrees,
                depth,
                machine,
            )
        }
        ControlNode::Pressable { id, press, child } => {
            expand_pressable(context, node, id, press, child, depth, machine)
        }
        ControlNode::Include { id, source, with } => {
            expand_include(context, id, source, with, depth, machine)
        }
        control @ (ControlNode::DeckSummary { id, adaptive, .. }
        | ControlNode::Brand { id, adaptive, .. }
        | ControlNode::Spacer { id, adaptive, .. }
        | ControlNode::Meter { id, adaptive, .. }
        | ControlNode::Divider { id, adaptive, .. }
        | ControlNode::PresetSelector { id, adaptive, .. }
        | ControlNode::SettingsButton { id, adaptive, .. }
        | ControlNode::WindowDrag { id, adaptive, .. }
        | ControlNode::TitleBar { id, adaptive, .. }
        | ControlNode::WindowControls { id, adaptive, .. }
        | ControlNode::Text { id, adaptive, .. }
        | ControlNode::Glyph { id, adaptive, .. }
        | ControlNode::NavItem { id, adaptive, .. }
        | ControlNode::TabLarge { id, adaptive, .. }
        | ControlNode::Button { id, adaptive, .. }
        | ControlNode::Bpm { id, adaptive, .. }
        | ControlNode::Time { id, adaptive, .. }
        | ControlNode::Scalar { id, adaptive, .. }
        | ControlNode::Crossfader { id, adaptive, .. }
        | ControlNode::Fader { id, adaptive, .. }
        | ControlNode::Wave { id, adaptive, .. }
        | ControlNode::Vis { id, adaptive, .. }
        | ControlNode::TrackList { id, adaptive, .. }
        | ControlNode::Tree { id, adaptive, .. }
        | ControlNode::ContextBar { id, adaptive, .. }
        | ControlNode::Toggle { id, adaptive, .. }
        | ControlNode::Checkbox { id, adaptive, .. }
        | ControlNode::Segmented { id, adaptive, .. }
        | ControlNode::Select { id, adaptive, .. }
        | ControlNode::StatusDot { id, adaptive, .. }
        | ControlNode::Swatch { id, adaptive, .. }
        | ControlNode::Cell { id, adaptive, .. }
        | ControlNode::Readout { id, adaptive, .. }
        | ControlNode::Chip { id, adaptive, .. }
        | ControlNode::Knob { id, adaptive, .. }
        | ControlNode::VuStereo { id, adaptive, .. }
        | ControlNode::VuVertical { id, adaptive, .. }) => {
            let (read, write) = control.bindings();
            expand_control(
                context,
                control,
                ControlFields::new(id, control.size().copied(), read, write, adaptive),
                depth,
                machine,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        expand::{Binding, BindingKind},
        ids::StrArena,
        resolve::load_module_graph,
        source::{Limits, MemResolver},
    };

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn deck_fixture() -> MemResolver {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "deck.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
                root: Column(children: [
                    Include(id: "transport", source: "deck/transport.kmodule.ron", with: { "deck": "$deck" }),
                ]))"#,
        );
        resolver.insert(
            "deck/transport.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "transport", parameters: ["deck"],
                root: Row(children: [
                    Button(id: "play", label: "PLAY",
                        write: Command(id: "deck.transport.toggle_play", with: { "deck": "$deck" })),
                ]))"#,
        );
        resolver
    }

    fn expand(
        set: &ModuleSet,
        uri: &SourceUri,
        args: &BTreeMap<String, String>,
    ) -> Result<(ExpandedNode, StrArena), UiDocError> {
        let mut budget = Budget::new(Limits::default().max_nodes);
        let mut interner = Interner::new(64 * 1024);
        let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());
        let module = Expander::new(
            Limits::default().max_depth,
            &mut budget,
            &mut interner,
            &mut visitor,
        )
        .expand_module(set, uri, args, "")?;
        Ok((module.root, interner.finish()))
    }

    fn expand_with_depth(
        set: &ModuleSet,
        uri: &SourceUri,
        max_depth: usize,
    ) -> Result<(ExpandedNode, StrArena), UiDocError> {
        let mut budget = Budget::new(Limits::default().max_nodes);
        let mut interner = Interner::new(64 * 1024);
        let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());
        let module = Expander::new(max_depth, &mut budget, &mut interner, &mut visitor)
            .expand_module(set, uri, &BTreeMap::new(), "")?;
        Ok((module.root, interner.finish()))
    }

    fn depth_fixture(reverse: bool) -> MemResolver {
        let shallow = r#"Include(id: "shallow", source: "c.kmodule.ron")"#;
        let deep = r#"Include(id: "deep", source: "b.kmodule.ron")"#;
        let children = if reverse {
            format!("{deep}, {shallow}")
        } else {
            format!("{shallow}, {deep}")
        };
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &format!(
                r#"(schema: "kithara.module", version: 1, id: "a",
                    root: Row(children: [{children}]))"#
            ),
        );
        resolver.insert(
            "b.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "b",
                root: Include(id: "c", source: "c.kmodule.ron"))"#,
        );
        resolver.insert(
            "c.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "c",
                root: Include(id: "d", source: "d.kmodule.ron"))"#,
        );
        resolver.insert(
            "d.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "d",
                root: Text(id: "leaf"))"#,
        );
        resolver
    }

    #[kithara::test]
    fn nested_include_receives_substituted_args() {
        let resolver = deck_fixture();
        let (uri, set) =
            load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
        let (root, arena) = expand(&set, &uri, &args(&[("deck", "b")])).unwrap();

        let ExpandedNode::Column { children, .. } = root else {
            panic!("expected column");
        };
        let ExpandedNode::Row { children, .. } = &children[0] else {
            panic!("expected row");
        };
        let ExpandedNode::Control { path, write, .. } = &children[0] else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(*path), "transport/play");
        let Some(Binding {
            kind: BindingKind::Command,
            with,
            ..
        }) = write
        else {
            panic!("expected command");
        };
        let deck = with
            .iter()
            .find(|(key, _)| arena.resolve(**key) == "deck")
            .map(|(_, value)| arena.resolve(*value));
        assert_eq!(deck, Some("b"));
    }

    #[kithara::test]
    fn missing_argument_is_unresolved_param() {
        let resolver = deck_fixture();
        let (uri, set) =
            load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
        let error = expand(&set, &uri, &BTreeMap::new()).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnresolvedParam { name, .. } if name == "deck"
        ));
    }

    #[kithara::test]
    fn undeclared_argument_is_rejected() {
        let resolver = deck_fixture();
        let (uri, set) =
            load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
        let error = expand(&set, &uri, &args(&[("deck", "a"), ("bogus", "1")])).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnknownParam { name, .. } if name == "bogus"
        ));
    }

    #[kithara::test]
    fn undeclared_include_argument_reports_instance_path() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "deck.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
                root: Include(id: "transport", source: "transport.kmodule.ron", with: {
                    "deck": "$deck",
                    "bogus": "1",
                }))"#,
        );
        resolver.insert(
            "transport.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "transport", parameters: ["deck"],
                root: Button(id: "play", label: "PLAY"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
        let limits = Limits::default();
        let mut budget = Budget::new(limits.max_nodes);
        let mut interner = Interner::new(64 * 1024);
        let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());

        let error = Expander::new(limits.max_depth, &mut budget, &mut interner, &mut visitor)
            .expand_module(&set, &uri, &args(&[("deck", "a")]), "deck-a")
            .unwrap_err();
        let message = error.to_string();
        assert!(matches!(
            error,
            UiDocError::UnknownParam { name, .. } if name == "bogus"
        ));
        assert!(message.contains("deck-a/transport"), "{message}");
    }

    #[kithara::test]
    fn doubled_dollar_expands_to_literal_dollar() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "price.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "price",
                root: Chip(id: "price", label: "$$5.99"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "price.kmodule.ron", &Limits::default()).unwrap();

        let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
        let ExpandedNode::Control {
            spec: ControlSpec::Chip { label, .. },
            ..
        } = root
        else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(label), "$5.99");
    }

    #[kithara::test]
    fn expansion_depth_limit_is_independent_of_include_order() {
        for reverse in [false, true] {
            let resolver = depth_fixture(reverse);
            let (uri, set) =
                load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap();
            let error = expand_with_depth(&set, &uri, 2).unwrap_err();
            assert!(matches!(
                error,
                UiDocError::DepthExceeded {
                    origin,
                    depth: 3,
                    max: 2,
                } if origin.0 == "d.kmodule.ron"
            ));
        }
    }
}
