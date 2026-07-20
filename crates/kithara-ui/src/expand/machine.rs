use std::collections::BTreeMap;

use super::{
    Budget, ControlSite, ControlSpec, ControlVisitor, ExpandedModule, ExpandedNode,
    binding_subst::{
        intern_binding, intern_optional_binding, intern_optional_text, intern_text, intern_texts,
        substitute_binding, substitute_map,
    },
};
use crate::{
    error::UiDocError,
    ids::{Interner, NodeId, SourceUri},
    module::{AdaptivePolicy, BindingRef, ButtonStyle, ControlNode, IconName},
    resolve::ModuleSet,
    size::SizeSpec,
};

pub(super) struct Context<'a> {
    pub(super) set: &'a ModuleSet,
    pub(super) origin: SourceUri,
    pub(super) args: BTreeMap<String, String>,
    pub(super) prefix: String,
}

/// Cross-cutting expansion state threaded through the recursion: the include
/// depth cap, the shared node budget, and the per-control validation visitor.
pub(crate) struct Expander<'m, 'v> {
    max_depth: usize,
    budget: &'m mut Budget,
    interner: &'m mut Interner,
    visitor: &'m mut ControlVisitor<'v>,
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
                let binding = substitute_binding(&context, binding, &path)?;
                intern_binding(self.interner, &binding, entry)
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
        let collapsed = self
            .interner
            .intern(&format!("ui.module.{}.collapsed", doc.id.0), entry)?;
        Ok(ExpandedModule {
            module,
            title,
            chip,
            chrome: doc.chrome,
            footer,
            collapsed,
            root,
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
            origin: uri.clone(),
            depth,
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
        origin: uri.clone(),
        args,
        prefix,
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

#[derive(Clone, Copy)]
struct ControlFields<'a> {
    id: &'a NodeId,
    size: Option<SizeSpec>,
    read: Option<&'a BindingRef>,
    write: Option<&'a BindingRef>,
    adaptive: &'a AdaptivePolicy,
}

struct ExtraBindings {
    columns_state: Option<BindingRef>,
    query: Option<BindingRef>,
    scope: Option<BindingRef>,
    zoom: Option<BindingRef>,
}

#[derive(Clone, Copy)]
struct ExtraBindingRefs<'a> {
    columns_state: Option<&'a BindingRef>,
    query: Option<&'a BindingRef>,
    scope: Option<&'a BindingRef>,
    zoom: Option<&'a BindingRef>,
}

impl ExtraBindings {
    fn substitute(
        context: &Context<'_>,
        control: &ControlNode,
        path: &str,
    ) -> Result<Self, UiDocError> {
        let columns_state = match control {
            ControlNode::TrackList { columns_state, .. } => columns_state
                .as_ref()
                .map(|binding| substitute_binding(context, binding, path))
                .transpose()?,
            _ => None,
        };
        let query = match control {
            ControlNode::Tree { query, .. } => query
                .as_ref()
                .map(|binding| substitute_binding(context, binding, path))
                .transpose()?,
            _ => None,
        };
        let scope = match control {
            ControlNode::ContextBar { scope, .. } => scope
                .as_ref()
                .map(|binding| substitute_binding(context, binding, path))
                .transpose()?,
            _ => None,
        };
        let zoom = match control {
            ControlNode::Wave { zoom, .. } => zoom
                .as_ref()
                .map(|binding| substitute_binding(context, binding, path))
                .transpose()?,
            _ => None,
        };
        Ok(Self {
            columns_state,
            query,
            scope,
            zoom,
        })
    }

    fn as_refs(&self) -> ExtraBindingRefs<'_> {
        ExtraBindingRefs {
            columns_state: self.columns_state.as_ref(),
            query: self.query.as_ref(),
            scope: self.scope.as_ref(),
            zoom: self.zoom.as_ref(),
        }
    }
}

