use std::collections::{BTreeMap, BTreeSet};

use syn::{Expr, ItemImpl, Type, punctuated::Punctuated, visit::Visit};

use super::graph::{Edge, EdgeKind, Evidence, EvidenceGraph, Node, NodeId, NodeKind};
use crate::common::parse::is_pub_visibility;

#[derive(Default)]
pub(super) struct CallIndex {
    functions: BTreeMap<String, BTreeSet<NodeId>>,
    methods: BTreeMap<String, BTreeSet<NodeId>>,
    abstractions: BTreeMap<(String, String), BTreeSet<NodeId>>,
}

impl CallIndex {
    fn insert(&mut self, name: String, owner: Option<&str>, id: NodeId) {
        if let Some(owner) = owner {
            self.methods
                .entry(format!("{owner}::{name}"))
                .or_default()
                .insert(id);
        } else {
            self.functions.entry(name).or_default().insert(id);
        }
    }

    fn candidates(&self, path: &[String]) -> impl Iterator<Item = &NodeId> {
        let candidates = match path {
            [name] => self.functions.get(name),
            [.., owner, name] => self.methods.get(&format!("{owner}::{name}")),
            [] => None,
        };
        candidates.into_iter().flat_map(|ids| ids.iter())
    }

    fn insert_abstraction(&mut self, kind: &str, name: &str, id: NodeId) {
        self.abstractions
            .entry((kind.to_string(), name.to_string()))
            .or_default()
            .insert(id);
    }

    fn abstraction(&self, kind: &str, name: &str, module: &str) -> Option<NodeId> {
        let candidates = self
            .abstractions
            .get(&(kind.to_string(), name.to_string()))?;
        candidates
            .iter()
            .find(|id| id.module == module)
            .or_else(|| {
                (candidates.len() == 1)
                    .then(|| candidates.first())
                    .flatten()
            })
            .cloned()
    }
}

pub(super) struct SourceUnit<'a> {
    pub(super) package: &'a str,
    pub(super) target: &'a str,
    pub(super) module: &'a str,
    pub(super) relative: &'a str,
    pub(super) file: &'a syn::File,
}

pub(super) fn is_test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        let syn::Meta::List(list) = &attribute.meta else {
            return false;
        };
        list.path.is_ident("cfg")
            && list
                .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .is_ok_and(|items| items.iter().all(meta_requires_test))
    })
}

fn meta_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            .is_ok_and(|items| items.iter().any(meta_requires_test)),
        syn::Meta::List(list) if list.path.is_ident("any") => list
            .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            .is_ok_and(|items| !items.is_empty() && items.iter().all(meta_requires_test)),
        syn::Meta::List(_) | syn::Meta::NameValue(_) => false,
    }
}

pub(super) fn collect_abstractions(
    unit: &SourceUnit<'_>,
    graph: &mut EvidenceGraph,
    index: &mut CallIndex,
) {
    let mut visitor = AbstractionVisitor {
        unit,
        inline_modules: Vec::new(),
        graph,
        index,
    };
    visitor.visit_file(unit.file);
}

pub(super) fn collect_definitions(
    unit: &SourceUnit<'_>,
    graph: &mut EvidenceGraph,
    index: &mut CallIndex,
) {
    let mut visitor = DefinitionVisitor {
        unit,
        inline_modules: Vec::new(),
        impl_type: None,
        impl_owner: None,
        graph,
        index,
    };
    visitor.visit_file(unit.file);
}

pub(super) fn collect_edges(unit: &SourceUnit<'_>, graph: &mut EvidenceGraph, index: &CallIndex) {
    let mut visitor = CallVisitor {
        unit,
        inline_modules: Vec::new(),
        impl_type: None,
        caller: None,
        graph,
        index,
    };
    visitor.visit_file(unit.file);
}

struct AbstractionVisitor<'a, 'unit> {
    unit: &'unit SourceUnit<'unit>,
    inline_modules: Vec<String>,
    graph: &'a mut EvidenceGraph,
    index: &'a mut CallIndex,
}

impl AbstractionVisitor<'_, '_> {
    fn module(&self) -> String {
        joined_module(self.unit.module, &self.inline_modules)
    }

