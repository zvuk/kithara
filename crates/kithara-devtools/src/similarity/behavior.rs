use std::collections::BTreeMap;

use proc_macro2::{TokenStream, TokenTree};
use syn::{
    Block, Expr, FnArg, GenericArgument, GenericParam, Lit, PathArguments, ReceiverKind, Safety,
    Signature, Stmt, Type,
    visit::{self, Visit},
};

const WL_ROUNDS: usize = 3;

#[derive(Debug, Clone)]
struct Node {
    label: String,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: &'static str,
}

#[derive(Debug, Clone)]
pub(super) struct BehaviorGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    signature: String,
    label_counts: BTreeMap<String, usize>,
    edge_counts: BTreeMap<String, usize>,
    wl_fingerprint: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
pub(super) struct BehaviorComparison {
    pub(super) score: f64,
    pub(super) shared_labels: Vec<String>,
}

impl BehaviorGraph {
    pub(super) fn from_function(signature: &Signature, block: &Block) -> Self {
        Self::from_function_with_generics(signature, block, BTreeMap::new())
    }

    pub(super) fn from_method(
        signature: &Signature,
        block: &Block,
        owner_generics: BTreeMap<String, usize>,
    ) -> Self {
        Self::from_function_with_generics(signature, block, owner_generics)
    }

    fn from_function_with_generics(
        signature: &Signature,
        block: &Block,
        mut generics: BTreeMap<String, usize>,
    ) -> Self {
        let next_index = generics.len();
        generics.extend(
            signature
                .generics
                .params
                .iter()
                .filter_map(|parameter| match parameter {
                    GenericParam::Type(parameter) => Some(parameter.ident.to_string()),
                    GenericParam::Lifetime(_) | GenericParam::Const(_) => None,
                })
                .enumerate()
                .map(|(index, name)| (name, next_index + index)),
        );
        let signature = signature_label(signature, &generics);
        let mut builder = GraphBuilder {
            graph: Self {
                nodes: vec![Node {
                    label: signature.clone(),
                }],
                edges: Vec::new(),
                signature,
                label_counts: BTreeMap::new(),
                edge_counts: BTreeMap::new(),
                wl_fingerprint: BTreeMap::new(),
            },
            parents: vec![0],
        };
        builder.visit_block(block);
        let mut graph = builder.graph;
        graph.label_counts = label_counts(graph.nodes.iter().map(|node| node.label.as_str()));
        graph.edge_counts = label_counts(graph.edges.iter().map(|edge| edge.label));
        graph.wl_fingerprint = graph.compute_wl_fingerprint();
        graph
    }

    pub(super) fn compare(&self, other: &Self) -> BehaviorComparison {
        let label_score = multiset_dice(&self.label_counts, &other.label_counts);
        let edge_score = multiset_dice(&self.edge_counts, &other.edge_counts);
        let wl_score = multiset_dice(&self.wl_fingerprint, &other.wl_fingerprint);
        let shared_labels = self
            .label_counts
            .keys()
            .filter(|label| other.label_counts.contains_key(*label))
            .take(12)
            .cloned()
            .collect();
        BehaviorComparison {
            score: 0.25 * label_score + 0.15 * edge_score + 0.60 * wl_score,
            shared_labels,
        }
    }

    pub(super) fn may_match(&self, other: &Self) -> bool {
        if self.signature != other.signature {
            return false;
        }
        let smallest = self.nodes.len().min(other.nodes.len());
        let largest = self.nodes.len().max(other.nodes.len());
        smallest * 2 >= largest && multiset_dice(&self.label_counts, &other.label_counts) >= 0.50
    }

    pub(super) fn bucket_key(&self) -> &str {
        &self.signature
    }

    pub(super) fn is_substantive(&self) -> bool {
        self.nodes.len() >= 6
    }

