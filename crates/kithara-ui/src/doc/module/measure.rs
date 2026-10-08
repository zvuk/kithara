use serde::{Deserialize, Serialize};

use super::binding::BindingRef;

/// Where the number that picks a branch comes from.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum Measure {
    /// The width the node is given, in logical pixels.
    Width,
    /// The height the node is given, in logical pixels.
    Height,
    /// A scalar the host answers.
    Read(BindingRef),
}

impl Measure {
    pub(crate) const fn axis(&self) -> Option<MeasureAxis> {
        match self {
            Self::Width => Some(MeasureAxis::Width),
            Self::Height => Some(MeasureAxis::Height),
            Self::Read(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum MeasureAxis {
    Width,
    Height,
}

impl MeasureAxis {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Width => "width",
            Self::Height => "height",
        }
    }
}
