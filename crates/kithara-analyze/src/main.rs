use std::{io, io::Error, process::ExitCode};

use clap::Parser;
use kithara::platform::{CancelScope, sync::Arc, thread, tokio::runtime::Builder};
use kithara_analyze::{Args, Batch, RunError};
use signal_hook::{consts::SIGINT, iterator::Signals};
use tracing::{Level, error};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            error!(%error, "batch analysis failed");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, RunError> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_max_level(Level::WARN)
        .try_init()
        .map_err(|error| Error::other(error.to_string()))?;
    let args = Args::parse();
    let batch = match Batch::try_new(args, io::stdin().lock()) {
        Ok(batch) => batch,
        Err(error) => error.exit(),
    };
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    let scope = Arc::new(CancelScope::new(None));
    let mut signals = Signals::new([SIGINT])?;
    let shutdown = signals.handle();
    let signal_scope = Arc::clone(&scope);
    let signal_thread = thread::spawn_named("analyze-sigint", move || {
        if signals.forever().next().is_some() {
            signal_scope.cancel();
        }
    });
    let result = runtime.block_on(batch.run(scope.token(), io::stdout()));
    scope.cancel();
    shutdown.close();
    signal_thread
        .join()
        .map_err(|_| Error::other("SIGINT thread panicked"))?;
    let (success, _) = result?;
    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
