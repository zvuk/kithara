use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{ControlKind, NodeId, SourceUri},
    module::{AdaptivePolicy, BindingRef, ControlNode, PropValue},
    resolve::ModuleSet,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExpandedNode {
    Row {
        id: Option<NodeId>,
        children: Vec<Self>,
    },
    Column {
        id: Option<NodeId>,
        children: Vec<Self>,
    },
    Slot {
        id: NodeId,
        children: Vec<Self>,
    },
    Control {
        path: String,
        id: NodeId,
        kind: ControlKind,
        props: BTreeMap<String, PropValue>,
        read: Option<BindingRef>,
        write: Option<BindingRef>,
        adaptive: AdaptivePolicy,
    },
}

type ControlVisitor<'a> = dyn FnMut(&ExpandedNode, &SourceUri) -> Result<(), UiDocError> + 'a;

struct Context<'a> {
    set: &'a ModuleSet,
    origin: SourceUri,
    args: BTreeMap<String, String>,
    prefix: String,
}

pub(crate) fn expand_module(
    set: &ModuleSet,
    entry: &SourceUri,
    args: &BTreeMap<String, String>,
    prefix: &str,
    visitor: &mut ControlVisitor<'_>,
) -> Result<ExpandedNode, UiDocError> {
    expand_at(set, entry, args.clone(), prefix.to_owned(), visitor)
}

fn expand_at(
    set: &ModuleSet,
    uri: &SourceUri,
    args: BTreeMap<String, String>,
    prefix: String,
    visitor: &mut ControlVisitor<'_>,
) -> Result<ExpandedNode, UiDocError> {
    let doc = set.defs.get(uri).ok_or_else(|| UiDocError::NotFound {
        origin: uri.clone(),
        rel: uri.0.clone(),
    })?;
    for name in args.keys() {
        if !doc.parameters.contains(name) {
            return Err(UiDocError::UnknownParam {
                origin: uri.clone(),
                name: name.clone(),
            });
        }
    }
    let context = Context {
        set,
        origin: uri.clone(),
        args,
        prefix,
    };
    walk(&context, &doc.root, visitor)
}

fn substitute(context: &Context<'_>, value: &str, path: &str) -> Result<String, UiDocError> {
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
    visitor: &mut ControlVisitor<'_>,
) -> Result<ExpandedNode, UiDocError> {
    match node {
        ControlNode::Row { id, children } => Ok(ExpandedNode::Row {
            id: id.clone(),
            children: children
                .iter()
                .map(|child| walk(context, child, visitor))
                .collect::<Result<_, _>>()?,
        }),
        ControlNode::Column { id, children } => Ok(ExpandedNode::Column {
            id: id.clone(),
            children: children
                .iter()
                .map(|child| walk(context, child, visitor))
                .collect::<Result<_, _>>()?,
        }),
        ControlNode::Slot { id, default } => Ok(ExpandedNode::Slot {
            id: id.clone(),
            children: default
                .iter()
                .map(|child| walk(context, child, visitor))
                .collect::<Result<_, _>>()?,
        }),
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
            expand_at(context.set, &target, args, path, visitor)
        }
        ControlNode::Control {
            id,
            kind,
            props,
            read,
            write,
            adaptive,
        } => {
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
                props,
                read,
                write,
                adaptive: adaptive.clone(),
            };
            visitor(&node, &context.origin)?;
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
        expand_module(set, uri, args, "", &mut |_, _| Ok(()))
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
}
