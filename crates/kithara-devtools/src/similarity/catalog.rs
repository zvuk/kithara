use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use super::{Direction, SimilarityConfig, Substitution, TypeRelationConfig};

#[derive(Debug, Clone)]
pub(super) struct Relation {
    pub(super) similarity: f64,
    pub(super) substitution: Substitution,
    pub(super) direction: Direction,
    pub(super) caveats: Vec<String>,
    pub(super) source: String,
    configured_left: String,
    configured_right: String,
}

#[derive(Debug)]
struct Family {
    similarity: f64,
    members: BTreeSet<String>,
    source: String,
}

#[derive(Debug, Default)]
pub(super) struct TypeCatalog {
    families: Vec<Family>,
    relations: BTreeMap<(String, String), Relation>,
    buckets: BTreeMap<String, String>,
}

impl TypeCatalog {
    pub(super) fn from_config(config: &SimilarityConfig) -> Result<Self> {
        let mut catalog = Self::default();
        catalog.add_builtin_families();
        catalog.add_builtin_relations()?;
        catalog.add_dependency_types(config);
        for (name, family) in &config.types.families {
            validate_similarity(family.default_similarity, &format!("type family `{name}`"))?;
            if family.members.len() < 2 {
                bail!("type family `{name}` requires at least two members");
            }
            catalog.families.push(Family {
                similarity: family.default_similarity,
                members: family
                    .members
                    .iter()
                    .map(|member| short_name(member))
                    .collect(),
                source: format!("config:{name}"),
            });
        }
        for relation in &config.types.relations {
            catalog.insert_relation(relation, "config")?;
        }
        catalog.rebuild_buckets();
        Ok(catalog)
    }

    #[must_use]
    pub(super) fn relation(&self, left: &str, right: &str) -> Option<Relation> {
        let left = short_name(left);
        let right = short_name(right);
        if left == right {
            return Some(Relation {
                similarity: 1.0,
                substitution: Substitution::Safe,
                direction: Direction::Bidirectional,
                caveats: Vec::new(),
                source: "identity".to_string(),
                configured_left: left,
                configured_right: right,
            });
        }
        let key = ordered_pair(&left, &right);
        if let Some(relation) = self.relations.get(&key) {
            let mut relation = relation.clone();
            if relation.configured_left != left || relation.configured_right != right {
                relation.direction = reverse(relation.direction);
            }
            return Some(relation);
        }
        self.families
            .iter()
            .filter(|family| family.members.contains(&left) && family.members.contains(&right))
            .max_by(|left, right| left.similarity.total_cmp(&right.similarity))
            .map(|family| Relation {
                similarity: family.similarity,
                substitution: Substitution::Conditional,
                direction: Direction::Bidirectional,
                caveats: vec!["container semantics differ".to_string()],
                source: family.source.clone(),
                configured_left: left.clone(),
                configured_right: right.clone(),
            })
    }

    #[must_use]
    pub(super) fn bucket(&self, name: &str) -> String {
        let name = short_name(name);
        self.buckets
            .get(&name)
            .map_or_else(|| format!("type:{name}"), Clone::clone)
    }

    fn add_builtin_families(&mut self) {
        self.families.extend([
            Family {
                similarity: 0.68,
                members: members(&["Vec", "VecDeque", "LinkedList", "BinaryHeap"]),
                source: "builtin:std.sequence".to_string(),
            },
            Family {
                similarity: 0.72,
                members: members(&["HashMap", "BTreeMap"]),
                source: "builtin:std.map".to_string(),
            },
            Family {
                similarity: 0.72,
                members: members(&["HashSet", "BTreeSet"]),
                source: "builtin:std.set".to_string(),
            },
        ]);
    }

    fn add_builtin_relations(&mut self) -> Result<()> {
        for relation in [
            TypeRelationConfig {
                left: "Arc".to_string(),
                right: "Rc".to_string(),
                similarity: 0.88,
                substitution: Substitution::Conditional,
                direction: Direction::Bidirectional,
                caveats: vec![
                    "thread-safety".to_string(),
                    "atomic reference counting".to_string(),
                ],
            },
            TypeRelationConfig {
                left: "Vec".to_string(),
                right: "VecDeque".to_string(),
                similarity: 0.82,
                substitution: Substitution::Conditional,
                direction: Direction::Bidirectional,
                caveats: vec![
                    "contiguous storage".to_string(),
                    "front insertion complexity".to_string(),
                ],
            },
            TypeRelationConfig {
                left: "HashMap".to_string(),
                right: "BTreeMap".to_string(),
                similarity: 0.80,
                substitution: Substitution::Conditional,
                direction: Direction::Bidirectional,
                caveats: vec!["ordering and complexity guarantees".to_string()],
            },
        ] {
            self.insert_relation(&relation, "builtin:std")?;
        }
        Ok(())
    }

