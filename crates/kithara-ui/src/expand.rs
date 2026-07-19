use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{InternId, Interner, NodeId, SourceUri},
    module::{
        AdaptivePolicy, BindingRef, ButtonStyle, ControlNode, DeckSummaryStyle, FaderStyle,
        ScalarFormat, TextStyle, Tone, WaveStyle,
    },
    resolve::ModuleSet,
    size::SizeSpec,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExpandedNode {
    Row {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        pad: Option<f32>,
        children: Vec<Self>,
    },
    Column {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        pad: Option<f32>,
        children: Vec<Self>,
    },
    Slot {
        id: InternId,
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    Control {
        path: InternId,
        id: InternId,
        spec: ControlSpec,
        size: Option<SizeSpec>,
        read: Option<Binding>,
        write: Option<Binding>,
        adaptive: AdaptivePolicy,
    },
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlSpec {
    DeckHeader {
        badge: Option<InternId>,
    },
    DeckSummary {
        style: DeckSummaryStyle,
    },
    Brand,
    Spacer,
    PresetSelector,
    SettingsButton,
    Text {
        style: TextStyle,
    },
    Button {
        label: InternId,
        active_label: Option<InternId>,
        style: ButtonStyle,
    },
    Bpm {
        placeholder: Option<InternId>,
    },
    Time,
    Scalar {
        format: ScalarFormat,
    },
    Fader {
        style: FaderStyle,
    },
    Wave {
        style: WaveStyle,
    },
    TrackList,
    Toggle,
    Checkbox,
    Readout {
        label: Option<InternId>,
        tone: Tone,
        framed: bool,
    },
    Chip {
        label: InternId,
    },
    Knob,
    VuStereo,
    VuVertical,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Binding {
    Command {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Parameter {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Telemetry {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Model {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct ControlSite<'a> {
    pub(crate) path: &'a str,
    pub(crate) control: &'a ControlNode,
    pub(crate) read: Option<&'a BindingRef>,
    pub(crate) write: Option<&'a BindingRef>,
}

type ControlVisitor<'v> =
    dyn for<'a> FnMut(ControlSite<'a>, &SourceUri) -> Result<(), UiDocError> + 'v;

pub(crate) struct Budget {
    nodes: usize,
    max: usize,
}

impl Budget {
    pub(crate) fn new(max: usize) -> Self {
        Self { nodes: 0, max }
    }

    pub(crate) fn charge(&mut self, origin: &SourceUri) -> Result<(), UiDocError> {
        self.nodes += 1;
        if self.nodes > self.max {
            return Err(UiDocError::NodesExceeded {
                origin: origin.clone(),
                count: self.nodes,
                max: self.max,
            });
        }
        Ok(())
    }
}

struct Context<'a> {
    set: &'a ModuleSet,
    origin: SourceUri,
    args: BTreeMap<String, String>,
    prefix: String,
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
    ) -> Result<ExpandedNode, UiDocError> {
        expand_at(set, entry, args.clone(), prefix.to_owned(), 0, self)
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

fn substitute(context: &Context<'_>, value: &str, path: &str) -> Result<String, UiDocError> {
    if let Some(literal) = value.strip_prefix("$$") {
        return Ok(format!("${literal}"));
    }
    let Some(name) = value.strip_prefix('$') else {
        return Ok(value.to_owned());
    };
    context
        .args
        .get(name)
        .cloned()
        .ok_or_else(|| UiDocError::UnresolvedParam {
            origin: context.origin.clone(),
            name: name.to_owned(),
            path: path.to_owned(),
        })
}

fn substitute_map(
    context: &Context<'_>,
    map: &BTreeMap<String, String>,
    path: &str,
) -> Result<BTreeMap<String, String>, UiDocError> {
    map.iter()
        .map(|(key, value)| Ok((key.clone(), substitute(context, value, path)?)))
        .collect()
}

fn substitute_binding(
    context: &Context<'_>,
    binding: &BindingRef,
    path: &str,
) -> Result<BindingRef, UiDocError> {
    let binding = match binding {
        BindingRef::Command { id, with } => BindingRef::Command {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
        BindingRef::Parameter { id, with } => BindingRef::Parameter {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
        BindingRef::Telemetry { id, with } => BindingRef::Telemetry {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
        BindingRef::Model { id, with } => BindingRef::Model {
            id: id.clone(),
            with: substitute_map(context, with, path)?,
        },
    };
    Ok(binding)
}

fn intern_map(
    interner: &mut Interner,
    values: &BTreeMap<String, String>,
    origin: &SourceUri,
) -> Result<BTreeMap<InternId, InternId>, UiDocError> {
    values
        .iter()
        .map(|(key, value)| {
            Ok((
                interner.intern(key, origin)?,
                interner.intern(value, origin)?,
            ))
        })
        .collect()
}

fn intern_binding(
    interner: &mut Interner,
    binding: &BindingRef,
    origin: &SourceUri,
) -> Result<Binding, UiDocError> {
    match binding {
        BindingRef::Command { id, with } => Ok(Binding::Command {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
        BindingRef::Parameter { id, with } => Ok(Binding::Parameter {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
        BindingRef::Telemetry { id, with } => Ok(Binding::Telemetry {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
        BindingRef::Model { id, with } => Ok(Binding::Model {
            id: interner.intern(&id.0, origin)?,
            with: intern_map(interner, with, origin)?,
        }),
    }
}

fn intern_text(
    context: &Context<'_>,
    interner: &mut Interner,
    value: &str,
    path: &str,
    origin: &SourceUri,
) -> Result<InternId, UiDocError> {
    interner.intern(&substitute(context, value, path)?, origin)
}

fn intern_optional_text(
    context: &Context<'_>,
    interner: &mut Interner,
    value: Option<&str>,
    path: &str,
    origin: &SourceUri,
) -> Result<Option<InternId>, UiDocError> {
    value
        .map(|value| intern_text(context, interner, value, path, origin))
        .transpose()
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
    let spec = match control {
        ControlNode::DeckHeader { badge, .. } => ControlSpec::DeckHeader {
            badge: intern_optional_text(
                context,
                machine.interner,
                badge.as_deref(),
                &path,
                &context.origin,
            )?,
        },
        ControlNode::DeckSummary { style, .. } => ControlSpec::DeckSummary { style: *style },
        ControlNode::Brand { .. } => ControlSpec::Brand,
        ControlNode::Spacer { .. } => ControlSpec::Spacer,
        ControlNode::PresetSelector { .. } => ControlSpec::PresetSelector,
        ControlNode::SettingsButton { .. } => ControlSpec::SettingsButton,
        ControlNode::Text { style, .. } => ControlSpec::Text { style: *style },
        ControlNode::Button {
            label,
            active_label,
            style,
            ..
        } => ControlSpec::Button {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
            active_label: intern_optional_text(
                context,
                machine.interner,
                active_label.as_deref(),
                &path,
                &context.origin,
            )?,
            style: *style,
        },
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
        ControlNode::Wave { style, .. } => ControlSpec::Wave { style: *style },
        ControlNode::TrackList { .. } => ControlSpec::TrackList,
        ControlNode::Toggle { .. } => ControlSpec::Toggle,
        ControlNode::Checkbox { .. } => ControlSpec::Checkbox,
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
        ControlNode::Chip { label, .. } => ControlSpec::Chip {
            label: intern_text(context, machine.interner, label, &path, &context.origin)?,
        },
        ControlNode::Knob { .. } => ControlSpec::Knob,
        ControlNode::VuStereo { .. } => ControlSpec::VuStereo,
        ControlNode::VuVertical { .. } => ControlSpec::VuVertical,
        ControlNode::Row { .. }
        | ControlNode::Column { .. }
        | ControlNode::Include { .. }
        | ControlNode::Slot { .. } => return walk(context, control, depth, machine),
    };
    finish_control(context, control, fields, &path, spec, machine)
}

fn expand_header_control(
    context: &Context<'_>,
    control: &ControlNode,
    depth: usize,
    machine: &mut Expander<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match control {
        control @ (ControlNode::DeckHeader {
            id,
            size,
            read,
            write,
            adaptive,
            ..
        }
        | ControlNode::DeckSummary {
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
        control @ (ControlNode::DeckHeader { .. }
        | ControlNode::DeckSummary { .. }
        | ControlNode::Brand { .. }
        | ControlNode::Spacer { .. }
        | ControlNode::PresetSelector { .. }
        | ControlNode::SettingsButton { .. }
        | ControlNode::Text { .. }) => expand_header_control(context, control, depth, machine),
        control @ (ControlNode::Button { .. }
        | ControlNode::Bpm { .. }
        | ControlNode::Time { .. }
        | ControlNode::Scalar { .. }
        | ControlNode::Fader { .. }
        | ControlNode::Wave { .. }
        | ControlNode::TrackList { .. }) => expand_value_control(context, control, depth, machine),
        control @ (ControlNode::Toggle { .. }
        | ControlNode::Checkbox { .. }
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
        let root = Expander::new(
            Limits::default().max_depth,
            &mut budget,
            &mut interner,
            &mut visitor,
        )
        .expand_module(set, uri, args, "")?;
        Ok((root, interner.finish()))
    }

    fn expand_with_depth(
        set: &ModuleSet,
        uri: &SourceUri,
        max_depth: usize,
    ) -> Result<(ExpandedNode, StrArena), UiDocError> {
        let mut budget = Budget::new(Limits::default().max_nodes);
        let mut interner = Interner::new(64 * 1024);
        let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());
        let root = Expander::new(max_depth, &mut budget, &mut interner, &mut visitor)
            .expand_module(set, uri, &BTreeMap::new(), "")?;
        Ok((root, interner.finish()))
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
            spec: ControlSpec::Chip { label },
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
