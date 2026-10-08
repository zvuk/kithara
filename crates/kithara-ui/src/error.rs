use ron::error::SpannedError;
use thiserror::Error;

use crate::ids::SourceUri;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UiDocError {
    #[error("{origin}: RON syntax error: {source}")]
    Syntax {
        origin: SourceUri,
        #[source]
        source: Box<SpannedError>,
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
    #[cfg(feature = "render")]
    #[error(transparent)]
    Text(#[from] crate::shaping::TextError),
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
    #[error("{origin}: package needs ui contract {needs}, this build offers {offers}")]
    ContractMismatch {
        needs: u32,
        offers: u32,
        origin: SourceUri,
    },
    #[error("{origin}: package puts this file behind role {role:?}, it names screen {found:?}")]
    RoleMismatch {
        found: String,
        origin: SourceUri,
        role: String,
    },
    #[error("package {package:?} answers for no screen role {role:?}")]
    MissingRole { package: String, role: String },
    #[error("{origin}: package declares no screens")]
    EmptyPackage { origin: SourceUri },
    #[error("{origin}: package role {role:?} names no file")]
    RoleWithoutFile { origin: SourceUri, role: String },
    #[error("{origin}: screen declares none of these writes: {writes:?}")]
    MissingWrites {
        origin: SourceUri,
        writes: Vec<String>,
    },
    #[error("{origin}: source {rel:?} escapes configured root")]
    RootEscape { origin: SourceUri, rel: String },
    #[error("{origin}: source {rel:?} could not be read: {source}")]
    Unreadable {
        origin: SourceUri,
        rel: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{origin}: control at {path} names custom kind {kind:?}, which no application registered"
    )]
    UnknownCustomKind {
        origin: SourceUri,
        path: String,
        kind: String,
    },
    #[error("{origin}: shader at {path}: {detail}")]
    Shader {
        origin: SourceUri,
        path: String,
        detail: String,
    },
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
    #[error("fill {address:?} from {key:?} names a collection no slot of the package shows")]
    UnknownFill { address: String, key: String },
    #[error("fill {address:?} under key {key:?}: key {reason}")]
    FillKey {
        address: String,
        key: String,
        reason: &'static str,
    },
    #[error("{origin}: fill {address:?} {reason}")]
    FillOrigin {
        origin: SourceUri,
        address: String,
        reason: &'static str,
    },
    #[error("{origin}: item template has no slot \"content\" for the fill")]
    TemplateWithoutContent { origin: SourceUri },
    #[error("{origin}: slot {slot:?} shows `from` through exactly one of `each` and `select`")]
    CollectionShape { origin: SourceUri, slot: String },
    #[error("{origin}: scope {name:?} at {path} carries {value:?}, which {reason}")]
    ScopeValue {
        origin: SourceUri,
        name: String,
        value: String,
        path: String,
        reason: &'static str,
    },
    #[error("{origin}: argument {name:?} is not declared in module parameters (at {path})")]
    UnknownParam {
        origin: SourceUri,
        name: String,
        path: String,
    },
    #[error("{origin}: view state {id:?} is written at {path} and read nowhere")]
    UnreadState {
        origin: SourceUri,
        id: String,
        path: String,
    },
    #[error("{origin}: view state {id:?} is not one this screen names")]
    UnknownState { origin: SourceUri, id: String },
    #[error("{origin}: {path} names page {page:?} of view state {id:?}, which has no such page")]
    UnknownPage {
        origin: SourceUri,
        id: String,
        page: String,
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
    #[error("{origin}: ContextBar at {path} requires scope_items, scope, and write together")]
    InvalidContextScope { origin: SourceUri, path: String },
    #[error("{origin}: container at {path} declares write but has no id to address it by")]
    UnaddressedSurface { origin: SourceUri, path: String },
    #[error(
        "{origin}: Object at {path} turns or scales a {child}, which is laid out as several boxes and has no single one to turn about"
    )]
    ObjectGroup {
        origin: SourceUri,
        path: String,
        child: &'static str,
    },
    #[error(
        "{origin}: Object at {path} poses a {child}, which paints its own pass rather than a draw list"
    )]
    ObjectNative {
        origin: SourceUri,
        path: String,
        child: &'static str,
    },
    #[error(
        "{origin}: Object at {path} declares both phase and motion; a motion computes the phase, so one pose cannot carry both"
    )]
    ObjectDrivenTwice { origin: SourceUri, path: String },
    #[error(
        "{origin}: Placed at {path} is not a child of a Stage; a placement is a point inside a scene"
    )]
    PlacedOutsideStage { origin: SourceUri, path: String },
    #[error(
        "{origin}: Placed at {path} declares write without read; a placement publishes where a drag left it and reads back the point it stands on"
    )]
    PlacedUnread { origin: SourceUri, path: String },
    #[error(
        "{origin}: Placed at {path} declares a magnet without write; a magnet acts on a placement the pointer carries"
    )]
    MagnetUncarried { origin: SourceUri, path: String },
    #[error("{origin}: Placed at {path} snaps onto {target:?}, which is no placement of its stage")]
    MagnetUnknown {
        origin: SourceUri,
        path: String,
        target: String,
    },
    #[error("{origin}: Placed at {path} snaps within {within}, which no distance is under")]
    MagnetReach {
        origin: SourceUri,
        path: String,
        within: f32,
    },
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
    /// A picture the skin names, and why it could not be cut into frames.
    ///
    /// Only where there is drawing: a build that mounts documents without
    /// painting them never reads a picture.
    #[cfg(feature = "render")]
    #[error("{origin}: picture {name:?} did not cut into frames: {source}")]
    Picture {
        origin: SourceUri,
        name: String,
        #[source]
        source: Box<crate::render::SheetError>,
    },
}
