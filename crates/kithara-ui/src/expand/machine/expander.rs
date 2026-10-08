use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use super::{
    super::{
        BlockSpec, Budget, ControlSite, ControlSpec, ControlVisitor, ExpandedInclude,
        ExpandedModule, ExpandedNode, MagnetSpec, MeasureSpec, SlotWrites,
        binding_subst::{
            intern_binding, intern_module_text, intern_module_text_opt, resolve_optional_param,
            resolve_param, substitute_binding,
        },
        site::{ControlFields, ExtraBindingRefs, ExtraBindings},
        spec::control_spec,
        structural::{expand_include, walk_child, walk_children},
    },
    container::{expand_column, expand_row},
    slot::expand_slot,
};
use crate::{
    error::UiDocError,
    ids::{Interner, NodeId, SourceUri},
    module::{
        AdaptiveStep, BindingRef, ControlNode, Magnet, Measure, Motion, PopoverAlign, PopoverAt,
        PopoverDismiss, Pose, TableColumn,
    },
    param::Param,
    registry::EndpointRegistry,
    resolve::ModuleSet,
    shader::ShaderCache,
    size::SizeSpec,
    text::TextDoc,
    validate,
};

pub(in crate::expand) struct Context<'a> {
    pub(in crate::expand) set: &'a ModuleSet,
    pub(in crate::expand) text: &'a TextDoc,
    pub(in crate::expand) args: BTreeMap<String, String>,
    pub(in crate::expand) origin: SourceUri,
    /// The module instance this expansion belongs to, which every include
    /// under it shares. State a document keeps is named under it, so a close
    /// button an included surface carries turns the flag its includer reads.
    pub(in crate::expand) instance: String,
    pub(in crate::expand) prefix: String,
    /// Fill mounted in the template `content` slot.
    pub(in crate::expand) content: Option<&'a SourceUri>,
}

/// Expansion arguments: `named` must be declared; undeclared `passed` values are discarded.
pub(in crate::expand) struct Frame<'a> {
    pub(in crate::expand) named: BTreeMap<String, String>,
    pub(in crate::expand) passed: BTreeMap<String, String>,
    pub(in crate::expand) prefix: String,
    pub(in crate::expand) content: Option<&'a SourceUri>,
}

impl Context<'_> {
    pub(in crate::expand) fn module(&self) -> Result<&str, UiDocError> {
        self.set.def(&self.origin).map(|doc| doc.id.0.as_str())
    }

    pub(in crate::expand) fn optional_param<T: Clone + DeserializeOwned>(
        &self,
        param: Option<&Param<T>>,
        path: &str,
    ) -> Result<Option<T>, UiDocError> {
        resolve_optional_param(&self.args, &self.origin, param, path)
    }

    pub(in crate::expand) fn param<T: Clone + DeserializeOwned>(
        &self,
        param: &Param<T>,
        path: &str,
    ) -> Result<T, UiDocError> {
        resolve_param(&self.args, &self.origin, param, path)
    }

    pub(in crate::expand) fn substitute(
        &self,
        binding: &BindingRef,
        path: &str,
    ) -> Result<BindingRef, UiDocError> {
        substitute_binding(&self.args, &self.origin, binding, path, &self.instance)
    }
}

/// Cross-cutting expansion state threaded through recursion, including the
/// structural address used to preserve expanded include roots.
pub(crate) struct Expander<'m, 'v> {
    pub(in crate::expand) interner: &'m mut Interner,
    pub(in crate::expand) shaders: &'m mut ShaderCache,
    pub(in crate::expand) endpoints: &'m dyn EndpointRegistry,
    pub(in crate::expand) address: Vec<usize>,
    pub(in crate::expand) includes: Vec<ExpandedInclude>,
    pub(super) budget: &'m mut Budget,
    pub(super) visitor: &'m mut ControlVisitor<'v>,
    text: &'m TextDoc,
    /// The surface whose content is being expanded.
    surface: Option<Surface>,
    max_depth: usize,
}

/// A surface whose content is being expanded: a popover with what opens and
/// what shuts it, or a modal.
enum Surface {
    Popover(BindingRef, PopoverDismiss),
    Modal,
}

impl<'m, 'v> Expander<'m, 'v> {
    pub(crate) fn new(
        max_depth: usize,
        budget: &'m mut Budget,
        interner: &'m mut Interner,
        endpoints: &'m dyn EndpointRegistry,
        shaders: &'m mut ShaderCache,
        text: &'m TextDoc,
        visitor: &'m mut ControlVisitor<'v>,
    ) -> Self {
        Self {
            max_depth,
            budget,
            endpoints,
            shaders,
            interner,
            text,
            visitor,
            surface: None,
            address: Vec::new(),
            includes: Vec::new(),
        }
    }

