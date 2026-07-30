use serde::{Deserialize, Serialize};

/// A typed document field a template may take from an `Include` argument,
/// written either as the variant itself or as `"$name"`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Param<T> {
    Fixed(T),
    Ref(String),
}
