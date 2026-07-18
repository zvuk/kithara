use std::collections::BTreeMap;

use derive_more::Display;

use crate::ids::{ControlKind, EndpointId};

#[derive(Clone, Copy, Debug, Display, Eq, PartialEq)]
#[non_exhaustive]
pub enum ValueKind {
    Trigger,
    Bool,
    Scalar,
    Text,
    Waveform,
    TrackList,
}

#[derive(Clone, Copy, Debug, Display, Eq, PartialEq)]
#[non_exhaustive]
pub enum PropKind {
    Bool,
    Num,
    Text,
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
pub struct ControlKindDesc {
    pub props: BTreeMap<String, PropKind>,
    pub read: Option<ValueKind>,
    pub write: Option<ValueKind>,
}

impl ControlKindDesc {
    #[must_use]
    pub fn new(read: Option<ValueKind>, write: Option<ValueKind>) -> Self {
        Self {
            props: BTreeMap::new(),
            read,
            write,
        }
    }

    #[must_use]
    pub fn with_prop(mut self, name: &str, kind: PropKind) -> Self {
        self.props.insert(name.to_owned(), kind);
        self
    }
}

pub trait ControlCatalog {
    fn kind(&self, kind: &ControlKind) -> Option<&ControlKindDesc>;
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