    /// Hands one site to the visitor, naming the popover a write from it shuts.
    pub(super) fn visit(
        &mut self,
        site: ControlSite<'_>,
        origin: &SourceUri,
    ) -> Result<(), UiDocError> {
        let shuts = match &self.surface {
            Some(Surface::Popover(open, PopoverDismiss::OnAnyAction)) => Some(open),
            _ => None,
        };
        (self.visitor)(ControlSite { shuts, ..site }, origin)
    }

    pub(crate) fn expand_module(
        &mut self,
        set: &ModuleSet,
        entry: &SourceUri,
        args: &BTreeMap<String, String>,
        prefix: &str,
    ) -> Result<ExpandedModule, UiDocError> {
        let doc = set.def(entry)?;
        self.address.clear();
        self.includes.clear();
        let frame = Frame {
            named: args.clone(),
            passed: BTreeMap::new(),
            prefix: prefix.to_owned(),
            content: None,
        };
        let root = expand_at(set, entry, frame, prefix.to_owned(), 0, self)?;
        let context = Context {
            set,
            text: self.text,
            origin: entry.clone(),
            args: args.clone(),
            instance: prefix.to_owned(),
            prefix: prefix.to_owned(),
            content: None,
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
            drop: doc.drop.is_some(),
            collapsed,
            root,
            chrome: doc.chrome,
            includes: std::mem::take(&mut self.includes),
        })
    }
}

pub(in crate::expand) fn expand_at<'a>(
    set: &'a ModuleSet,
    uri: &SourceUri,
    Frame {
        named,
        mut passed,
        prefix,
        content,
    }: Frame<'a>,
    instance: String,
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
    let doc = set.def(uri)?;
    if let Some(name) = named.keys().find(|name| !doc.parameters.contains(*name)) {
        return Err(UiDocError::UnknownParam {
            origin: uri.clone(),
            name: name.clone(),
            path: prefix,
        });
    }
    passed.retain(|name, _| doc.parameters.contains(name));
    passed.extend(named);
    let context = Context {
        set,
        args: passed,
        instance,
        prefix,
        content,
        text: machine.text,
        origin: uri.clone(),
    };
    walk(&context, &doc.root, depth, machine)
}

