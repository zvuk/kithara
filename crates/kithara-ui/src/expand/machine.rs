use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use super::{
    Binding, BlockSpec, Budget, ControlSite, ControlSpec, ControlVisitor, DropSpec, ExpandedModule,
    ExpandedNode, MeasureSpec, SurfaceSpec,
    binding_subst::{
        intern_binding, intern_module_text, intern_module_text_opt, intern_optional_binding,
        resolve_optional_param, resolve_param, substitute_binding, substitute_map,
    },
    site::{ControlFields, ExtraBindingRefs, ExtraBindings},
    spec::control_spec,
};
use crate::{
    error::UiDocError,
    ids::{InternId, Interner, NodeId, SourceUri},
    module::{AdaptiveStep, BindingRef, ControlNode, Measure, PopoverAlign, PopoverAt},
    param::Param,
    resolve::ModuleSet,
    size::SizeSpec,
    text::TextDoc,
    validate,
};

pub(super) struct Context<'a> {
    pub(super) set: &'a ModuleSet,
    pub(super) text: &'a TextDoc,
    pub(super) args: BTreeMap<String, String>,
    pub(super) origin: SourceUri,
    pub(super) prefix: String,
}

impl Context<'_> {
    pub(super) fn optional_param<T: Clone + DeserializeOwned>(
        &self,
        param: Option<&Param<T>>,
        path: &str,
    ) -> Result<Option<T>, UiDocError> {
        resolve_optional_param(&self.args, &self.origin, param, path)
    }

    pub(super) fn param<T: Clone + DeserializeOwned>(
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
    text: &'m TextDoc,
    in_popover: bool,
    max_depth: usize,
}

