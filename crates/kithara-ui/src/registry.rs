use std::collections::BTreeMap;

use derive_more::Display;

use crate::ids::EndpointId;

#[derive(Clone, Copy, Debug, Display, Eq, PartialEq)]
#[non_exhaustive]
pub enum ValueKind {
    Trigger,
    Bool,
    Scalar,
    /// A point in the box of whatever reads it, in points from its corner.
    Point,
    Color,
    Stereo,
    Text,
    Image,
    Record,
    Waveform,
    PortalMap,
    Range,
    /// A position in a list the control offers, counted from its first item.
    Index,
    Table,
    Tree,
}

#[derive(Clone, Copy, Debug, Display, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum EndpointCategory {
    Command,
    Parameter,
    Telemetry,
    Model,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EndpointDesc {
    pub value: ValueKind,
    pub scopes: Vec<String>,
}

impl EndpointDesc {
    #[must_use]
    pub const fn new(value: ValueKind) -> Self {
        Self {
            value,
            scopes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_scope(mut self, name: &str) -> Self {
        self.scopes.push(name.to_owned());
        self
    }
}

#[derive(Default)]
pub struct MapEndpoints {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MapEndpoints {
    pub fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for MapEndpoints {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub trait EndpointRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc>;
}

/// The endpoint a document binds to when it wants the host's own time.
///
/// The name and the declaration live beside the rest of the endpoint vocabulary
/// rather than beside the host that answers them: a document is compiled, and
/// its bindings validated, by builds that never draw anything.
pub const SECONDS: &str = "ui.clock.seconds";

/// What that endpoint answers with, declared once so a document may bind to it
/// without every application having to register it.
static SECONDS_DESC: EndpointDesc = EndpointDesc::new(ValueKind::Scalar);

/// Declares the endpoints a host answers for itself, over whatever the
/// application declares.
pub struct BuiltinEndpoints<'a>(&'a dyn EndpointRegistry);

impl<'a> BuiltinEndpoints<'a> {
    #[must_use]
    pub const fn new(app: &'a dyn EndpointRegistry) -> Self {
        Self(app)
    }
}

impl EndpointRegistry for BuiltinEndpoints<'_> {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        if category == EndpointCategory::Model && id.0 == SECONDS {
            return Some(&SECONDS_DESC);
        }
        self.0.endpoint(category, id)
    }
}