pub(in crate::expand) fn child_path(prefix: &str, id: &NodeId) -> String {
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
    let columns: &[TableColumn] = match &spec {
        ControlSpec::Table { columns, .. } => columns,
        _ => &[],
    };
    machine.visit(
        ControlSite {
            columns,
            read: read.as_ref(),
            write: write.as_ref(),
            columns_state: extra.columns_state,
            status: extra.status,
            query: extra.query,
            scope: extra.scope,
            zoom: extra.zoom,
            active: extra.active,
            writes: extra.writes,
            ..ControlSite::new(control, path)
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
    id: &NodeId,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let (read, write) = control.bindings();
    let fields = ControlFields::new(id, control.size().copied(), read, write);
    let path = begin_control(context, fields.id, machine)?;
    if matches!(machine.surface, Some(Surface::Modal))
        && match control {
            ControlNode::WindowDrag { .. }
            | ControlNode::TitleBar { .. }
            | ControlNode::WindowControls { .. } => true,
            ControlNode::ContextBar { scope_items, .. } => !scope_items.is_empty(),
            _ => false,
        }
    {
        return Err(UiDocError::InvalidId {
            origin: context.origin.clone(),
            id: path,
            reason: "a modal hosts in-flow content only".to_owned(),
        });
    }
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
    machine.visit(
        ControlSite {
            read: read.as_ref(),
            ..ControlSite::new(node, &path)
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
    machine.visit(
        ControlSite {
            read: Some(&hidden),
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    Ok(ExpandedNode::Optional {
        block: BlockSpec {
            path: machine.interner.intern(&path, &context.origin)?,
            hidden: intern_binding(machine.interner, &hidden, &context.origin)?,
        },
        child: Box::new(walk_child(context, child, 0, depth, machine)?),
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

/// Refuses a surface of kind `inner` opening inside the surface being expanded.
fn refuse_nested(
    context: &Context<'_>,
    path: &str,
    inner: &str,
    machine: &Expander<'_, '_>,
) -> Result<(), UiDocError> {
    let outer = match machine.surface {
        None => return Ok(()),
        Some(Surface::Popover(..)) => "popover",
        Some(Surface::Modal) => "modal",
    };
    let article = if outer == inner { "another" } else { "a" };
    Err(UiDocError::InvalidId {
        origin: context.origin.clone(),
        id: path.to_owned(),
        reason: format!("a {inner} must not open inside {article} {outer}"),
    })
}

fn expand_popover(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    declared: (&BindingRef, PopoverAt, PopoverAlign, PopoverDismiss),
    subtrees: (&ControlNode, &ControlNode),
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let ((open, at, align, dismiss), (anchor, content)) = (declared, subtrees);
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    refuse_nested(context, &path, "popover", machine)?;
    let open = context.substitute(open, &path)?;
    if dismiss == PopoverDismiss::OnAnyAction && !matches!(open, BindingRef::View { .. }) {
        return Err(UiDocError::InvalidId {
            origin: context.origin.clone(),
            id: path,
            reason: "a popover any action shuts must open on a view flag".to_owned(),
        });
    }
    machine.visit(
        ControlSite {
            read: Some(&open),
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    let anchor = walk_child(context, anchor, 0, depth, machine)?;
    machine.surface = Some(Surface::Popover(open.clone(), dismiss));
    let content = walk_child(context, content, 1, depth, machine);
    machine.surface = None;
    Ok(ExpandedNode::Popover {
        at,
        align,
        path: machine.interner.intern(&path, &context.origin)?,
        open: intern_binding(machine.interner, &open, &context.origin)?,
        anchor: Box::new(anchor),
        content: Box::new(content?),
    })
}

fn expand_modal(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    declared: (&BindingRef, &BindingRef),
    content: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let (open, close) = declared;
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    refuse_nested(context, &path, "modal", machine)?;
    let open = context.substitute(open, &path)?;
    let close = context.substitute(close, &path)?;
    machine.visit(
        ControlSite {
            read: Some(&open),
            write: Some(&close),
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    machine.surface = Some(Surface::Modal);
    let content = walk_child(context, content, 0, depth, machine);
    machine.surface = None;
    Ok(ExpandedNode::Modal {
        path: machine.interner.intern(&path, &context.origin)?,
        open: intern_binding(machine.interner, &open, &context.origin)?,
        content: Box::new(content?),
    })
}

fn expand_pressable(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    declared: (&BindingRef, Option<&BindingRef>),
    child: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let (press, secondary) = declared;
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    let press = context.substitute(press, &path)?;
    let secondary = secondary
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    machine.visit(
        ControlSite {
            write: Some(&press),
            writes: SlotWrites {
                secondary: secondary.as_ref(),
                ..SlotWrites::default()
            },
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    Ok(ExpandedNode::Pressable {
        path: machine.interner.intern(&path, &context.origin)?,
        press: intern_binding(machine.interner, &press, &context.origin)?,
        child: Box::new(walk_child(context, child, 0, depth, machine)?),
    })
}

/// Where an object starts, where it ends, and what carries it between them.
#[derive(Clone, Copy)]
struct Track<'a> {
    pose: &'a Pose,
    motion: Option<&'a Motion<BindingRef>>,
    phase: Option<&'a BindingRef>,
    to: Option<&'a Pose>,
}

/// The pose survives expansion rather than being folded into the control, so
/// the render pass can move it between frames from whatever drives it.
fn expand_object(
    context: &Context<'_>,
    node: &ControlNode,
    id: &NodeId,
    track: Track<'_>,
    child: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    let driver = match (track.phase, track.motion) {
        (Some(phase), None) => Some(phase),
        (None, Some(motion)) => Some(&motion.clock),
        (None, None) | (Some(_), Some(_)) => None,
    };
    let driver = driver
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    machine.visit(
        ControlSite {
            read: driver.as_ref(),
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    let driver = driver
        .as_ref()
        .map(|binding| intern_binding(machine.interner, binding, &context.origin))
        .transpose()?;
    let (phase, motion) = match track.motion {
        Some(motion) => (None, driver.map(|clock| motion.with_clock(clock))),
        None => (driver, None),
    };
    Ok(ExpandedNode::Object {
        phase,
        motion,
        pose: *track.pose,
        to: track.to.copied(),
        child: Box::new(walk_child(context, child, 0, depth, machine)?),
    })
}

/// A placement keeps its point, its endpoints and its magnet through
/// expansion: where it stands is answered per frame, and which placements take
/// it is answered by the stage that holds them all.
fn expand_placed(
    context: &Context<'_>,
    node: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    let ControlNode::Placed {
        id,
        at,
        read,
        write,
        magnet,
        child,
    } = node
    else {
        unreachable!("expand_placed is called only for a placement")
    };
    machine.budget.charge(&context.origin)?;
    let path = child_path(&context.prefix, id);
    let read = read
        .as_ref()
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    let write = write
        .as_ref()
        .map(|binding| context.substitute(binding, &path))
        .transpose()?;
    machine.visit(
        ControlSite {
            read: read.as_ref(),
            write: write.as_ref(),
            ..ControlSite::new(node, &path)
        },
        &context.origin,
    )?;
    let magnet = magnet
        .as_ref()
        .map(|magnet| intern_magnet(machine, magnet, &context.origin))
        .transpose()?;
    Ok(ExpandedNode::Placed {
        magnet,
        path: machine.interner.intern(&path, &context.origin)?,
        id: machine.interner.intern(&id.0, &context.origin)?,
        at: *at,
        read: read
            .as_ref()
            .map(|binding| intern_binding(machine.interner, binding, &context.origin))
            .transpose()?,
        write: write
            .as_ref()
            .map(|binding| intern_binding(machine.interner, binding, &context.origin))
            .transpose()?,
        child: Box::new(walk_child(context, child, 0, depth, machine)?),
    })
}

fn intern_magnet(
    machine: &mut Expander<'_, '_>,
    magnet: &Magnet,
    origin: &SourceUri,
) -> Result<MagnetSpec, UiDocError> {
    let to = magnet
        .to
        .iter()
        .map(|target| machine.interner.intern(&target.0, origin))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MagnetSpec {
        to,
        within: magnet.within,
    })
}

fn expand_scroll(
    context: &Context<'_>,
    id: &NodeId,
    size: Option<SizeSpec>,
    child: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    Ok(ExpandedNode::Scroll {
        id: machine.interner.intern(&id.0, &context.origin)?,
        size,
        child: Box::new(walk(context, child, depth, machine)?),
    })
}

fn expand_stage(
    context: &Context<'_>,
    id: &NodeId,
    size: Option<SizeSpec>,
    children: &[ControlNode],
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    machine.budget.charge(&context.origin)?;
    Ok(ExpandedNode::Stage {
        size,
        id: machine.interner.intern(&id.0, &context.origin)?,
        children: walk_children(context, children, depth, machine)?,
    })
}

pub(in crate::expand) fn walk(
    context: &Context<'_>,
    node: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match node {
        row @ ControlNode::Row { .. } => expand_row(context, row, depth, machine),
        column @ ControlNode::Column { .. } => expand_column(context, column, depth, machine),
        ControlNode::Scroll { id, size, child } => {
            expand_scroll(context, id, *size, child, depth, machine)
        }
        ControlNode::Stage { id, size, children } => {
            expand_stage(context, id, *size, children, depth, machine)
        }
        placed @ ControlNode::Placed { .. } => expand_placed(context, placed, depth, machine),
        ControlNode::Slot {
            id,
            size,
            default,
            from,
            each,
            select,
        } => {
            let shows = (from, each, select);
            expand_slot(context, node, (id, *size, default), shows, depth, machine)
        }
        ControlNode::Adaptive {
            id,
            measure,
            size,
            base,
            steps,
        } => {
            let (declared, branches) = ((measure, *size), (base.as_ref(), steps.as_slice()));
            expand_adaptive(context, node, id, declared, branches, depth, machine)
        }
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
            dismiss,
            anchor,
            content,
        } => {
            let subtrees = (anchor.as_ref(), content.as_ref());
            expand_popover(
                context,
                node,
                id,
                (open, *at, *align, *dismiss),
                subtrees,
                depth,
                machine,
            )
        }
        ControlNode::Modal {
            id,
            open,
            close,
            content,
        } => expand_modal(context, node, id, (open, close), content, depth, machine),
        ControlNode::Pressable {
            id,
            press,
            secondary,
            child,
        } => expand_pressable(
            context,
            node,
            id,
            (press, secondary.as_ref()),
            child,
            depth,
            machine,
        ),
        ControlNode::Object {
            id,
            transform,
            to,
            phase,
            motion,
            child,
        } => expand_object(
            context,
            node,
            id,
            Track {
                pose: transform,
                to: to.as_ref(),
                phase: phase.as_ref(),
                motion: motion.as_ref(),
            },
            child,
            depth,
            machine,
        ),
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
        | ControlNode::Lottie { id, .. }
        | ControlNode::Sprite { id, .. }
        | ControlNode::Shader { id, .. }
        | ControlNode::Custom { id, .. }
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
        | ControlNode::VuVertical { id, .. }) => {
            expand_control(context, control, id, depth, machine)
        }
    }
}