    fn add_named(&mut self, ident: &syn::Ident, kind: NodeKind, identity_kind: &str) {
        let module = self.module();
        let start = ident.span().start();
        let origin = format!("{}:{}", self.unit.relative, start.line);
        let id = NodeId::abstraction(
            self.unit.package,
            self.unit.target,
            &module,
            identity_kind,
            &ident.to_string(),
        );
        self.graph.merge_node(
            Node::new(
                id.clone(),
                kind,
                ident.to_string(),
                Evidence::static_fact(origin.clone()),
            )
            .at_position(self.unit.relative, start.line, start.column),
        );
        self.graph.merge_edge(Edge::new(
            NodeId::module(self.unit.package, self.unit.target, &module),
            id.clone(),
            EdgeKind::Contains,
            Evidence::static_fact(origin),
        ));
        self.index
            .insert_abstraction(identity_kind, &ident.to_string(), id);
    }

    fn add_module_functions(&mut self, ident: &syn::Ident) {
        let module = self.module();
        let start = ident.span().start();
        let origin = format!("{}:{}", self.unit.relative, start.line);
        let id = NodeId::module_functions(self.unit.package, self.unit.target, &module);
        let label = if module == "crate" {
            "crate functions".to_string()
        } else {
            format!(
                "{} functions",
                module.rsplit("::").next().unwrap_or(module.as_str())
            )
        };
        self.graph.merge_node(
            Node::new(
                id.clone(),
                NodeKind::ModuleFunctions,
                label,
                Evidence::static_fact(origin.clone()),
            )
            .at_position(self.unit.relative, start.line, start.column),
        );
        self.graph.merge_edge(Edge::new(
            NodeId::module(self.unit.package, self.unit.target, &module),
            id,
            EdgeKind::Contains,
            Evidence::static_fact(origin),
        ));
    }
}

