use crate::{
    error::UiDocError,
    expand::{ExpandedNode, expand_module},
    ids::{InstanceId, SourceUri},
    layout::{Axis, LayoutNode, parse_layout},
    registry::{ControlCatalog, EndpointRegistry},
    resolve::load_module_graph,
    source::{Limits, SourceResolver},
    validate,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompiledUi {
    pub root: CompiledNode,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CompiledNode {
    Split {
        axis: Axis,
        children: Vec<(f32, Self)>,
    },
    Module {
        instance: InstanceId,
        root: Box<ExpandedNode>,
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
    let document = parse_layout(&loaded.text, &loaded.uri)?;
    validate::check_layout_instances(&document, &loaded.uri)?;
    let mut count = 0;
    let root = build(
        &document.root,
        &loaded.uri,
        resolver,
        catalog,
        endpoints,
        limits,
        &mut count,
    )?;
    if count > limits.max_nodes {
        return Err(UiDocError::NodesExceeded {
            origin: loaded.uri,
            count,
            max: limits.max_nodes,
        });
    }
    Ok(CompiledUi { root })
}

fn build(
    node: &LayoutNode,
    layout_uri: &SourceUri,
    resolver: &dyn SourceResolver,
    catalog: &dyn ControlCatalog,
    endpoints: &dyn EndpointRegistry,
    limits: &Limits,
    count: &mut usize,
) -> Result<CompiledNode, UiDocError> {
    match node {
        LayoutNode::Split { axis, children } => Ok(CompiledNode::Split {
            axis: *axis,
            children: children
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
                            count,
                        )?,
                    ))
                })
                .collect::<Result<_, UiDocError>>()?,
        }),
        LayoutNode::Module {
            instance,
            source,
            with,
        } => {
            let (module_uri, set) = load_module_graph(resolver, Some(layout_uri), source, limits)?;
            let root = expand_module(
                &set,
                &module_uri,
                with,
                &instance.0,
                &mut |control, origin| {
                    validate::check_controls(control, origin, catalog, endpoints)
                },
            )?;
            *count += count_controls(&root);
            Ok(CompiledNode::Module {
                instance: instance.clone(),
                root: Box::new(root),
            })
        }
    }
}

fn count_controls(node: &ExpandedNode) -> usize {
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => children.iter().map(count_controls).sum(),
        ExpandedNode::Control { .. } => 1,
    }
}
