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
    #[error("{origin}: invalid skin color {value:?}; expected #RRGGBB or #RRGGBBAA")]
    BadColor { origin: SourceUri, value: String },
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
    #[error("{origin}: adaptive node {id:?} at {path} declares no steps")]
    AdaptiveWithoutSteps {
        origin: SourceUri,
        id: String,
        path: String,
    },
    #[error(
        "{origin}: adaptive step {index} at {path} starts at {from}; steps climb from a finite value"
    )]
    AdaptiveStepOrder {
        origin: SourceUri,
        path: String,
        index: usize,
        from: f32,
    },
    #[error("{origin}: adaptive step at {path} draws from {from} {axis} and needs {needs}")]
    AdaptiveStepRoom {
        origin: SourceUri,
        path: String,
        axis: &'static str,
        from: f32,
        needs: f32,
    },
    #[error(
        "{origin}: container at {path} stands cells needing {needs} {axis} in the {room} it has"
    )]
    RevealRoom {
        origin: SourceUri,
        path: String,
        axis: &'static str,
        needs: f32,
        room: f32,
    },
    #[error("{origin}: node at {path} declares {room} {axis} and holds content needing {needs}")]
    DeclaredRoom {
        origin: SourceUri,
        path: String,
        axis: &'static str,
        needs: f32,
        room: f32,
    },
    #[error("{origin}: {path} measures its own {axis} and must declare that axis as a box")]
    UnmeasuredAxis {
        origin: SourceUri,
        path: String,
        axis: &'static str,
    },
    #[error("{origin}: reveal at {path} has no container measuring itself to show it")]
    UnmeasuredReveal { origin: SourceUri, path: String },
    #[error(
        "{origin}: reveal at {path} appears from {from}; a threshold is finite and not negative"
    )]
    RevealThreshold {
        origin: SourceUri,
        path: String,
        from: f32,
    },
    #[error(
        "{origin}: reveal at {path} appears from {from} and stops at {until}; a band ends above the room it starts in"
    )]
    RevealBand {
        origin: SourceUri,
        path: String,
        from: f32,
        until: f32,
    },
    #[error(
        "{origin}: adaptive node {id:?} at {path} reads its measure and takes the size of the branch it draws"
    )]
    MeasuredBoxWithoutAxis {
        origin: SourceUri,
        id: String,
        path: String,
    },
    #[error("{origin}: optional block {id:?} at {path} has no parent to hide it")]
    RootBlock {
        origin: SourceUri,
        id: String,
        path: String,
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
    #[error("{origin}: {value:?} names no variant and is no ${{parameter}} at {path}")]
    BadVariant {
        origin: SourceUri,
        value: String,
        path: String,
    },
    #[error("{origin}: argument ${name} at {path} carries {value:?}, which names no variant")]
    BadParamVariant {
        origin: SourceUri,
        name: String,
        value: String,
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
    #[error("{origin}: track list at {path} must contain the Title column")]
    MissingTrackTitleColumn { origin: SourceUri, path: String },
    #[error("{origin}: ContextBar at {path} requires scope_items, scope, and write together")]
    InvalidContextScope { origin: SourceUri, path: String },
    #[error("{origin}: container at {path} declares write but has no id to address it by")]
    UnaddressedSurface { origin: SourceUri, path: String },
    #[error("{origin}: compiled node count {count} exceeds limit {max}")]
    NodesExceeded {
        origin: SourceUri,
        count: usize,
        max: usize,
    },
    #[error("{origin}: unknown text key {key:?} at {path}")]
    UnknownTextKey {
        origin: SourceUri,
        key: String,
        path: String,
    },
    #[error("{origin}: text key {key:?} is defined in more than one catalog")]
    DuplicateTextKey { origin: SourceUri, key: String },
}