impl<'ast> Visit<'ast> for AbstractionVisitor<'_, '_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.content.is_none() || is_test_only(&item.attrs) {
            return;
        }
        let parent = self.module();
        self.inline_modules.push(item.ident.to_string());
        let module = self.module();
        let start = item.ident.span().start();
        let origin = format!("{}:{}", self.unit.relative, start.line);
        let id = NodeId::module(self.unit.package, self.unit.target, &module);
        self.graph.merge_node(
            Node::new(
                id.clone(),
                NodeKind::Module,
                item.ident.to_string(),
                Evidence::static_fact(origin.clone()),
            )
            .at_position(self.unit.relative, start.line, start.column),
        );
        self.graph.merge_edge(Edge::new(
            NodeId::module(self.unit.package, self.unit.target, &parent),
            id,
            EdgeKind::Contains,
            Evidence::static_fact(origin),
        ));
        syn::visit::visit_item_mod(self, item);
        self.inline_modules.pop();
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_named(&item.ident, NodeKind::ConcreteType, "type");
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_named(&item.ident, NodeKind::ConcreteType, "type");
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_named(&item.ident, NodeKind::ConcreteType, "type");
        syn::visit::visit_item_union(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_named(&item.ident, NodeKind::ConcreteType, "type");
        syn::visit::visit_item_type(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_named(&item.ident, NodeKind::Trait, "trait");
        let module = self.module();
        let owner = NodeId::abstraction(
            self.unit.package,
            self.unit.target,
            &module,
            "trait",
            &item.ident.to_string(),
        );
        for method in &item.items {
            let syn::TraitItem::Fn(method) = method else {
                continue;
            };
            let start = method.sig.ident.span().start();
            let origin = format!("{}:{}", self.unit.relative, start.line);
            let symbol = format!("{}::{}", item.ident, method.sig.ident);
            let id = NodeId::symbol(self.unit.package, self.unit.target, &module, symbol.clone());
            self.graph.merge_node(
                Node::new(
                    id.clone(),
                    NodeKind::TraitMethod,
                    format!("fn {symbol}()"),
                    Evidence::static_fact(origin.clone()),
                )
                .at_position(self.unit.relative, start.line, start.column),
            );
            self.graph.merge_edge(Edge::new(
                owner.clone(),
                id,
                EdgeKind::Contains,
                Evidence::static_fact(origin),
            ));
        }
        syn::visit::visit_item_trait(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_module_functions(&item.sig.ident);
        syn::visit::visit_item_fn(self, item);
    }
}

struct DefinitionVisitor<'a, 'unit> {
    unit: &'unit SourceUnit<'unit>,
    inline_modules: Vec<String>,
    impl_type: Option<String>,
    impl_owner: Option<NodeId>,
    graph: &'a mut EvidenceGraph,
    index: &'a mut CallIndex,
}

impl DefinitionVisitor<'_, '_> {
    fn module(&self) -> String {
        joined_module(self.unit.module, &self.inline_modules)
    }

    fn add_function(&mut self, signature: &syn::Signature, visibility: &syn::Visibility) {
        let module = self.module();
        let name = signature.ident.to_string();
        let start = signature.ident.span().start();
        let symbol = self
            .impl_type
            .as_ref()
            .map_or_else(|| name.clone(), |owner| format!("{owner}::{name}"));
        let id = NodeId::symbol(self.unit.package, self.unit.target, &module, symbol.clone());
        let kind = if self
            .impl_type
            .as_deref()
            .is_some_and(|owner| returns_owner(signature, owner))
        {
            NodeKind::Constructor
        } else if is_pub_visibility(visibility) {
            NodeKind::PublicFunction
        } else {
            NodeKind::Function
        };
        let origin = format!("{}:{}", self.unit.relative, start.line);
        self.graph.merge_node(
            Node::new(
                id.clone(),
                kind,
                format!("fn {symbol}()"),
                Evidence::static_fact(origin.clone()),
            )
            .at_position(self.unit.relative, start.line, start.column),
        );
        let owner = self.impl_type.as_ref().map_or_else(
            || NodeId::module_functions(self.unit.package, self.unit.target, &module),
            |_| {
                self.impl_owner.clone().unwrap_or_else(|| {
                    NodeId::module_functions(self.unit.package, self.unit.target, &module)
                })
            },
        );
        self.graph.merge_edge(Edge::new(
            owner,
            id.clone(),
            EdgeKind::Contains,
            Evidence::static_fact(origin),
        ));
        self.index.insert(name, self.impl_type.as_deref(), id);
    }
}

impl<'ast> Visit<'ast> for DefinitionVisitor<'_, '_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.content.is_some() && !is_test_only(&item.attrs) {
            self.inline_modules.push(item.ident.to_string());
            syn::visit::visit_item_mod(self, item);
            self.inline_modules.pop();
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if is_test_only(&item.attrs) {
            return;
        }
        let module = self.module();
        let concrete = type_name(&item.self_ty);
        let concrete_owner = self.index.abstraction("type", &concrete, &module);
        if let Some((trait_path, _)) = &item.trait_ {
            let trait_name = trait_path
                .segments
                .last()
                .map(|segment| segment.ident.to_string());
            if let Some(trait_name) = trait_name {
                let target = self.index.abstraction("trait", &trait_name, &module);
                if let (Some(source), Some(target)) = (concrete_owner.clone(), target) {
                    let start = item.impl_token.span.start();
                    self.graph.merge_edge(Edge::new(
                        source,
                        target,
                        EdgeKind::Implements,
                        Evidence::static_fact(format!(
                            "{}:{}:{}",
                            self.unit.relative, start.line, start.column
                        )),
                    ));
                }
            }
        }
        let previous = self.impl_type.replace(concrete);
        let previous_owner = std::mem::replace(&mut self.impl_owner, concrete_owner);
        syn::visit::visit_item_impl(self, item);
        self.impl_type = previous;
        self.impl_owner = previous_owner;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_function(&item.sig, &item.vis);
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.add_function(&item.sig, &item.vis);
        syn::visit::visit_impl_item_fn(self, item);
    }
}

struct CallVisitor<'a, 'unit> {
    unit: &'unit SourceUnit<'unit>,
    inline_modules: Vec<String>,
    impl_type: Option<String>,
    caller: Option<NodeId>,
    graph: &'a mut EvidenceGraph,
    index: &'a CallIndex,
}

impl CallVisitor<'_, '_> {
    fn module(&self) -> String {
        joined_module(self.unit.module, &self.inline_modules)
    }

    fn function_id(&self, ident: &syn::Ident) -> NodeId {
        let name = ident.to_string();
        let symbol = self
            .impl_type
            .as_ref()
            .map_or_else(|| name.clone(), |owner| format!("{owner}::{name}"));
        NodeId::symbol(self.unit.package, self.unit.target, self.module(), symbol)
    }

