use std::collections::BTreeMap;

use serde::Serialize;
use syn::{GenericArgument, PathArguments, Type, TypeParamBound};

use super::{
    Direction, Substitution,
    catalog::{Relation, TypeCatalog},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ShapeId(usize);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    Array(ShapeId),
    Generic(usize),
    Never,
    Path {
        name: String,
        arguments: Vec<ShapeId>,
    },
    Pointer(ShapeId),
    Reference(ShapeId),
    Slice(ShapeId),
    Trait(Vec<String>),
    Tuple(Vec<ShapeId>),
    Unknown(&'static str),
}

#[derive(Debug, Default)]
pub(super) struct ShapeArena {
    shapes: Vec<Shape>,
    interned: BTreeMap<Shape, ShapeId>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct TypeSubstitution {
    pub(super) left: String,
    pub(super) right: String,
    pub(super) substitution: Substitution,
    pub(super) direction: Direction,
    pub(super) caveats: Vec<String>,
    pub(super) source: String,
}

#[derive(Debug, Clone)]
pub(super) struct ShapeComparison {
    pub(super) score: f64,
    pub(super) substitutions: Vec<TypeSubstitution>,
}

#[derive(Debug, Default)]
pub(super) struct ComparisonCache {
    comparisons: BTreeMap<(ShapeId, ShapeId), ShapeComparison>,
}

impl ShapeArena {
    pub(super) fn len(&self) -> usize {
        self.shapes.len()
    }

    pub(super) fn intern(
        &mut self,
        ty: &Type,
        generic_parameters: &BTreeMap<String, usize>,
    ) -> ShapeId {
        let shape = match ty {
            Type::Array(array) => Shape::Array(self.intern(&array.elem, generic_parameters)),
            Type::FnPtr(_) => Shape::Unknown("function"),
            Type::Group(group) => return self.intern(&group.elem, generic_parameters),
            Type::ImplTrait(implementation) => Shape::Trait(
                implementation
                    .bounds
                    .iter()
                    .filter_map(bound_name)
                    .collect(),
            ),
            Type::Infer(_) => Shape::Unknown("infer"),
            Type::Macro(_) => Shape::Unknown("macro"),
            Type::Never(_) => Shape::Never,
            Type::Paren(parenthesized) => {
                return self.intern(&parenthesized.elem, generic_parameters);
            }
            Type::Path(path) => {
                let Some(segment) = path.path.segments.last() else {
                    return self.intern_shape(Shape::Unknown("path"));
                };
                let name = segment.ident.to_string();
                if path.qself.is_none()
                    && path.path.segments.len() == 1
                    && let Some(index) = generic_parameters.get(&name)
                {
                    return self.intern_shape(Shape::Generic(*index));
                }
                Shape::Path {
                    name,
                    arguments: path_arguments(&segment.arguments)
                        .map(|argument| self.intern(argument, generic_parameters))
                        .collect(),
                }
            }
            Type::Ptr(pointer) => Shape::Pointer(self.intern(&pointer.elem, generic_parameters)),
            Type::Reference(reference) => {
                Shape::Reference(self.intern(&reference.elem, generic_parameters))
            }
            Type::Slice(slice) => Shape::Slice(self.intern(&slice.elem, generic_parameters)),
            Type::TraitObject(object) => {
                Shape::Trait(object.bounds.iter().filter_map(bound_name).collect())
            }
            Type::Tuple(tuple) => Shape::Tuple(
                tuple
                    .elems
                    .iter()
                    .map(|element| self.intern(element, generic_parameters))
                    .collect(),
            ),
            Type::Verbatim(_) => Shape::Unknown("verbatim"),
            _ => Shape::Unknown("other"),
        };
        self.intern_shape(shape)
    }

    #[must_use]
    pub(super) fn is_primitive(&self, shape: ShapeId) -> bool {
        match &self.shapes[shape.0] {
            Shape::Never => true,
            Shape::Path { name, arguments } => {
                arguments.is_empty()
                    && matches!(
                        name.as_str(),
                        "bool"
                            | "char"
                            | "f32"
                            | "f64"
                            | "i8"
                            | "i16"
                            | "i32"
                            | "i64"
                            | "i128"
                            | "isize"
                            | "str"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "u128"
                            | "usize"
                    )
            }
            _ => false,
        }
    }

    #[must_use]
    pub(super) fn is_ambiguous_scalar(&self, shape: ShapeId) -> bool {
        if self.is_primitive(shape) {
            return true;
        }
        match &self.shapes[shape.0] {
            Shape::Generic(_) => true,
            Shape::Path { name, arguments }
                if matches!(
                    name.as_str(),
                    "Option"
                        | "NonZeroI8"
                        | "NonZeroI16"
                        | "NonZeroI32"
                        | "NonZeroI64"
                        | "NonZeroI128"
                        | "NonZeroIsize"
                        | "NonZeroU8"
                        | "NonZeroU16"
                        | "NonZeroU32"
                        | "NonZeroU64"
                        | "NonZeroU128"
                        | "NonZeroUsize"
                ) =>
            {
                !arguments.is_empty()
                    && arguments
                        .iter()
                        .all(|argument| self.is_ambiguous_scalar(*argument))
            }
            _ => false,
        }
    }

    pub(super) fn bucket_key(&self, shape: ShapeId, catalog: &TypeCatalog) -> String {
        match &self.shapes[shape.0] {
            Shape::Array(element) => format!("array<{}>", self.bucket_key(*element, catalog)),
            Shape::Generic(index) => format!("generic:{index}"),
            Shape::Never => "never".to_string(),
            Shape::Path { name, arguments } => {
                let arguments = self
                    .container_arguments(name, arguments)
                    .iter()
                    .map(|argument| self.bucket_key(*argument, catalog))
                    .collect::<Vec<_>>();
                if arguments.is_empty() {
                    catalog.bucket(name)
                } else {
                    format!("{}<{}>", catalog.bucket(name), arguments.join(","))
                }
            }
            Shape::Pointer(element) => format!("pointer<{}>", self.bucket_key(*element, catalog)),
            Shape::Reference(element) => {
                format!("reference<{}>", self.bucket_key(*element, catalog))
            }
            Shape::Slice(element) => format!("slice<{}>", self.bucket_key(*element, catalog)),
            Shape::Trait(bounds) => format!("trait:{}", bounds.join("+")),
            Shape::Tuple(elements) => format!(
                "tuple<{}>",
                elements
                    .iter()
                    .map(|element| self.bucket_key(*element, catalog))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Shape::Unknown(kind) => format!("unknown:{kind}"),
        }
    }

    pub(super) fn compare(
        &self,
        left: ShapeId,
        right: ShapeId,
        catalog: &TypeCatalog,
        cache: &mut ComparisonCache,
    ) -> ShapeComparison {
        if let Some(comparison) = cache.comparisons.get(&(left, right)) {
            return comparison.clone();
        }
        let comparison = if left == right {
            ShapeComparison {
                score: 1.0,
                substitutions: Vec::new(),
            }
        } else {
            self.compare_uncached(left, right, catalog, cache)
        };
        cache.comparisons.insert((left, right), comparison.clone());
        comparison
    }

    fn compare_uncached(
        &self,
        left: ShapeId,
        right: ShapeId,
        catalog: &TypeCatalog,
        cache: &mut ComparisonCache,
    ) -> ShapeComparison {
        match (&self.shapes[left.0], &self.shapes[right.0]) {
            (
                Shape::Path {
                    name: left_name,
                    arguments: left_arguments,
                },
                Shape::Path {
                    name: right_name,
                    arguments: right_arguments,
                },
            ) => self.compare_paths(
                left_name,
                left_arguments,
                right_name,
                right_arguments,
                catalog,
                cache,
            ),
            (Shape::Array(left), Shape::Array(right))
            | (Shape::Pointer(left), Shape::Pointer(right))
            | (Shape::Reference(left), Shape::Reference(right))
            | (Shape::Slice(left), Shape::Slice(right)) => {
                self.compare(*left, *right, catalog, cache)
            }
            (Shape::Tuple(left), Shape::Tuple(right)) if left.len() == right.len() => {
                self.compare_children(left, right, catalog, cache)
            }
            (Shape::Trait(left), Shape::Trait(right)) => ShapeComparison {
                score: set_similarity(left, right),
                substitutions: Vec::new(),
            },
            _ => ShapeComparison {
                score: 0.0,
                substitutions: Vec::new(),
            },
        }
    }

    fn compare_paths(
        &self,
        left_name: &str,
        left_arguments: &[ShapeId],
        right_name: &str,
        right_arguments: &[ShapeId],
        catalog: &TypeCatalog,
        cache: &mut ComparisonCache,
    ) -> ShapeComparison {
        let Some(relation) = catalog.relation(left_name, right_name) else {
            return ShapeComparison {
                score: 0.0,
                substitutions: Vec::new(),
            };
        };
        let left_arguments = self.container_arguments(left_name, left_arguments);
        let right_arguments = self.container_arguments(right_name, right_arguments);
        let arguments = if left_arguments.len() == right_arguments.len() {
            self.compare_children(&left_arguments, &right_arguments, catalog, cache)
        } else {
            ShapeComparison {
                score: 0.0,
                substitutions: Vec::new(),
            }
        };
        let argument_factor = if left_arguments.is_empty() && right_arguments.is_empty() {
            1.0
        } else {
            0.45 + 0.55 * arguments.score
        };
        let mut substitutions = substitution(left_name, right_name, &relation);
        substitutions.extend(arguments.substitutions);
        ShapeComparison {
            score: relation.similarity * argument_factor,
            substitutions,
        }
    }

    fn container_arguments(&self, name: &str, arguments: &[ShapeId]) -> Vec<ShapeId> {
        if matches!(name, "SmallVec" | "ArrayVec")
            && let [argument] = arguments
            && let Shape::Array(element) = &self.shapes[argument.0]
        {
            return vec![*element];
        }
        arguments.to_vec()
    }

    fn compare_children(
        &self,
        left: &[ShapeId],
        right: &[ShapeId],
        catalog: &TypeCatalog,
        cache: &mut ComparisonCache,
    ) -> ShapeComparison {
        if left.is_empty() {
            return ShapeComparison {
                score: 1.0,
                substitutions: Vec::new(),
            };
        }
        let mut score = 0.0;
        let mut substitutions = Vec::new();
        for (left, right) in left.iter().zip(right) {
            let comparison = self.compare(*left, *right, catalog, cache);
            score += comparison.score;
            substitutions.extend(comparison.substitutions);
        }
        ShapeComparison {
            score: score / count_f64(left.len()),
            substitutions,
        }
    }

    fn intern_shape(&mut self, shape: Shape) -> ShapeId {
        if let Some(id) = self.interned.get(&shape) {
            return *id;
        }
        let id = ShapeId(self.shapes.len());
        self.shapes.push(shape.clone());
        self.interned.insert(shape, id);
        id
    }
}

impl ComparisonCache {
    pub(super) fn len(&self) -> usize {
        self.comparisons.len()
    }
}

fn substitution(left: &str, right: &str, relation: &Relation) -> Vec<TypeSubstitution> {
    if left == right {
        return Vec::new();
    }
    vec![TypeSubstitution {
        left: left.to_string(),
        right: right.to_string(),
        substitution: relation.substitution,
        direction: relation.direction,
        caveats: relation.caveats.clone(),
        source: relation.source.clone(),
    }]
}

fn set_similarity(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let common = left.iter().filter(|item| right.contains(item)).count();
    2.0 * count_f64(common) / count_f64(left.len() + right.len())
}

fn count_f64(value: usize) -> f64 {
    u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

fn path_arguments(arguments: &PathArguments) -> impl Iterator<Item = &Type> {
    match arguments {
        PathArguments::AngleBracketed(arguments) => arguments
            .args
            .iter()
            .filter_map(|argument| match argument {
                GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter(),
        PathArguments::None | PathArguments::Parenthesized(_) => Vec::new().into_iter(),
    }
}

fn bound_name(bound: &TypeParamBound) -> Option<String> {
    match bound {
        TypeParamBound::Trait(trait_bound) => trait_bound
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::similarity::SimilarityConfig;

    #[test]
    fn nested_shapes_and_pair_comparisons_are_reused() {
        let config: SimilarityConfig = toml::from_str(
            r#"
                [[types.relations]]
                left = "List"
                right = "Vec"
                similarity = 0.84
            "#,
        )
        .expect("similarity config");
        let catalog = TypeCatalog::from_config(&config).expect("type catalog");
        let generics = BTreeMap::new();
        let mut arena = ShapeArena::default();
        let vec_shape = arena.intern(
            &syn::parse_str("Vec<Option<Baz>>").expect("Vec type"),
            &generics,
        );
        let list_shape = arena.intern(
            &syn::parse_str("List<Option<Baz>>").expect("List type"),
            &generics,
        );
        let repeated_vec = arena.intern(
            &syn::parse_str("Vec<Option<Baz>>").expect("repeated Vec type"),
            &generics,
        );

        assert_eq!(vec_shape, repeated_vec);
        assert_eq!(arena.len(), 4);

        let mut cache = ComparisonCache::default();
        let first = arena.compare(vec_shape, list_shape, &catalog, &mut cache);
        let comparisons_after_first = cache.len();
        let second = arena.compare(vec_shape, list_shape, &catalog, &mut cache);

        assert_eq!(first.score, second.score);
        assert_eq!(cache.len(), comparisons_after_first);
        assert_eq!(comparisons_after_first, 2);
    }

    #[test]
    fn inline_capacity_array_is_normalized_as_container_element() {
        let config: SimilarityConfig = toml::from_str(
            r#"
                [[types.relations]]
                left = "Vec"
                right = "SmallVec"
                similarity = 0.86
                caveats = ["inline capacity"]
            "#,
        )
        .expect("similarity config");
        let catalog = TypeCatalog::from_config(&config).expect("type catalog");
        let generics = BTreeMap::new();
        let mut arena = ShapeArena::default();
        let vec_shape = arena.intern(&syn::parse_str("Vec<Baz>").expect("Vec type"), &generics);
        let small_shape = arena.intern(
            &syn::parse_str("SmallVec<[Baz; 4]>").expect("SmallVec type"),
            &generics,
        );

        let comparison = arena.compare(
            vec_shape,
            small_shape,
            &catalog,
            &mut ComparisonCache::default(),
        );

        assert!((comparison.score - 0.86).abs() < 0.001);
        assert_eq!(comparison.substitutions[0].caveats, ["inline capacity"]);
    }
}