    fn compute_wl_fingerprint(&self) -> BTreeMap<String, usize> {
        let mut labels = self
            .nodes
            .iter()
            .map(|node| stable_hash(&node.label))
            .collect::<Vec<_>>();
        let mut fingerprint = BTreeMap::new();
        add_hashes(&mut fingerprint, 0, &labels);
        for round in 1..=WL_ROUNDS {
            let previous = labels.clone();
            labels = (0..self.nodes.len())
                .map(|node| {
                    let mut neighborhood = self
                        .edges
                        .iter()
                        .filter_map(|edge| {
                            if edge.from == node {
                                Some(format!("out:{}:{}", edge.label, previous[edge.to]))
                            } else if edge.to == node {
                                Some(format!("in:{}:{}", edge.label, previous[edge.from]))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    neighborhood.sort();
                    stable_hash(&format!("{}|{}", previous[node], neighborhood.join("|")))
                })
                .collect();
            add_hashes(&mut fingerprint, round, &labels);
        }
        fingerprint
    }
}

struct GraphBuilder {
    graph: BehaviorGraph,
    parents: Vec<usize>,
}

impl GraphBuilder {
    fn enter(&mut self, label: String, edge: &'static str) {
        let node = self.graph.nodes.len();
        self.graph.nodes.push(Node { label });
        let parent = *self.parents.last().unwrap_or(&0);
        self.graph.edges.push(Edge {
            from: parent,
            to: node,
            label: edge,
        });
        self.parents.push(node);
    }

    fn leave(&mut self) {
        self.parents.pop();
    }
}

impl<'ast> Visit<'ast> for GraphBuilder {
    fn visit_stmt(&mut self, statement: &'ast Stmt) {
        let label = match statement {
            Stmt::Local(_) => Some("statement:let"),
            Stmt::Item(_) => Some("statement:item"),
            Stmt::Expr(_, Some(_)) => Some("statement:semicolon"),
            Stmt::Expr(_, None) => Some("statement:tail"),
            Stmt::Macro(_) => Some("statement:macro"),
        };
        if let Some(label) = label {
            self.enter(label.to_string(), "contains");
            visit::visit_stmt(self, statement);
            self.leave();
        } else {
            visit::visit_stmt(self, statement);
        }
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Some(label) = expression_label(expression) {
            self.enter(label, expression_edge(expression));
            visit::visit_expr(self, expression);
            self.leave();
        } else {
            visit::visit_expr(self, expression);
        }
    }
}

fn signature_label(signature: &Signature, generics: &BTreeMap<String, usize>) -> String {
    let receiver = signature.inputs.iter().find_map(|argument| match argument {
        FnArg::Receiver(receiver) => Some(match &receiver.kind {
            ReceiverKind::Reference(_, _, Some(_)) => "ref-mut",
            ReceiverKind::Reference(_, _, None) => "ref",
            ReceiverKind::Value if receiver.mutability.is_some() => "value-mut",
            ReceiverKind::Value => "value",
            ReceiverKind::Typed(_, _) => "typed",
            _ => "other",
        }),
        FnArg::Typed(_) => None,
    });
    let arguments = signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Receiver(_) => None,
            FnArg::Typed(argument) => Some(type_label(&argument.ty, generics)),
        })
        .collect::<Vec<_>>();
    let output = match &signature.output {
        syn::ReturnType::Default => "unit".to_string(),
        syn::ReturnType::Type(_, ty) => type_label(ty, generics),
    };
    format!(
        "function:receiver={}:args=[{}]:async={}:unsafe={}:returns={output}",
        receiver.unwrap_or("none"),
        arguments.join(","),
        signature.asyncness.is_some(),
        matches!(signature.safety, Safety::Unsafe(_)),
    )
}

fn expression_label(expression: &Expr) -> Option<String> {
    let label = match expression {
        Expr::Array(_) => "construct:array".to_string(),
        Expr::Assign(_) => "effect:assign".to_string(),
        Expr::Async(_) => "control:async".to_string(),
        Expr::Await(_) => "control:await".to_string(),
        Expr::Binary(_) => "operation:binary".to_string(),
        Expr::Block(_) => "control:block".to_string(),
        Expr::Break(_) => "control:break".to_string(),
        Expr::Call(call) => format!("call:{}", call_name(&call.func)),
        Expr::Cast(_) => "operation:cast".to_string(),
        Expr::Closure(_) => "control:closure".to_string(),
        Expr::Const(_) => "control:const".to_string(),
        Expr::Continue(_) => "control:continue".to_string(),
        Expr::Field(field) => {
            if matches!(&*field.base, Expr::Path(path) if path.path.is_ident("self")) {
                "state:self-field".to_string()
            } else {
                "state:field".to_string()
            }
        }
        Expr::ForLoop(_) => "control:for".to_string(),
        Expr::If(_) => "control:if".to_string(),
        Expr::Index(_) => "operation:index".to_string(),
        Expr::Let(_) => "operation:let-pattern".to_string(),
        Expr::Lit(literal) => format!("literal:{}", literal_label(&literal.lit)),
        Expr::Loop(_) => "control:loop".to_string(),
        Expr::Macro(expression) => {
            let name = expression.mac.path.segments.last().map_or_else(
                || "unknown".to_string(),
                |segment| segment.ident.to_string(),
            );
            let semantics = macro_semantics(&expression.mac.tokens);
            format!("macro:{name}:{semantics}")
        }
        Expr::Match(_) => "control:match".to_string(),
        Expr::MethodCall(call) => format!("method:{}", method_family(&call.method.to_string())),
        Expr::Paren(_) | Expr::Group(_) => return None,
        Expr::Path(path) if path.path.is_ident("self") => "value:self".to_string(),
        Expr::Path(path) => path.path.segments.last().map_or_else(
            || "value:path".to_string(),
            |segment| {
                let name = segment.ident.to_string();
                if path.path.segments.len() > 1
                    || name
                        .chars()
                        .next()
                        .is_some_and(|character| character.is_ascii_uppercase())
                {
                    format!("value:symbol:{name}")
                } else {
                    "value:local".to_string()
                }
            },
        ),
        Expr::Range(_) => "construct:range".to_string(),
        Expr::RawAddr(_) | Expr::Reference(_) => "ownership:borrow".to_string(),
        Expr::Repeat(_) => "construct:repeat".to_string(),
        Expr::Return(_) => "control:return".to_string(),
        Expr::Struct(expression) => expression.path.segments.last().map_or_else(
            || "construct:struct".to_string(),
            |segment| format!("construct:{}", segment.ident),
        ),
        Expr::Try(_) => "control:try".to_string(),
        Expr::TryBlock(_) => "control:try-block".to_string(),
        Expr::Tuple(_) => "construct:tuple".to_string(),
        Expr::Unary(_) => "operation:unary".to_string(),
        Expr::Unsafe(_) => "control:unsafe".to_string(),
        Expr::While(_) => "control:while".to_string(),
        Expr::Yield(_) => "control:yield".to_string(),
        Expr::Verbatim(_) => "syntax:verbatim".to_string(),
        _ => "expression:other".to_string(),
    };
    Some(label)
}

fn expression_edge(expression: &Expr) -> &'static str {
    match expression {
        Expr::Assign(_) => "writes",
        Expr::Await(_) | Expr::Call(_) | Expr::MethodCall(_) => "invokes",
        Expr::Break(_)
        | Expr::Continue(_)
        | Expr::ForLoop(_)
        | Expr::If(_)
        | Expr::Loop(_)
        | Expr::Match(_)
        | Expr::Return(_)
        | Expr::Try(_)
        | Expr::While(_) => "controls",
        Expr::Field(_) => "reads",
        _ => "contains",
    }
}