    fn add_call(&mut self, path: &[String], line: usize, column: usize) {
        let Some(caller) = self.caller.clone() else {
            return;
        };
        let Some(name) = path.last() else {
            return;
        };
        let origin = format!("{}:{line}:{column}", self.unit.relative);
        let candidates = self.index.candidates(path).cloned().collect::<Vec<_>>();
        if candidates.is_empty() {
            let target = NodeId::symbol(
                self.unit.package,
                self.unit.target,
                self.module(),
                format!("<unresolved:{name}>"),
            );
            self.graph.merge_node(
                Node::new(
                    target.clone(),
                    NodeKind::UnresolvedCall,
                    format!("{name}(?)"),
                    Evidence::unresolved(origin.clone()),
                )
                .at(self.unit.relative, line),
            );
            self.graph.merge_edge(Edge::new(
                caller,
                target,
                EdgeKind::Calls,
                Evidence::unresolved(origin),
            ));
            return;
        }
        for target in candidates {
            self.graph.merge_edge(Edge::new(
                caller.clone(),
                target,
                EdgeKind::Calls,
                Evidence::inferred(origin.clone()),
            ));
        }
    }

    fn add_spawn(&mut self, label: String, line: usize, column: usize) {
        let Some(caller) = self.caller.clone() else {
            return;
        };
        let module = self.module();
        let task = NodeId::site(
            self.unit.package,
            self.unit.target,
            &module,
            &format!("spawn@{line}:{column}"),
        );
        let origin = format!("{}:{line}:{column}", self.unit.relative);
        self.graph.merge_node(
            Node::new(
                task.clone(),
                NodeKind::Task,
                label,
                Evidence::static_fact(origin.clone()),
            )
            .at(self.unit.relative, line),
        );
        self.graph.merge_edge(Edge::new(
            caller.clone(),
            task.clone(),
            EdgeKind::Spawns,
            Evidence::static_fact(origin.clone()),
        ));
        self.graph.merge_edge(Edge::new(
            caller,
            task,
            EdgeKind::Contains,
            Evidence::static_fact(origin),
        ));
    }
}

impl<'ast> Visit<'ast> for CallVisitor<'_, '_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.content.is_some() && !is_test_only(&item.attrs) {
            self.inline_modules.push(item.ident.to_string());
            syn::visit::visit_item_mod(self, item);
            self.inline_modules.pop();
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if is_test_only(&item.attrs) {
            return;
        }
        let previous = self.impl_type.replace(type_name(&item.self_ty));
        syn::visit::visit_item_impl(self, item);
        self.impl_type = previous;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if is_test_only(&item.attrs) {
            return;
        }
        let previous = self.caller.replace(self.function_id(&item.sig.ident));
        syn::visit::visit_block(self, &item.block);
        self.caller = previous;
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if is_test_only(&item.attrs) {
            return;
        }
        let previous = self.caller.replace(self.function_id(&item.sig.ident));
        syn::visit::visit_block(self, &item.block);
        self.caller = previous;
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        use syn::spanned::Spanned as _;

