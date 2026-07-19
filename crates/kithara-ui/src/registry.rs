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
    TrackList,
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
    pub fn new(value: ValueKind) -> Self {
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