impl<'m, 'v> Expander<'m, 'v> {
    pub(crate) fn new(
        max_depth: usize,
        budget: &'m mut Budget,
        interner: &'m mut Interner,
        text: &'m TextDoc,
        visitor: &'m mut ControlVisitor<'v>,
    ) -> Self {
        Self {
            max_depth,
            budget,
            interner,
            text,
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
            text: self.text,
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
        let (interner, text) = (&mut *self.interner, self.text);
        let title =
            intern_module_text_opt(interner, text, doc.title.as_deref(), prefix, "title", entry)?;
        let chip =
            intern_module_text_opt(interner, text, doc.chip.as_deref(), prefix, "chip", entry)?;
        let assign = doc
            .assign
            .iter()
            .enumerate()
            .map(|(i, label)| {
                intern_module_text(interner, text, label, prefix, &format!("assign/{i}"), entry)
            })
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
                path: prefix,
            });
        }
    }
    let context = Context {
        set,
        text: machine.text,
        args,
        prefix,
        origin: uri.clone(),
    };
    walk(&context, &doc.root, depth, machine)
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
    let Some(spec) = control_spec(context, control, &extra, &path, machine.interner)? else {
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

fn expand_adaptive(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    declared: (&Measure, Option<SizeSpec>),
    branches: (&ControlNode, &[AdaptiveStep]),
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let ((measure, size), (base, steps)) = (declared, branches);
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    let read = match measure {
        Measure::Read(binding) => Some(context.substitute(binding, &path)?),
        Measure::Width | Measure::Height => None,
    };
    (machine.visitor)(
        ControlSite {
            path: &path,
            control: node,
            read: read.as_ref(),
            write: None,
            columns_state: None,
            query: None,
            scope: None,
            zoom: None,
            active: None,
        },
        &context.origin,
    )?;
    let measure = match (measure, read) {
        (Measure::Width, _) => MeasureSpec::Width,
        (Measure::Height, _) => MeasureSpec::Height,
        (Measure::Read(_), Some(binding)) => {
            MeasureSpec::Read(intern_binding(machine.interner, &binding, &context.origin)?)
        }
        (Measure::Read(_), None) => unreachable!("a read measure substitutes its binding"),
    };
    Ok(ExpandedNode::Adaptive {
        measure,
        size,
        base: Box::new(walk(context, base, depth, machine)?),
        steps: steps
            .iter()
            .map(|step| Ok((step.from, walk(context, &step.node, depth, machine)?)))
            .collect::<Result<_, UiDocError>>()?,
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

fn expand_reveal(
    context: &Context<'_>,
    from: f32,
    until: Option<f32>,
    child: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    Ok(ExpandedNode::Reveal {
        from,
        until,
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
            measure,
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
                measure: *measure,
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
        column @ ControlNode::Column { .. } => expand_column(context, column, depth, machine),
        ControlNode::Scroll { id, size, child } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Scroll {
                id: machine.interner.intern(&id.0, &context.origin)?,
                size: *size,
                child: Box::new(walk(context, child, depth, machine)?),
            })
        }
        ControlNode::Slot { id, size, default } => {
            expand_slot(context, id, *size, default, depth, machine)
        }
        ControlNode::Adaptive {
            id,
            measure,
            size,
            base,
            steps,
        } => expand_adaptive(
            context,
            node,
            id,
            (measure, *size),
            (base.as_ref(), steps),
            depth,
            machine,
        ),
        ControlNode::Optional { id, hidden, child } => {
            expand_optional(context, node, id, hidden, child, depth, machine)
        }
        ControlNode::Reveal { from, until, child } => {
            expand_reveal(context, *from, *until, child, depth, machine)
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
        control @ (ControlNode::DeckSummary { id, .. }
        | ControlNode::Brand { id, .. }
        | ControlNode::Spacer { id, .. }
        | ControlNode::Meter { id, .. }
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
        | ControlNode::PortalMap { id, .. }
        | ControlNode::Range { id, .. }
        | ControlNode::TrackList { id, .. }
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
        | ControlNode::VuVertical { id, .. }) => {
            let (read, write) = control.bindings();
            expand_control(
                context,
                control,
                ControlFields::new(id, control.size().copied(), read, write),
                depth,
                machine,
            )
        }
    }
}

fn expand_column(
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
        children,
    } = node
    else {
        unreachable!("expand_column is called only for a column")
    };
    machine.budget.charge(&context.origin)?;
    let declared = (id.as_ref(), write.as_ref(), None);
    let (surface, _) = container_bindings(context, node, declared, machine)?;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        expand::{Binding, BindingKind},
        ids::{DocId, StrArena},
        resolve::load_module_graph,
        source::{Limits, MemResolver},
    };

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn catalog(pairs: &[(&str, &str)]) -> TextDoc {
        TextDoc {
            id: DocId("test".to_owned()),
            schema: "kithara.text".to_owned(),
            version: 1,
            entries: pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }
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
        expand_with_text(set, uri, args, builtin::text_doc())
    }

    fn expand_with_text(
        set: &ModuleSet,
        uri: &SourceUri,
        args: &BTreeMap<String, String>,
        text: &TextDoc,
    ) -> Result<(ExpandedNode, StrArena), UiDocError> {
        let mut budget = Budget::new(Limits::default().max_nodes);
        let mut interner = Interner::new(64 * 1024);
        let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());
        let module = Expander::new(
            Limits::default().max_depth,
            &mut budget,
            &mut interner,
            text,
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
        let module = Expander::new(
            max_depth,
            &mut budget,
            &mut interner,
            builtin::text_doc(),
            &mut visitor,
        )
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

        let error = Expander::new(
            limits.max_depth,
            &mut budget,
            &mut interner,
            builtin::text_doc(),
            &mut visitor,
        )
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
    fn doubled_at_expands_to_literal_at() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "handle.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "handle",
                root: Chip(id: "handle", label: "@@user"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "handle.kmodule.ron", &Limits::default()).unwrap();

        let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
        let ExpandedNode::Control {
            spec: ControlSpec::Chip { label, .. },
            ..
        } = root
        else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(label), "@user");
    }

    #[kithara::test]
    fn at_key_resolves_directly_to_its_catalog_value() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "greeting.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "greeting",
                root: Chip(id: "chip", label: "@menu.modules"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "greeting.kmodule.ron", &Limits::default()).unwrap();
        let text = catalog(&[("menu.modules", "Modules")]);

        let (root, arena) = expand_with_text(&set, &uri, &BTreeMap::new(), &text).unwrap();
        let ExpandedNode::Control {
            spec: ControlSpec::Chip { label, .. },
            ..
        } = root
        else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(label), "Modules");
    }

    #[kithara::test]
    fn bare_at_sign_mid_string_is_not_a_marker() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "contact.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "contact",
                root: Chip(id: "chip", label: "user@example.com"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "contact.kmodule.ron", &Limits::default()).unwrap();

        let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
        let ExpandedNode::Control {
            spec: ControlSpec::Chip { label, .. },
            ..
        } = root
        else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(label), "user@example.com");
    }

    #[kithara::test]
    fn unknown_at_key_is_a_compile_error() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "greeting.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "greeting",
                root: Chip(id: "chip", label: "@missing.key"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "greeting.kmodule.ron", &Limits::default()).unwrap();
        let text = catalog(&[]);

        let error = expand_with_text(&set, &uri, &BTreeMap::new(), &text).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::UnknownTextKey { key, .. } if key == "missing.key"
        ));
    }

    #[kithara::test]
    fn escaped_at_argument_survives_substitution_before_key_resolution() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "wrapper.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "wrapper",
                root: Include(id: "inner", source: "inner.kmodule.ron", with: { "label": "@@handle" }))"#,
        );
        resolver.insert(
            "inner.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "inner", parameters: ["label"],
                root: Chip(id: "chip", label: "$label"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "wrapper.kmodule.ron", &Limits::default()).unwrap();

        let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
        let ExpandedNode::Control {
            spec: ControlSpec::Chip { label, .. },
            ..
        } = root
        else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(label), "@handle");
    }

    #[kithara::test]
    fn substituted_at_key_resolves_through_the_catalog() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "wrapper.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "wrapper",
                root: Include(id: "inner", source: "inner.kmodule.ron", with: { "label": "@menu.modules" }))"#,
        );
        resolver.insert(
            "inner.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "inner", parameters: ["label"],
                root: Chip(id: "chip", label: "$label"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "wrapper.kmodule.ron", &Limits::default()).unwrap();
        let text = catalog(&[("menu.modules", "Modules")]);

        let (root, arena) = expand_with_text(&set, &uri, &BTreeMap::new(), &text).unwrap();
        let ExpandedNode::Control {
            spec: ControlSpec::Chip { label, .. },
            ..
        } = root
        else {
            panic!("expected control");
        };
        assert_eq!(arena.resolve(label), "Modules");
    }

    #[kithara::test]
    fn module_title_resolves_an_at_key_without_dollar_substitution() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "titled.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "titled", title: "@menu.modules",
                root: Chip(id: "chip", label: "PLAY"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "titled.kmodule.ron", &Limits::default()).unwrap();
        let text = catalog(&[("menu.modules", "Modules")]);
        let mut budget = Budget::new(Limits::default().max_nodes);
        let mut interner = Interner::new(64 * 1024);
        let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());

        let module = Expander::new(
            Limits::default().max_depth,
            &mut budget,
            &mut interner,
            &text,
            &mut visitor,
        )
        .expand_module(&set, &uri, &BTreeMap::new(), "")
        .unwrap();
        let arena = interner.finish();

        assert_eq!(arena.resolve(module.title.unwrap()), "Modules");
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
