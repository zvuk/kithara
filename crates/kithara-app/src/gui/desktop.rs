use arc_swap::ArcSwap;
use kithara::platform::{sync::Arc, tokio::sync::mpsc};

#[cfg(feature = "masonry")]
use super::frontend::retained;
use super::frontend::{Boot, FrontendError, immediate};
use crate::{
    config::AppConfig,
    engine::{self, EngineSnapshot},
    pools::AppHost,
};

/// Which host draws the studio.
///
/// The retained host is a build the `masonry` feature turns on; without it
/// there is only one host to pick. Both read the same documents and the same
/// state, so the choice is the shell and nothing else.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum Host {
    /// iced: the tree is rebuilt from the state on every message.
    #[cfg_attr(not(feature = "masonry"), default)]
    Immediate,
    /// masonry and Vello: the tree is kept and told what changed.
    #[cfg(feature = "masonry")]
    #[default]
    Retained,
}

/// Builds the engine over `audio` on its own thread, then runs the studio in
/// `host` until the application exits.
///
/// # Errors
/// Returns an error if the studio or the engine cannot be built, or the event
/// loop fails.
pub fn run(config: AppConfig, host: Host, audio: AppHost) -> Result<(), FrontendError> {
    let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
    let (commands, receiver) = mpsc::unbounded_channel();
    let boot = Boot::builder()
        .maybe_package(config.ui_package.as_deref())
        .settings(&config.ui)
        .tracks(config.tracks.clone())
        .palette(config.palette)
        .snapshots(Arc::clone(&snapshots))
        .commands(commands)
        .build()?;
    let shutdown = config.shutdown.clone();
    let driver = engine::spawn(
        move || engine::build(config, audio, snapshots),
        receiver,
        shutdown.child(),
    )?;
    let result = match host {
        Host::Immediate => immediate(boot),
        #[cfg(feature = "masonry")]
        Host::Retained => retained(boot),
    };

    let finished = driver.finish(&shutdown);
    result?;
    finished?;
    Ok(())
}
