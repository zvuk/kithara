use std::path::Path;

use arc_swap::ArcSwap;
use kithara::{
    platform::{
        CancelToken,
        sync::Arc,
        tokio::{runtime::Handle, sync::mpsc},
    },
    worker::Worker,
};
use kithara_app_library::Registration;

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

struct AppOwner {
    worker: Option<Worker>,
    shutdown: CancelToken,
}

impl Drop for AppOwner {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(worker) = &self.worker {
            worker.shutdown();
        }
    }
}

/// Builds the engine over `audio` on its own thread, then runs the studio in
/// `host` until the application exits.
///
/// # Errors
/// Returns an error if the studio or the engine cannot be built, or the event
/// loop fails.
pub fn run(
    config: AppConfig,
    config_path: Option<&Path>,
    plugins: Vec<Registration>,
    host: Host,
    audio: AppHost,
    runtime: &Handle,
) -> Result<(), FrontendError> {
    let owner = AppOwner {
        worker: config.base_worker.clone(),
        shutdown: config.shutdown.clone(),
    };
    let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
    let (commands, receiver) = mpsc::unbounded_channel();
    let boot = Boot::builder()
        .maybe_package(config.ui_package.as_deref())
        .maybe_config_path(config_path)
        .settings(&config.ui)
        .tracks(config.tracks.clone())
        .palette(config.palette)
        .snapshots(Arc::clone(&snapshots))
        .commands(commands)
        .runtime(runtime.clone())
        .plugins(plugins)
        .build()?;
    let shutdown = config.shutdown.clone();
    let driver = engine::spawn(
        runtime,
        move || engine::build(config, audio, snapshots),
        receiver,
        shutdown.child(),
    )?;
    let result = match host {
        Host::Immediate => immediate(boot),
        #[cfg(feature = "masonry")]
        Host::Retained => retained(boot),
    };

    if result.is_err() {
        owner.shutdown.cancel();
    }
    let finished = driver.finish();
    result?;
    finished?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ::kithara::{
        platform::{
            CancelScope,
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        },
        worker::{DispatcherConfig, Event, Observer, TaskConfig, TaskError, WorkerConfig},
    };
    use kithara_test_utils::kithara;

    use super::{AppOwner, Worker};

    struct HeldCleanup {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Observer for HeldCleanup {
        fn on_event(&mut self, _: Event) {}
    }

    impl Drop for HeldCleanup {
        fn drop(&mut self) {
            self.entered.send(()).ok();
            self.release.recv().ok();
        }
    }

    #[kithara::test(native, flash(false))]
    #[case::normal(false)]
    #[case::error(true)]
    fn the_app_owner_joins_shared_worker_cleanup_on_every_return(#[case] fail: bool) {
        let scope = CancelScope::new(None);
        let shutdown = scope.token();
        let worker = Worker::new(WorkerConfig::new().with_cancel(shutdown.child()));
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("app-owner-cleanup")
                .observer(HeldCleanup {
                    entered,
                    release: release_rx,
                })
                .build(),
        );
        let owner = AppOwner {
            worker: Some(worker.clone()),
            shutdown: shutdown.clone(),
        };
        let (returned, returned_rx) = mpsc::channel();
        let closer = thread::spawn(move || {
            let run = move || {
                let _owner = owner;
                if fail { Err(()) } else { Ok(()) }
            };
            returned.send(run()).expect("application returned");
        });

        let entered = entered_rx.recv_timeout(Instant::now() + Duration::from_secs(2));
        let premature = returned_rx.recv_timeout(Instant::now() + Duration::from_millis(50));
        drop(release);
        closer.join().expect("application owner exits");

        entered.expect("dispatcher cleanup started");
        assert!(
            premature.is_err(),
            "application return must wait for its worker"
        );
        assert_eq!(
            returned_rx
                .try_recv()
                .expect("application returned after cleanup"),
            if fail { Err(()) } else { Ok(()) }
        );
        assert!(shutdown.is_cancelled());
        assert!(worker.is_cancelled());
        assert!(matches!(
            dispatcher.reserve(TaskConfig::new()),
            Err(TaskError::Stopped)
        ));
    }
}