impl<'a> ControlFields<'a> {
    fn new(
        id: &'a NodeId,
        size: Option<SizeSpec>,
        read: Option<&'a BindingRef>,
        write: Option<&'a BindingRef>,
        adaptive: &'a AdaptivePolicy,
    ) -> Self {
        Self {
            id,
            size,
            read,
            write,
            adaptive,
        }
    }
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
        .map(|binding| substitute_binding(context, binding, path))
        .transpose()?;
    let write = fields
        .write
        .map(|binding| substitute_binding(context, binding, path))
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
        },
        &context.origin,
    )?;
    Ok(ExpandedNode::Control {
        path: machine.interner.intern(path, &context.origin)?,
        id: machine.interner.intern(&fields.id.0, &context.origin)?,
        spec,
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
    let spec = match control {
        ControlNode::DeckSummary { style, .. } => ControlSpec::DeckSummary { style: *style },
        ControlNode::Brand { .. } => ControlSpec::Brand,
        ControlNode::Spacer { .. } => ControlSpec::Spacer,
        ControlNode::PresetSelector { .. } => ControlSpec::PresetSelector,
        ControlNode::SettingsButton { .. } => ControlSpec::SettingsButton,
        ControlNode::TitleBar { label, .. } => title_bar_spec(context, machine, label, &path)?,
        ControlNode::WindowControls { style, .. } => ControlSpec::WindowControls { style: *style },
        ControlNode::Text { style, label, .. } => ControlSpec::Text {
            style: *style,
            label: intern_optional_text(
                context,
                machine.interner,
                label.as_deref(),
                &path,
                &context.origin,
            )?,
        },
        ControlNode::Glyph { icon, .. } => ControlSpec::Glyph { icon: *icon },
        ControlNode::NavItem { label, icon, .. } => ControlSpec::NavItem {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
            icon: *icon,
        },
        ControlNode::TabLarge { label, .. } => ControlSpec::TabLarge {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
        },
        ControlNode::Button {
            label,
            icon,
            active_label,
            style,
            ..
        } => button_spec(
            context,
            machine.interner,
            label,
            *icon,
            active_label.as_deref(),
            *style,
            &path,
        )?,
        ControlNode::Bpm { placeholder, .. } => ControlSpec::Bpm {
            placeholder: intern_optional_text(
                context,
                machine.interner,
                placeholder.as_deref(),
                &path,
                &context.origin,
            )?,
        },
        ControlNode::Time { .. } => ControlSpec::Time,
        ControlNode::Scalar { format, .. } => ControlSpec::Scalar { format: *format },
        ControlNode::Fader { style, .. } => ControlSpec::Fader { style: *style },
        ControlNode::Wave { style, badge, .. } => ControlSpec::Wave {
            style: *style,
            badge: intern_optional_text(
                context,
                machine.interner,
                badge.as_deref(),
                &path,
                &context.origin,
            )?,
            zoom: intern_optional_binding(machine.interner, extra.zoom.as_ref(), &context.origin)?,
        },
        ControlNode::TrackList { columns, .. } => ControlSpec::TrackList {
            columns: columns.clone(),
            columns_state: intern_optional_binding(
                machine.interner,
                extra.columns_state.as_ref(),
                &context.origin,
            )?,
        },
        ControlNode::Tree { .. } => ControlSpec::Tree {
            query: intern_optional_binding(
                machine.interner,
                extra.query.as_ref(),
                &context.origin,
            )?,
        },
        ControlNode::ContextBar { scope_items, .. } => context_bar_spec(
            context,
            machine.interner,
            scope_items,
            extra.scope.as_ref(),
            &path,
        )?,
        ControlNode::Toggle { .. } => ControlSpec::Toggle,
        ControlNode::Checkbox { .. } => ControlSpec::Checkbox,
        ControlNode::Segmented { items, .. } => ControlSpec::Segmented {
            items: intern_texts(context, machine.interner, items, &path, &context.origin)?,
        },
        ControlNode::Select { label, .. } => ControlSpec::Select {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
        },
        ControlNode::StatusDot { label, tone, .. } => ControlSpec::StatusDot {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
            tone: *tone,
        },
        ControlNode::Cell {
            label, highlighted, ..
        } => ControlSpec::Cell {
            label: intern_optional_text(
                context,
                machine.interner,
                label.as_deref(),
                &path,
                &context.origin,
            )?,
            highlighted: *highlighted,
        },
        ControlNode::Readout {
            label,
            tone,
            framed,
            ..
        } => ControlSpec::Readout {
            label: intern_optional_text(
                context,
                machine.interner,
                label.as_deref(),
                &path,
                &context.origin,
            )?,
            tone: *tone,
            framed: *framed,
        },
        ControlNode::Chip { label, style, .. } => ControlSpec::Chip {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
            style: *style,
        },
        ControlNode::Knob { .. } => ControlSpec::Knob,
        ControlNode::VuStereo { .. } => ControlSpec::VuStereo,
        ControlNode::VuVertical { .. } => ControlSpec::VuVertical,
        ControlNode::Row { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Slot { .. } => return walk(context, control, depth, machine),
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

fn button_spec(
    context: &Context<'_>,
    interner: &mut Interner,
    label: &str,
    icon: Option<IconName>,
    active_label: Option<&str>,
    style: ButtonStyle,
    path: &str,
) -> Result<ControlSpec, UiDocError> {
    Ok(ControlSpec::Button {
        label: intern_text(context, interner, label, path, &context.origin)?,
        icon,
        active_label: intern_optional_text(context, interner, active_label, path, &context.origin)?,
        style,
    })
}

fn expand_header_control(
    context: &Context<'_>,
    control: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match control {
        control @ (ControlNode::DeckSummary {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Brand {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::Spacer {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::PresetSelector {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::SettingsButton {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::Text {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Glyph {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }) => expand_control(
            context,
            control,
            ControlFields::new(id, *size, read.as_ref(), write.as_ref(), adaptive),
            depth,
            machine,
        ),
        _ => walk(context, control, depth, machine),
    }
}

fn expand_value_control(
    context: &Context<'_>,
    control: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match control {
        control @ (ControlNode::Button {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::NavItem {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::TabLarge {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Bpm {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Time {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::Scalar {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Fader {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Wave {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::TrackList {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Tree {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::ContextBar {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }) => expand_control(
            context,
            control,
            ControlFields::new(id, *size, read.as_ref(), write.as_ref(), adaptive),
            depth,
            machine,
        ),
        _ => walk(context, control, depth, machine),
    }
}

fn expand_window_control(
    context: &Context<'_>,
    control: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match control {
        control @ (ControlNode::TitleBar {
            id, size, adaptive, ..
        }
        | ControlNode::WindowControls {
            id, size, adaptive, ..
        }) => expand_control(
            context,
            control,
            ControlFields::new(id, *size, None, None, adaptive),
            depth,
            machine,
        ),
        _ => walk(context, control, depth, machine),
    }
}

fn expand_atom_control(
    context: &Context<'_>,
    control: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match control {
        control @ (ControlNode::Toggle {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::Checkbox {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::Segmented {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Select {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::StatusDot {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Cell {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Readout {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Chip {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::Knob {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::VuStereo {
            id,
            size,
            read,
            write,
            adaptive,
        }
        | ControlNode::VuVertical {
            id,
            size,
            read,
            write,
            adaptive,
        }) => expand_control(
            context,
            control,
            ControlFields::new(id, *size, read.as_ref(), write.as_ref(), adaptive),
            depth,
            machine,
        ),
        _ => walk(context, control, depth, machine),
    }
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
            children,
        } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Row {
                id: id
                    .as_ref()
                    .map(|id| machine.interner.intern(&id.0, &context.origin))
                    .transpose()?,
                size: *size,
                gap: *gap,
                pad: *pad,
                children: children
                    .iter()
                    .map(|child| walk(context, child, depth, machine))
                    .collect::<Result<_, _>>()?,
            })
        }
        ControlNode::Column {
            id,
            size,
            gap,
            pad,
            children,
        } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Column {
                id: id
                    .as_ref()
                    .map(|id| machine.interner.intern(&id.0, &context.origin))
                    .transpose()?,
                size: *size,
                gap: *gap,
                pad: *pad,
                children: children
                    .iter()
                    .map(|child| walk(context, child, depth, machine))
                    .collect::<Result<_, _>>()?,
            })
        }
        ControlNode::Slot { id, size, default } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Slot {
                id: machine.interner.intern(&id.0, &context.origin)?,
                size: *size,
                children: default
                    .iter()
                    .map(|child| walk(context, child, depth, machine))
                    .collect::<Result<_, _>>()?,
            })
        }
        ControlNode::Include { id, source, with } => {
            let path = child_path(&context.prefix, id);
            let args = substitute_map(context, with, &path)?;
            let target =
                crate::source::join_rel(crate::source::base_dir(Some(&context.origin)), source)
                    .map(SourceUri)
                    .ok_or_else(|| UiDocError::RootEscape {
                        origin: context.origin.clone(),
                        rel: source.clone(),
                    })?;
            expand_at(context.set, &target, args, path, depth + 1, machine)
        }
        control @ (ControlNode::DeckSummary { .. }
        | ControlNode::Brand { .. }
        | ControlNode::Spacer { .. }
        | ControlNode::PresetSelector { .. }
        | ControlNode::SettingsButton { .. }
        | ControlNode::Text { .. }
        | ControlNode::Glyph { .. }) => expand_header_control(context, control, depth, machine),
        control @ (ControlNode::TitleBar { .. } | ControlNode::WindowControls { .. }) => {
            expand_window_control(context, control, depth, machine)
        }
        control @ (ControlNode::Button { .. }
        | ControlNode::NavItem { .. }
        | ControlNode::TabLarge { .. }
        | ControlNode::Bpm { .. }
        | ControlNode::Time { .. }
        | ControlNode::Scalar { .. }
        | ControlNode::Fader { .. }
        | ControlNode::Wave { .. }
        | ControlNode::TrackList { .. }
        | ControlNode::Tree { .. }
        | ControlNode::ContextBar { .. }) => expand_value_control(context, control, depth, machine),
        control @ (ControlNode::Toggle { .. }
        | ControlNode::Checkbox { .. }
        | ControlNode::Segmented { .. }
        | ControlNode::Select { .. }
        | ControlNode::StatusDot { .. }
        | ControlNode::Cell { .. }
        | ControlNode::Readout { .. }
        | ControlNode::Chip { .. }
        | ControlNode::Knob { .. }
        | ControlNode::VuStereo { .. }
        | ControlNode::VuVertical { .. }) => expand_atom_control(context, control, depth, machine),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        expand::Binding,
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
        let Some(Binding::Command { with, .. }) = write else {
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
