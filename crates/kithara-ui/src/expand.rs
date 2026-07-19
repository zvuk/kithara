use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{ControlKind, NodeId, SourceUri},
    module::{AdaptivePolicy, BindingRef, ControlNode, PropValue},
    resolve::ModuleSet,
    size::SizeSpec,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExpandedNode {
    Row {
        id: Option<NodeId>,
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    Column {
        id: Option<NodeId>,
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    Slot {
        id: NodeId,
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    Control {
        path: String,
        id: NodeId,
        kind: ControlKind,
        size: Option<SizeSpec>,
        props: BTreeMap<String, PropValue>,
        read: Option<BindingRef>,
        write: Option<BindingRef>,
        adaptive: AdaptivePolicy,
    },
}

type ControlVisitor<'a> = dyn FnMut(&ExpandedNode, &SourceUri) -> Result<(), UiDocError> + 'a;

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
struct Machine<'m, 'v> {
    max_depth: usize,
    budget: &'m mut Budget,
    visitor: &'m mut ControlVisitor<'v>,
}

pub(crate) fn expand_module(
    set: &ModuleSet,
    entry: &SourceUri,
    args: &BTreeMap<String, String>,
    prefix: &str,
    max_depth: usize,
    budget: &mut Budget,
    visitor: &mut ControlVisitor<'_>,
) -> Result<ExpandedNode, UiDocError> {
    let mut machine = Machine {
        max_depth,
        budget,
        visitor,
    };
    expand_at(set, entry, args.clone(), prefix.to_owned(), 0, &mut machine)
}

fn expand_at(
    set: &ModuleSet,
    uri: &SourceUri,
    args: BTreeMap<String, String>,
    prefix: String,
    depth: usize,
    machine: &mut Machine<'_, '_>,
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

fn child_path(prefix: &str, id: &NodeId) -> String {
    if prefix.is_empty() {
        id.0.clone()
    } else {
        format!("{prefix}/{id}")
    }
}

fn walk(
    context: &Context<'_>,
    node: &ControlNode,
    depth: usize,
    machine: &mut Machine<'_, '_>,
) -> Result<ExpandedNode, UiDocError> {
    match node {
        ControlNode::Row { id, size, children } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Row {
                id: id.clone(),
                size: *size,
                children: children
                    .iter()
                    .map(|child| walk(context, child, depth, machine))
                    .collect::<Result<_, _>>()?,
            })
        }
        ControlNode::Column { id, size, children } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Column {
                id: id.clone(),
                size: *size,
                children: children
                    .iter()
                    .map(|child| walk(context, child, depth, machine))
                    .collect::<Result<_, _>>()?,
            })
        }
        ControlNode::Slot { id, size, default } => {
            machine.budget.charge(&context.origin)?;
            Ok(ExpandedNode::Slot {
                id: id.clone(),
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
        ControlNode::Control {
            id,
            kind,
            size,
            props,
            read,
            write,
            adaptive,
        } => {
            machine.budget.charge(&context.origin)?;
            let path = child_path(&context.prefix, id);
            let props = props
                .iter()
                .map(|(key, value)| {
                    let value = match value {
                        PropValue::Text(text) => PropValue::Text(substitute(context, text, &path)?),
                        other => other.clone(),
                    };
                    Ok((key.clone(), value))
                })
                .collect::<Result<_, UiDocError>>()?;
            let read = read
                .as_ref()
                .map(|binding| substitute_binding(context, binding, &path))
                .transpose()?;
            let write = write
                .as_ref()
                .map(|binding| substitute_binding(context, binding, &path))
                .transpose()?;
            let node = ExpandedNode::Control {
                path,
                id: id.clone(),
                kind: kind.clone(),
                size: *size,
                props,
                read,
                write,
                adaptive: adaptive.clone(),
            };
            (machine.visitor)(&node, &context.origin)?;
            Ok(node)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        module::BindingRef,
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
                    Control(id: "play", kind: "button",
                        write: Command(id: "deck.transport.toggle_play", with: { "deck": "$deck" })),
                ]))"#,
        );
        resolver
    }

    fn expand(
        set: &ModuleSet,
        uri: &SourceUri,
        args: &BTreeMap<String, String>,
    ) -> Result<ExpandedNode, UiDocError> {
        let mut budget = Budget::new(Limits::default().max_nodes);
        expand_module(
            set,
            uri,
            args,
            "",
            Limits::default().max_depth,
            &mut budget,
            &mut |_, _| Ok(()),
        )
    }

    fn expand_with_depth(
        set: &ModuleSet,
        uri: &SourceUri,
        max_depth: usize,
    ) -> Result<ExpandedNode, UiDocError> {
        let mut budget = Budget::new(Limits::default().max_nodes);
        expand_module(
            set,
            uri,
            &BTreeMap::new(),
            "",
            max_depth,
            &mut budget,
            &mut |_, _| Ok(()),
        )
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
                root: Control(id: "leaf", kind: "text"))"#,
        );
        resolver
    }

    #[kithara::test]
    fn nested_include_receives_substituted_args() {
        let resolver = deck_fixture();
        let (uri, set) =
            load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
        let root = expand(&set, &uri, &args(&[("deck", "b")])).unwrap();

        let ExpandedNode::Column { children, .. } = root else {
            panic!("expected column");
        };
        let ExpandedNode::Row { children, .. } = &children[0] else {
            panic!("expected row");
        };
        let ExpandedNode::Control { path, write, .. } = &children[0] else {
            panic!("expected control");
        };
        assert_eq!(path, "transport/play");
        let Some(BindingRef::Command { with, .. }) = write else {
            panic!("expected command");
        };
        assert_eq!(with.get("deck").map(String::as_str), Some("b"));
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
                root: Control(id: "play", kind: "button"))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
        let limits = Limits::default();
        let mut budget = Budget::new(limits.max_nodes);

        let error = expand_module(
            &set,
            &uri,
            &args(&[("deck", "a")]),
            "deck-a",
            limits.max_depth,
            &mut budget,
            &mut |_, _| Ok(()),
        )
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
                root: Control(id: "price", kind: "text", props: {
                    "text": Text("$$5.99"),
                }))"#,
        );
        let (uri, set) =
            load_module_graph(&resolver, None, "price.kmodule.ron", &Limits::default()).unwrap();

        let root = expand(&set, &uri, &BTreeMap::new()).unwrap();
        let ExpandedNode::Control { props, .. } = root else {
            panic!("expected control");
        };
        assert_eq!(
            props.get("text"),
            Some(&PropValue::Text("$5.99".to_owned()))
        );
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
