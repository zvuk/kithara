mod curve;
mod eq;
mod filter;

pub(crate) use curve::ParameterCurve;
pub use curve::{ParameterCurveError, ParameterKnot};
pub use eq::EqGainProgram;
pub use filter::FilterProgram;
