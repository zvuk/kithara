use kithara_config::Config;

use crate::{LinkConfig, TempoTrajectory};

/// Construction inputs for a synchronized track or track factory.
#[derive(Config)]
#[config(construction, debug)]
#[non_exhaustive]
pub struct LinkedConfig<T> {
    #[config(debug(skip))]
    pub(crate) inner: T,
    #[config(nested, builder(default))]
    pub(crate) settings: LinkConfig,
    #[config(debug(skip))]
    pub(crate) trajectory: TempoTrajectory,
}

/// Construction inputs for the synchronized session owner.
#[derive(Config)]
#[config(construction, debug)]
#[non_exhaustive]
pub struct LinkedHostConfig<H> {
    #[config(debug(skip))]
    pub(crate) inner: H,
    #[config(debug(skip))]
    pub(crate) trajectory: TempoTrajectory,
}