        if let Expr::Path(path) = call.func.as_ref() {
            let segments = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            let start = call.func.span().start();
            if is_spawn(&segments) {
                self.add_spawn(segments.join("::"), start.line, start.column);
            } else if !is_resource_call(&segments) {
                self.add_call(&segments, start.line, start.column);
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let start = call.method.span().start();
        self.add_call(
            &[format!("<receiver>::{}", call.method)],
            start.line,
            start.column,
        );
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn joined_module(base: &str, inline: &[String]) -> String {
    if inline.is_empty() {
        base.to_string()
    } else if base == "crate" {
        inline.join("::")
    } else {
        format!("{base}::{}", inline.join("::"))
    }
}

fn type_name(ty: &Type) -> String {
    let Type::Path(path) = ty else {
        return "?".to_string();
    };
    path.path
        .segments
        .last()
        .map_or_else(|| "?".to_string(), |segment| segment.ident.to_string())
}

fn returns_owner(signature: &syn::Signature, owner: &str) -> bool {
    let syn::ReturnType::Type(_, returned) = &signature.output else {
        return false;
    };
    type_owns(returned, owner)
}

fn type_owns(ty: &Type, owner: &str) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    path.path.segments.iter().any(|segment| {
        segment.ident == "Self"
            || segment.ident == owner
            || match &segment.arguments {
                syn::PathArguments::AngleBracketed(arguments) => {
                    arguments.args.iter().any(|argument| {
                        matches!(
                            argument,
                            syn::GenericArgument::Type(inner) if type_owns(inner, owner)
                        )
                    })
                }
                syn::PathArguments::None | syn::PathArguments::Parenthesized(_) => false,
            }
    })
}

fn is_spawn(segments: &[String]) -> bool {
    matches!(segments, [.., owner, method] if method == "spawn" && matches!(owner.as_str(), "thread" | "task" | "tokio"))
}

fn is_resource_call(segments: &[String]) -> bool {
    matches!(segments, [.., owner, method] if owner == "Arc" && matches!(method.as_str(), "new" | "new_cyclic" | "from" | "clone"))
        || matches!(segments, [.., owner, method] if owner == "mpsc" && matches!(method.as_str(), "channel" | "sync_channel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(source: &str) -> EvidenceGraph {
        let file = syn::parse_file(source).expect("source should parse");
        let unit = SourceUnit {
            package: "demo",
            target: "demo",
            module: "crate",
            relative: "src/lib.rs",
            file: &file,
        };
        let mut graph = EvidenceGraph::default();
        let mut index = CallIndex::default();
        collect_abstractions(&unit, &mut graph, &mut index);
        collect_definitions(&unit, &mut graph, &mut index);
        collect_edges(&unit, &mut graph, &index);
        graph
    }

    #[test]
    fn qualified_external_call_does_not_bind_to_same_named_local_function() {
        let graph = collect(
            "fn visit_file() {}\n\
             fn analyze(visitor: &mut dyn syn::visit::Visit<'_>, file: &syn::File) {\n\
                 syn::visit::visit_file(visitor, file);\n\
             }",
        );
        let analyze = NodeId::symbol("demo", "demo", "crate", "analyze");
        let local = NodeId::symbol("demo", "demo", "crate", "visit_file");

        assert!(!graph.edges().any(|edge| {
            edge.kind == EdgeKind::Calls && edge.source == analyze && edge.target == local
        }));
    }

    #[test]
    fn type_qualified_method_call_uses_the_named_owner() {
        let graph = collect(
            "struct Worker;\n\
             impl Worker { fn run() {} }\n\
             fn start() { Worker::run(); }",
        );
        let start = NodeId::symbol("demo", "demo", "crate", "start");
        let run = NodeId::symbol("demo", "demo", "crate", "Worker::run");

        assert!(graph.edges().any(|edge| {
            edge.kind == EdgeKind::Calls && edge.source == start && edge.target == run
        }));
    }

    #[test]
    fn function_location_points_at_the_name_after_attributes() {
        let graph = collect("/// docs\n#[inline]\npub fn start() {}\n");
        let start = NodeId::symbol("demo", "demo", "crate", "start");
        let location = graph
            .nodes()
            .find(|node| node.id == start)
            .and_then(|node| node.location.as_ref())
            .expect("function location");

        assert_eq!(location.line, 3);
        assert_eq!(location.column, 7);
    }

    #[test]
    fn test_only_cfg_requires_every_possible_branch_to_require_test() {
        fn attributes(source: &str) -> Vec<syn::Attribute> {
            let file = syn::parse_file(source).expect("cfg fixture should parse");
            let syn::Item::Mod(module) = &file.items[0] else {
                panic!("cfg fixture should contain a module");
            };
            module.attrs.clone()
        }

        assert!(is_test_only(&attributes("#[cfg(test)] mod fixture {}")));
        assert!(is_test_only(&attributes(
            "#[cfg(all(test, unix))] mod fixture {}"
        )));
        assert!(!is_test_only(&attributes(
            "#[cfg(any(test, unix))] mod fixture {}"
        )));
        assert!(!is_test_only(&attributes(
            "#[cfg(not(test))] mod fixture {}"
        )));
    }

    #[test]
    fn impl_in_another_module_belongs_to_the_declared_type_abstraction() {
        let declaration = syn::parse_file("struct Worker;").expect("declaration should parse");
        let implementation =
            syn::parse_file("impl Worker { fn run(&self) {} }").expect("impl should parse");
        let declaration_unit = SourceUnit {
            package: "demo",
            target: "demo",
            module: "model",
            relative: "src/model.rs",
            file: &declaration,
        };
        let implementation_unit = SourceUnit {
            package: "demo",
            target: "demo",
            module: "behavior",
            relative: "src/behavior.rs",
            file: &implementation,
        };
        let mut graph = EvidenceGraph::default();
        let mut index = CallIndex::default();
        collect_abstractions(&declaration_unit, &mut graph, &mut index);
        collect_abstractions(&implementation_unit, &mut graph, &mut index);
        collect_definitions(&declaration_unit, &mut graph, &mut index);
        collect_definitions(&implementation_unit, &mut graph, &mut index);

        let owner = NodeId::abstraction("demo", "demo", "model", "type", "Worker");
        let method = NodeId::symbol("demo", "demo", "behavior", "Worker::run");
        assert!(graph.edges().any(|edge| {
            edge.kind == EdgeKind::Contains && edge.source == owner && edge.target == method
        }));
    }
}
