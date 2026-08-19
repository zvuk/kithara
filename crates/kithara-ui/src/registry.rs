use derive_more::Display;

use crate::ids::EndpointId;

#[derive(Clone, Copy, Debug, Display, Eq, PartialEq)]
#[non_exhaustive]
pub enum ValueKind {
    Trigger,
    Bool,
    Scalar,
    Stereo,
    Text,
    Waveform,
    PortalMap,
    Range,
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
