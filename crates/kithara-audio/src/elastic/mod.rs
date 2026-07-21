#[cfg(feature = "stretch-signalsmith")]
mod anchor;
mod config;
#[cfg(feature = "stretch-signalsmith")]
mod error;
mod exports;
#[cfg(feature = "stretch-signalsmith")]
mod reader;

pub use config::ElasticReaderConfig;
#[cfg(feature = "stretch-signalsmith")]
pub use exports::*;
