use std::{
    io::{self, Write},
    num::NonZeroUsize,
};

use kithara::{
    analysis::AnalysisOutcome,
    platform::{
        CancelToken,
        sync::Arc,
        tokio::{
            select,
            sync::{Semaphore, mpsc},
            task::{self, JoinHandle},
        },
    },
};

use crate::{
    Batch, RunError,
    analysis::{self, Engine, OpenError},
    input::Track,
    sink::Sink,
};

pub(crate) type TrackResult = (Track, Result<AnalysisOutcome, OpenError>);

impl Batch {
    pub(crate) async fn analyze(
        self,
        jobs: NonZeroUsize,
        cancel: CancelToken,
        output: mpsc::Sender<TrackResult>,
    ) -> Result<(), RunError> {
        let permits = Arc::new(Semaphore::new(jobs.get()));
        let engine = Arc::new(Engine::try_new(&self.args, jobs, cancel.clone())?);
        let mut tasks: Vec<JoinHandle<Result<(), RunError>>> =
            Vec::with_capacity(self.tracks.len());
        for track in self.tracks {
            let permit = select! {
                biased;
                () = cancel.cancelled() => break,
                permit = Arc::clone(&permits).acquire_owned() => permit.map_err(io::Error::other)?,
            };
            let engine = Arc::clone(&engine);
            let output = output.clone();
            tasks.push(task::spawn(async move {
                let outcome = engine.analyze(&track).await;
                drop(permit);
                output
                    .send((track, outcome))
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
                Ok(())
            }));
        }
        drop(output);
        let mut result = Ok(());
        for task in tasks {
            result = result.and(task.await.map_err(RunError::from).flatten());
        }
        result
    }

    /// Analyze the validated batch and write each record as soon as it finishes.
    /// Returns whether every emitted record settled, together with the writer.
    ///
    /// # Errors
    /// Returns an error if configuration, task execution or output fails.
    pub async fn run<W: Write + Send + 'static>(
        self,
        cancel: CancelToken,
        writer: W,
    ) -> Result<(bool, W), RunError> {
        let format = self.args.format;
        let sink = Sink::try_new(self.args.out.clone(), writer)?;
        let jobs = analysis::jobs(&self.args)?;
        let (output, records) = mpsc::channel(jobs.get());
        let writer = task::spawn(sink.consume(records, format));
        let analyzed = self.analyze(jobs, cancel.clone(), output).await;
        let (success, writer) = writer.await??;
        analyzed?;
        Ok((success && !cancel.is_cancelled(), writer))
    }
}
