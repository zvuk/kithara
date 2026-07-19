use std::collections::BTreeMap;

use derive_more::Display;

use crate::{ids::EndpointId, size::SizeSpec};

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
    pub size: SizeSpec,
}

impl ControlKindDesc {
    #[must_use]
    pub fn new(read: Option<ValueKind>, write: Option<ValueKind>) -> Self {
        Self {
            props: BTreeMap::new(),
            read,
            write,
            size: SizeSpec::FILL,
        }
    }

    #[must_use]
    pub fn with_prop(mut self, name: &str, kind: PropKind) -> Self {
        self.props.insert(name.to_owned(), kind);
        self
    }

    #[must_use]
    pub fn with_size(mut self, size: SizeSpec) -> Self {
        self.size = size;
        self
    }
}

pub trait ControlCatalog {
    fn kind(&self, kind: &str) -> Option<&ControlKindDesc>;
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn control_kind_defaults_to_fill() {
        let description = ControlKindDesc::new(None, None);

        assert_eq!(description.size, SizeSpec::FILL);
    }
}