    fn add_dependency_types(&mut self, config: &SimilarityConfig) {
        let Some(sequence) = self
            .families
            .iter_mut()
            .find(|family| family.source == "builtin:std.sequence")
        else {
            return;
        };
        if config.active_dependencies.contains("smallvec") {
            sequence.members.insert("SmallVec".to_string());
        }
        if config.active_dependencies.contains("arrayvec") {
            sequence.members.insert("ArrayVec".to_string());
        }
    }

    fn insert_relation(&mut self, relation: &TypeRelationConfig, source: &str) -> Result<()> {
        if relation.left.is_empty() || relation.right.is_empty() {
            bail!("type relation requires non-empty left and right types");
        }
        validate_similarity(
            relation.similarity,
            &format!("type relation `{} <-> {}`", relation.left, relation.right),
        )?;
        let left = short_name(&relation.left);
        let right = short_name(&relation.right);
        self.relations.insert(
            ordered_pair(&left, &right),
            Relation {
                similarity: relation.similarity,
                substitution: relation.substitution,
                direction: relation.direction,
                caveats: relation.caveats.clone(),
                source: source.to_string(),
                configured_left: left,
                configured_right: right,
            },
        );
        Ok(())
    }

    fn rebuild_buckets(&mut self) {
        let mut adjacency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for family in &self.families {
            for left in &family.members {
                for right in &family.members {
                    if left != right {
                        adjacency
                            .entry(left.clone())
                            .or_default()
                            .insert(right.clone());
                    }
                }
            }
        }
        for left_right in self.relations.keys() {
            let (left, right) = left_right;
            adjacency
                .entry(left.clone())
                .or_default()
                .insert(right.clone());
            adjacency
                .entry(right.clone())
                .or_default()
                .insert(left.clone());
        }
        self.buckets.clear();
        for start in adjacency.keys() {
            if self.buckets.contains_key(start) {
                continue;
            }
            let mut stack = vec![start.clone()];
            let mut component = BTreeSet::new();
            while let Some(current) = stack.pop() {
                if !component.insert(current.clone()) {
                    continue;
                }
                if let Some(neighbors) = adjacency.get(&current) {
                    stack.extend(neighbors.iter().cloned());
                }
            }
            let Some(canonical) = component.first() else {
                continue;
            };
            let bucket = format!("group:{canonical}");
            for member in component {
                self.buckets.insert(member, bucket.clone());
            }
        }
    }
}

fn reverse(direction: Direction) -> Direction {
    match direction {
        Direction::Bidirectional => Direction::Bidirectional,
        Direction::LeftToRight => Direction::RightToLeft,
        Direction::RightToLeft => Direction::LeftToRight,
    }
}

fn validate_similarity(value: f64, owner: &str) -> Result<()> {
    if !(0.0..=1.0).contains(&value) {
        bail!("{owner} similarity must be between 0.0 and 1.0");
    }
    Ok(())
}

fn short_name(name: &str) -> String {
    name.rsplit("::").next().unwrap_or(name).to_string()
}

fn ordered_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_string(), right.to_string())
    } else {
        (right.to_string(), left.to_string())
    }
}

fn members(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_relation_follows_compared_type_order() {
        let config: SimilarityConfig = toml::from_str(
            r#"
                [[types.relations]]
                left = "Arc"
                right = "Rc"
                similarity = 0.8
                direction = "left-to-right"
            "#,
        )
        .expect("similarity config");
        let catalog = TypeCatalog::from_config(&config).expect("catalog");

        assert!(matches!(
            catalog
                .relation("Arc", "Rc")
                .map(|relation| relation.direction),
            Some(Direction::LeftToRight)
        ));
        assert!(matches!(
            catalog
                .relation("Rc", "Arc")
                .map(|relation| relation.direction),
            Some(Direction::RightToLeft)
        ));
    }
}
