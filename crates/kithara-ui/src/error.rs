use thiserror::Error;

use crate::ids::SourceUri;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UiDocError {
    #[error("{origin}: RON syntax error: {source}")]
    Syntax {
        origin: SourceUri,
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    #[error("{origin}: unknown schema {schema:?}")]
    UnknownSchema { origin: SourceUri, schema: String },
    #[error("{origin}: unsupported {schema} version {version}, max supported {max}")]
    UnsupportedVersion {
        origin: SourceUri,
        schema: String,
        version: u32,
        max: u32,
    },
    #[error("{origin}: expected a {expected} document, found {found}")]
    WrongDocKind {
        origin: SourceUri,
        expected: &'static str,
        found: &'static str,
    },
    #[error("{origin}: duplicate id {id:?} at {path}")]
    DuplicateId {
        origin: SourceUri,
        id: String,
        path: String,
    },
    #[error("{origin}: invalid id {id:?}: {reason}")]
    InvalidId {
        origin: SourceUri,
        id: String,
        reason: String,
    },
    #[error("{origin}: invalid split weight {value} at {path}")]
    InvalidWeight {
        origin: SourceUri,
        path: String,
        value: String,
    },
    #[error("{origin}: source is {bytes} bytes, exceeds limit {max}")]
    TooLarge {
        origin: SourceUri,
        bytes: usize,
        max: usize,
    },
    #[error("{origin}: string arena budget exceeded (max {max} bytes)")]
    ArenaFull { origin: SourceUri, max: usize },
    #[error("{origin}: source not found: {rel:?}")]
    NotFound { origin: SourceUri, rel: String },
    #[error("{origin}: source {rel:?} escapes configured root")]
    RootEscape { origin: SourceUri, rel: String },
    #[error(
        "include cycle: {}",
        chain
            .iter()
            .map(|uri| uri.0.as_str())
            .collect::<Vec<_>>()
            .join(" -> ")
    )]
    IncludeCycle { chain: Vec<SourceUri> },
    #[error("{origin}: include depth {depth} exceeds limit {max}")]
    DepthExceeded {
        origin: SourceUri,
        depth: usize,
        max: usize,
    },
    #[error("{origin}: unresolved parameter ${name} at {path}")]
    UnresolvedParam {
        origin: SourceUri,
        name: String,
        path: String,
    },
    #[error("{origin}: argument {name:?} is not declared in module parameters (at {path})")]
    UnknownParam {
        origin: SourceUri,
        name: String,
        path: String,
    },
    #[error("{origin}: unknown endpoint {category} {id:?} at {path}")]
    UnknownEndpoint {
        origin: SourceUri,
        category: String,
        id: String,
        path: String,
    },
    #[error("{origin}: endpoint {id:?} at {path}: missing scope arg {scope:?}")]
    MissingScope {
        origin: SourceUri,
        id: String,
        scope: String,
        path: String,
    },
    #[error("{origin}: binding {id:?} at {path}: unknown scope arg {scope:?}")]
    UnknownScope {
        origin: SourceUri,
        id: String,
        scope: String,
        path: String,
    },
    #[error(
        "{origin}: binding {id:?} at {path}: control expects {expected}, endpoint provides {got}"
    )]
    BindingType {
        origin: SourceUri,
        id: String,
        path: String,
        expected: String,
        got: String,
    },
    #[error("{origin}: binding {id:?} at {path}: {detail}")]
    BindingDirection {
        origin: SourceUri,
        id: String,
        path: String,
        detail: String,
    },
    #[error("{origin}: compiled node count {count} exceeds limit {max}")]
    NodesExceeded {
        origin: SourceUri,
        count: usize,
        max: usize,
    },
}
