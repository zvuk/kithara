use crate::{
    error::UiDocError,
    expand::{Binding, Budget, ControlSite, ExpandedNode, Expander},
    ids::{InternId, Interner, SourceUri, StrArena},
    layout::{Axis, FrameSides, LayoutNode, parse_layout},
    module::ChromeStyle,
    registry::EndpointRegistry,
    resolve::load_module_graph,
    size::{SizeSpec, combine_horizontal, combine_vertical, compute_size},
    skin::SkinDoc,
    source::{SourceResolver, UiConfig},
    validate,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompiledUi {
    pub root: CompiledNode,
    pub size: SizeSpec,
    arena: StrArena,
}

impl CompiledUi {
    delegate::delegate! {
        to self.arena {
            /// Resolves a string interned by this compiled UI.
            #[must_use]
            pub fn resolve(&self, id: InternId) -> &str;
        }
    }
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
        instance: InternId,
        module: InternId,
        title: Option<InternId>,
        chip: Option<InternId>,
        chrome: ChromeStyle,
        frame: FrameSides,
        corners: bool,
        footer: Option<Binding>,
        collapsed: InternId,
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
    endpoints: &dyn EndpointRegistry,
    skin: &SkinDoc,
    config: &UiConfig,
) -> Result<CompiledUi, UiDocError> {
    let loaded = resolver.load(None, entry)?;
    let bytes = loaded.text.len();
    if bytes > config.limits.max_bytes {
        return Err(UiDocError::TooLarge {
            origin: loaded.uri,
            bytes,
            max: config.limits.max_bytes,
        });
    }
    let document = parse_layout(&loaded.text, &loaded.uri)?;
    validate::check_layout_instances(&document, &loaded.uri)?;
    let mut budget = Budget::new(config.limits.max_nodes);
    let mut interner = Interner::new(config.max_arena_bytes);
    let root = Compiler {
        resolver,
        endpoints,
        skin,
        config,
        budget: &mut budget,
        interner: &mut interner,
    }
    .build(&document.root, &loaded.uri)?;
    let size = compiled_node_size(&root);
    let arena = interner.finish();
    Ok(CompiledUi { root, size, arena })
}

struct Compiler<'a> {
    resolver: &'a dyn SourceResolver,
    endpoints: &'a dyn EndpointRegistry,
    skin: &'a SkinDoc,
    config: &'a UiConfig,
    budget: &'a mut Budget,
    interner: &'a mut Interner,
}

impl Compiler<'_> {
    fn build(
        &mut self,
        node: &LayoutNode,
        layout_uri: &SourceUri,
    ) -> Result<CompiledNode, UiDocError> {
        self.budget.charge(layout_uri)?;
        match node {
            LayoutNode::Split { axis, children } => {
                let children: Vec<_> = children
                    .iter()
                    .map(|child| Ok((child.weight, self.build(&child.node, layout_uri)?)))
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
                frame,
                corners,
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
                let (module_uri, set) = load_module_graph(
                    self.resolver,
                    Some(layout_uri),
                    source,
                    &self.config.limits,
                )?;
                let mut visitor = |site: ControlSite<'_>, origin: &SourceUri| {
                    validate::check_controls(site, origin, self.endpoints)
                };
                let document = set
                    .defs
                    .get(&module_uri)
                    .ok_or_else(|| UiDocError::NotFound {
                        origin: module_uri.clone(),
                        rel: module_uri.0.clone(),
                    })?;
                validate::check_module_footer(document, &module_uri, self.endpoints)?;
                let expanded = Expander::new(
                    self.config.limits.max_depth,
                    self.budget,
                    self.interner,
                    &mut visitor,
                )
                .expand_module(&set, &module_uri, &args, &instance.0)?;
                let size = (*size).unwrap_or_else(|| {
                    crate::size::with_module_chrome(
                        compute_size(&expanded.root, self.skin),
                        expanded.chrome,
                        self.skin,
                    )
                });
                let instance = self.interner.intern(&instance.0, layout_uri)?;
                Ok(CompiledNode::Module {
                    instance,
                    module: expanded.module,
                    title: expanded.title,
                    chip: expanded.chip,
                    chrome: expanded.chrome,
                    frame: *frame,
                    corners: *corners,
                    footer: expanded.footer,
                    collapsed: expanded.collapsed,
                    root: Box::new(expanded.root),
                    size,
                })
            }
        }
    }
}

fn compiled_node_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}
