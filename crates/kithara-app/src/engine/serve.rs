use kithara::platform::{
    CancelToken,
    tokio::sync::{mpsc::UnboundedReceiver, oneshot},
};
use tracing::warn;

use super::{Engine, EngineError, Envelope, run};

/// Builds the engine, reports the outcome on `built`, and runs the loop over a
/// built engine once the root took the report.
pub(crate) async fn serve(
    build: impl FnOnce() -> Result<Engine, EngineError>,
    built: oneshot::Sender<Result<(), EngineError>>,
    commands: UnboundedReceiver<Envelope>,
    cancel: CancelToken,
) {
    let (engine, report) = match build() {
        Ok(engine) => (Some(engine), Ok(())),
        Err(error) => (None, Err(error)),
    };
    if built.send(report).is_ok()
        && let Some(engine) = engine
        && !run(engine, commands, cancel).await
    {
        warn!("the engine ended without its shutdown");
    }
}

#[cfg(test)]
mod tests {
    use ::kithara::platform::{
        CancelToken,
        time::{self, Duration},
        tokio::sync::{mpsc, oneshot},
    };
    use kithara_test_utils::kithara;

    use super::serve;
    use crate::engine::EngineError;

    #[kithara::test(native, tokio, flash(false))]
    async fn a_failed_build_is_reported_and_the_loop_never_starts() {
        let (built_tx, built) = oneshot::channel();
        let (_commands, received) = mpsc::unbounded_channel();
        let shutdown = CancelToken::root();

        time::timeout(
            Duration::from_secs(2),
            serve(
                || Err(EngineError::Missing("base worker")),
                built_tx,
                received,
                shutdown.child(),
            ),
        )
        .await
        .expect("a loop that started would run until a command or the cancel ends it");

        assert!(matches!(
            built.await,
            Ok(Err(EngineError::Missing("base worker")))
        ));
    }
}
