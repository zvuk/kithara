use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::SourceUri,
    module::{ControlNode, ModuleDoc, parse_module},
    source::{Limits, SourceResolver},
    validate,
};

#[derive(Debug, Default)]
pub(crate) struct ModuleSet {
    pub(crate) defs: BTreeMap<SourceUri, ModuleDoc>,
}

pub(crate) fn load_module_graph(
    resolver: &dyn SourceResolver,
    base: Option<&SourceUri>,
    rel: &str,
    limits: &Limits,
) -> Result<(SourceUri, ModuleSet), UiDocError> {
    let mut set = ModuleSet::default();
    let mut stack = Vec::new();
    let uri = load_rec(resolver, base, rel, limits, &mut set, &mut stack, 0)?;
    Ok((uri, set))
}

fn load_rec(
    resolver: &dyn SourceResolver,
    base: Option<&SourceUri>,
    rel: &str,
    limits: &Limits,
    set: &mut ModuleSet,
    stack: &mut Vec<SourceUri>,
    depth: usize,
) -> Result<SourceUri, UiDocError> {
    let loaded = resolver.load(base, rel)?;
    let bytes = loaded.text.len();
    if bytes > limits.max_bytes {
        return Err(UiDocError::TooLarge {
            origin: loaded.uri,
            bytes,
            max: limits.max_bytes,
        });
    }
    if stack.contains(&loaded.uri) {
        let mut chain = stack.clone();
        chain.push(loaded.uri);
        return Err(UiDocError::IncludeCycle { chain });
    }
    if depth >= limits.max_depth {
        return Err(UiDocError::DepthExceeded {
            origin: loaded.uri,
            depth,
            max: limits.max_depth,
        });
    }
    if set.defs.contains_key(&loaded.uri) {
        return Ok(loaded.uri);
    }
    let doc = parse_module(&loaded.text, &loaded.uri)?;
    validate::check_module_node_ids(&doc, &loaded.uri)?;
    stack.push(loaded.uri.clone());
    walk_includes(resolver, &loaded.uri, &doc.root, limits, set, stack, depth)?;
    let popped = stack.pop();
    debug_assert_eq!(popped.as_ref(), Some(&loaded.uri));
    set.defs.insert(loaded.uri.clone(), doc);
    Ok(loaded.uri)
}

fn walk_includes(
    resolver: &dyn SourceResolver,
    origin: &SourceUri,
    node: &ControlNode,
    limits: &Limits,
    set: &mut ModuleSet,
    stack: &mut Vec<SourceUri>,
    depth: usize,
) -> Result<(), UiDocError> {
    match node {
        ControlNode::Row { children, .. } | ControlNode::Column { children, .. } => {
            for child in children {
                walk_includes(resolver, origin, child, limits, set, stack, depth)?;
            }
            Ok(())
        }
        ControlNode::Slot { default, .. } => {
            for child in default {
                walk_includes(resolver, origin, child, limits, set, stack, depth)?;
            }
            Ok(())
        }
        ControlNode::Include { source, .. } => {
            load_rec(
                resolver,
                Some(origin),
                source,
                limits,
                set,
                stack,
                depth + 1,
            )?;
            Ok(())
        }
        ControlNode::Control { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::source::{Limits, MemResolver};

    fn module(id: &str, body: &str) -> String {
        format!(r#"(schema: "kithara.module", version: 1, id: "{id}", root: {body})"#)
    }

    #[kithara::test]
    fn loads_nested_includes_two_levels_deep() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [Include(id: "b", source: "sub/b.kmodule.ron")])"#,
            ),
        );
        resolver.insert(
            "sub/b.kmodule.ron",
            &module(
                "b",
                r#"Row(children: [Include(id: "c", source: "c.kmodule.ron")])"#,
            ),
        );
        resolver.insert(
            "sub/c.kmodule.ron",
            &module("c", r#"Control(id: "x", kind: "text")"#),
        );

        let (uri, set) =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap();
        assert_eq!(uri.0, "a.kmodule.ron");
        assert_eq!(set.defs.len(), 3);
    }

    #[kithara::test]
    fn include_cycle_reports_full_chain() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [Include(id: "b", source: "b.kmodule.ron")])"#,
            ),
        );
        resolver.insert(
            "b.kmodule.ron",
            &module(
                "b",
                r#"Row(children: [Include(id: "a", source: "a.kmodule.ron")])"#,
            ),
        );

        let error =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("a.kmodule.ron -> b.kmodule.ron -> a.kmodule.ron"),
            "{message}"
        );
    }

    #[kithara::test]
    fn depth_limit_is_enforced() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [Include(id: "b", source: "b.kmodule.ron")])"#,
            ),
        );
        resolver.insert(
            "b.kmodule.ron",
            &module("b", r#"Control(id: "x", kind: "text")"#),
        );

        let limits = Limits {
            max_depth: 1,
            ..Limits::default()
        };
        let error = load_module_graph(&resolver, None, "a.kmodule.ron", &limits).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::DepthExceeded {
                depth: 1,
                max: 1,
                ..
            }
        ));
    }

    #[kithara::test]
    fn shared_include_is_loaded_once_not_a_cycle() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "a.kmodule.ron",
            &module(
                "a",
                r#"Row(children: [
                    Include(id: "left", source: "shared.kmodule.ron"),
                    Include(id: "right", source: "shared.kmodule.ron"),
                ])"#,
            ),
        );
        resolver.insert(
            "shared.kmodule.ron",
            &module("shared", r#"Control(id: "x", kind: "text")"#),
        );

        load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap();
    }

    #[kithara::test]
    fn oversized_included_source_is_rejected() {
        let entry = module("a", r#"Include(id: "b", source: "b.kmodule.ron")"#);
        let child = module(
            "b",
            &format!(
                r#"Control(id: "text", kind: "text", props: {{ "text": Text("{}") }})"#,
                "x".repeat(256)
            ),
        );
        assert!(child.len() > entry.len());
        let mut resolver = MemResolver::default();
        resolver.insert("a.kmodule.ron", &entry);
        resolver.insert("b.kmodule.ron", &child);
        let limits = Limits {
            max_bytes: entry.len(),
            ..Limits::default()
        };

        let error = load_module_graph(&resolver, None, "a.kmodule.ron", &limits).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::TooLarge { origin, bytes, max }
                if origin.0 == "b.kmodule.ron" && bytes == child.len() && max == entry.len()
        ));
    }
}
