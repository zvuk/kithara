use std::io::Write;

use kithara::{
    analysis::AnalysisOutcome,
    platform::{
        CancelToken,
        sync::Arc,
        tokio::{
            select,
            sync::{Semaphore, mpsc, oneshot},
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
        cancel: CancelToken,
        output: mpsc::Sender<TrackResult>,
    ) -> Result<(), RunError> {
        let permits = Arc::new(Semaphore::new(analysis::jobs(&self.args)?.get()));
        let engine = Arc::new(Engine::try_new(&self.args, cancel.clone())?);
        let mut tasks: Vec<JoinHandle<Result<(), RunError>>> =
            Vec::with_capacity(self.tracks.len());
        let mut ready: Option<oneshot::Receiver<()>> = None;
        for track in self.tracks {
            let permits = Arc::clone(&permits);
            let engine = Arc::clone(&engine);
            let output = output.clone();
            let cancel = cancel.clone();
            let previous = ready.take();
            let (next, turn) = oneshot::channel();
            ready = Some(turn);
            tasks.push(task::spawn(async move {
                if let Some(previous) = previous {
                    select! {
                        biased;
                        () = cancel.cancelled() => return Ok(()),
                        result = previous => result.map_err(|error| std::io::Error::other(error.to_string()))?,
                    }
                }
                let _permit = select! {
                    biased;
                    () = cancel.cancelled() => return Ok(()),
                    result = permits.acquire_owned() => result.map_err(|error| std::io::Error::other(error.to_string()))?,
                };
                next.send(()).map_err(|()| std::io::Error::other("admission queue closed"))?;
                if cancel.is_cancelled() { return Ok(()); }
                let outcome = engine.analyze(&track).await;
                output.send((track, outcome)).await.map_err(|error| std::io::Error::other(error.to_string()))?;
                Ok::<(), RunError>(())
            }));
        }
        drop(output);
        let mut result = Ok(());
        for task in tasks {
            result = result.and(task.await.map_err(RunError::from).flatten());
        }
        drop(ready);
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
        let (output, records) = mpsc::channel(analysis::jobs(&self.args)?.get());
        let writer = task::spawn(sink.consume(records, format));
        let analyzed = self.analyze(cancel.clone(), output).await;
        let (success, writer) = writer.await??;
        analyzed?;
        Ok((success && !cancel.is_cancelled(), writer))
    }
}
