use crate::{
    error::UiDocError,
    expand::{Budget, ExpandedNode, expand_module},
    ids::{InstanceId, SourceUri},
    layout::{Axis, LayoutNode, parse_layout},
    registry::{ControlCatalog, EndpointRegistry},
    resolve::load_module_graph,
    size::{SizeSpec, combine_horizontal, combine_vertical, compute_size},
    source::{Limits, SourceResolver},
    validate,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompiledUi {
    pub root: CompiledNode,
    pub size: SizeSpec,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CompiledNode {
    Split {
        axis: Axis,
        children: Vec<(f32, Self)>,
        size: SizeSpec,
    },
    Module {
        instance: InstanceId,
        root: Box<ExpandedNode>,
        size: SizeSpec,
    },
}

/// Compiles a layout and its module graph into renderer-ready UI data.
///
/// # Errors
/// Returns [`UiDocError`] when loading, parsing, expansion, or validation fails.
pub fn compile(
    entry: &str,
    resolver: &dyn SourceResolver,
    catalog: &dyn ControlCatalog,
    endpoints: &dyn EndpointRegistry,
    limits: &Limits,
) -> Result<CompiledUi, UiDocError> {
    let loaded = resolver.load(None, entry)?;
    let bytes = loaded.text.len();
    if bytes > limits.max_bytes {
        return Err(UiDocError::TooLarge {
            origin: loaded.uri,
            bytes,
            max: limits.max_bytes,
        });
    }
    let document = parse_layout(&loaded.text, &loaded.uri)?;
    validate::check_layout_instances(&document, &loaded.uri)?;
    let mut budget = Budget::new(limits.max_nodes);
    let root = build(
        &document.root,
        &loaded.uri,
        resolver,
        catalog,
        endpoints,
        limits,
        &mut budget,
    )?;
    let size = compiled_node_size(&root);
    Ok(CompiledUi { root, size })
}

fn build(
    node: &LayoutNode,
    layout_uri: &SourceUri,
    resolver: &dyn SourceResolver,
    catalog: &dyn ControlCatalog,
    endpoints: &dyn EndpointRegistry,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<CompiledNode, UiDocError> {
    budget.charge(layout_uri)?;
    match node {
        LayoutNode::Split { axis, children } => {
            let children: Vec<_> = children
                .iter()
                .map(|child| {
                    Ok((
                        child.weight,
                        build(
                            &child.node,
                            layout_uri,
                            resolver,
                            catalog,
                            endpoints,
                            limits,
                            budget,
                        )?,
                    ))
                })
                .collect::<Result<_, UiDocError>>()?;
            let sizes = children.iter().map(|(_, child)| compiled_node_size(child));
            let size = match axis {
                Axis::Horizontal => combine_horizontal(sizes),
                Axis::Vertical => combine_vertical(sizes),
            };
            Ok(CompiledNode::Split {
                axis: *axis,
                children,
                size,
            })
        }
        LayoutNode::Module {
            instance,
            source,
            with,
            size,
        } => {
            for value in with.values() {
                if !value.starts_with("$$")
                    && let Some(name) = value.strip_prefix('$')
                {
                    return Err(UiDocError::UnresolvedParam {
                        origin: layout_uri.clone(),
                        name: name.to_owned(),
                        path: instance.0.clone(),
                    });
                }
            }
            let args = with
                .iter()
                .map(|(key, value)| {
                    let value = value
                        .strip_prefix("$$")
                        .map_or_else(|| value.clone(), |literal| format!("${literal}"));
                    (key.clone(), value)
                })
                .collect();
            let (module_uri, set) = load_module_graph(resolver, Some(layout_uri), source, limits)?;
            let root = expand_module(
                &set,
                &module_uri,
                &args,
                &instance.0,
                limits.max_depth,
                budget,
                &mut |control, origin| {
                    validate::check_controls(control, origin, catalog, endpoints)
                },
            )?;
            let size = (*size).unwrap_or_else(|| compute_size(&root, catalog));
            Ok(CompiledNode::Module {
                instance: instance.clone(),
                root: Box::new(root),
                size,
            })
        }
    }
}

fn compiled_node_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}