fn call_name(expression: &Expr) -> String {
    match expression {
        Expr::Path(path) => path.path.segments.last().map_or_else(
            || "path".to_string(),
            |segment| {
                let name = segment.ident.to_string();
                if name.chars().next().is_some_and(char::is_uppercase) {
                    format!("constructor:{name}")
                } else {
                    name
                }
            },
        ),
        _ => "indirect".to_string(),
    }
}

fn type_label(ty: &Type, generics: &BTreeMap<String, usize>) -> String {
    match ty {
        Type::Array(array) => format!("array<{}>", type_label(&array.elem, generics)),
        Type::FnPtr(_) => "fn".to_string(),
        Type::Group(group) => type_label(&group.elem, generics),
        Type::ImplTrait(implementation) => format!(
            "impl:{}",
            implementation
                .bounds
                .iter()
                .filter_map(|bound| match bound {
                    syn::TypeParamBound::Trait(bound) => bound
                        .path
                        .segments
                        .last()
                        .map(|segment| segment.ident.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("+")
        ),
        Type::Infer(_) => "_".to_string(),
        Type::Macro(_) => "macro".to_string(),
        Type::Never(_) => "never".to_string(),
        Type::Paren(parenthesized) => type_label(&parenthesized.elem, generics),
        Type::Path(path) => {
            let Some(segment) = path.path.segments.last() else {
                return "path".to_string();
            };
            let name = segment.ident.to_string();
            if path.qself.is_none()
                && path.path.segments.len() == 1
                && let Some(index) = generics.get(&name)
            {
                return format!("G{index}");
            }
            let arguments = match &segment.arguments {
                PathArguments::AngleBracketed(arguments) => arguments
                    .args
                    .iter()
                    .filter_map(|argument| match argument {
                        GenericArgument::Type(ty) => Some(type_label(ty, generics)),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                PathArguments::None | PathArguments::Parenthesized(_) => Vec::new(),
            };
            if arguments.is_empty() {
                name
            } else {
                format!("{name}<{}>", arguments.join(","))
            }
        }
        Type::Ptr(pointer) => format!("pointer<{}>", type_label(&pointer.elem, generics)),
        Type::Reference(reference) => format!(
            "ref{}<{}>",
            if reference.mutability.is_some() {
                "-mut"
            } else {
                ""
            },
            type_label(&reference.elem, generics)
        ),
        Type::Slice(slice) => format!("slice<{}>", type_label(&slice.elem, generics)),
        Type::TraitObject(_) => "dyn".to_string(),
        Type::Tuple(tuple) => format!(
            "tuple<{}>",
            tuple
                .elems
                .iter()
                .map(|element| type_label(element, generics))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Type::Verbatim(_) => "verbatim".to_string(),
        _ => "other".to_string(),
    }
}

fn macro_semantics(tokens: &TokenStream) -> String {
    let mut semantics = Vec::new();
    collect_macro_semantics(tokens.clone(), &mut semantics);
    semantics.sort();
    semantics.dedup();
    if semantics.is_empty() {
        "opaque".to_string()
    } else {
        semantics.join("+")
    }
}

fn collect_macro_semantics(tokens: TokenStream, semantics: &mut Vec<String>) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => collect_macro_semantics(group.stream(), semantics),
            TokenTree::Ident(identifier) => {
                let identifier = identifier.to_string();
                if matches!(identifier.as_str(), "true" | "false")
                    || identifier
                        .chars()
                        .next()
                        .is_some_and(|character| character.is_ascii_uppercase())
                {
                    semantics.push(identifier);
                }
            }
            TokenTree::Literal(literal) => {
                let value = literal.to_string();
                if matches!(value.as_str(), "true" | "false") {
                    semantics.push(value);
                }
            }
            TokenTree::Punct(_) => {}
        }
    }
}

fn method_family(method: &str) -> &str {
    match method {
        "push" | "push_back" | "push_front" | "insert" | "extend" => "collection-add",
        "get" | "get_mut" | "first" | "last" | "contains" | "find" => "collection-read",
        "remove" | "pop" | "pop_back" | "pop_front" | "retain" => "collection-remove",
        "iter" | "iter_mut" | "into_iter" => "iterate",
        "clone" | "to_owned" => "clone",
        "lock" | "read" | "write" => "synchronize",
        "send" | "recv" | "try_send" | "try_recv" => "channel",
        _ => method,
    }
}

fn literal_label(literal: &Lit) -> String {
    match literal {
        Lit::Bool(value) => format!("bool:{}", value.value),
        Lit::Byte(value) => format!("byte:{}", value.value()),
        Lit::ByteStr(value) => {
            format!(
                "bytes:{:016x}",
                stable_hash(&format!("{:?}", value.value()))
            )
        }
        Lit::CStr(value) => {
            format!("cstr:{:016x}", stable_hash(&format!("{:?}", value.value())))
        }
        Lit::Char(value) => format!("char:{}", value.value()),
        Lit::Float(value) => format!("float:{}", value.base10_digits()),
        Lit::Int(value) => format!("int:{}", value.base10_digits()),
        Lit::Str(value) => format!("text:{:016x}", stable_hash(&value.value())),
        Lit::Verbatim(_) => "verbatim".to_string(),
        _ => "other".to_string(),
    }
}

fn label_counts<'a>(labels: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for label in labels {
        *counts.entry(label.to_string()).or_default() += 1;
    }
    counts
}

fn multiset_dice(left: &BTreeMap<String, usize>, right: &BTreeMap<String, usize>) -> f64 {
    let left_total = left.values().sum::<usize>();
    let right_total = right.values().sum::<usize>();
    if left_total + right_total == 0 {
        return 1.0;
    }
    let common = left
        .iter()
        .map(|(key, left_count)| {
            right
                .get(key)
                .map_or(0, |right_count| (*left_count).min(*right_count))
        })
        .sum::<usize>();
    2.0 * count_f64(common) / count_f64(left_total + right_total)
}

fn count_f64(value: usize) -> f64 {
    u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

fn add_hashes(fingerprint: &mut BTreeMap<String, usize>, round: usize, hashes: &[u64]) {
    for hash in hashes {
        *fingerprint
            .entry(format!("{round}:{hash:016x}"))
            .or_default() += 1;
    }
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_generic_names_do_not_change_behavior_fingerprint() {
        let left: syn::ItemFn = syn::parse_quote! {
            fn first<T: Clone>(items: &[T]) -> Option<T> {
                let selected = items.first()?;
                Some(selected.clone())
            }
        };
        let right: syn::ItemFn = syn::parse_quote! {
            fn head<U: Clone>(values: &[U]) -> Option<U> {
                let value = values.first()?;
                Some(value.clone())
            }
        };

        let comparison = BehaviorGraph::from_function(&left.sig, &left.block)
            .compare(&BehaviorGraph::from_function(&right.sig, &right.block));

        assert_eq!(comparison.score, 1.0);
        assert!(
            comparison
                .shared_labels
                .contains(&"control:try".to_string())
        );
    }
}
