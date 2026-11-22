mod config;
mod effect;
mod parameters;

pub use config::ReverbConfig;
pub use effect::{Reverb, ReverbError};
pub use parameters::{Damping, Decay, Width};
