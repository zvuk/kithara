use js_sys::Function;
use kithara::{
    platform::{sync::Arc, tokio::task::spawn},
    play::{InterruptionKind, PlayError, Position},
    queue::{QueueCommand, TrackId, Transition},
};
use kithara_command::When;

use super::WasmInner;
use crate::{
    observer::{PlayerObserver, SeekCallback},
    pools::{FfiPools, FfiQueueControl},
    types::{FfiError, FfiPlayerSnapshot, FfiPlayerStatus},
};

impl WasmInner {
    const MS_PER_SECOND: f64 = 1000.0;

    pub(crate) fn advance_to_next_item(&self) -> Result<(), FfiError> {
        self.submit(QueueCommand::Next(Transition::None))
    }

    pub(crate) fn analyze(&self, id: TrackId) -> Result<(), FfiError> {
        let (sender, request) = self.bridge.analysis_request(id)?;
        let queue = self.bridge.control()?;
        let admissions = self.items.lock().admissions();
        if admissions.is_empty() {
            if queue.is_closed() {
                return Err(FfiError::Internal {
                    description: PlayError::Closed.to_string(),
                });
            }
            if queue.track(id).is_none() {
                return Err(FfiError::InvalidArgument {
                    reason: format!("track {id:?} is not queued"),
                });
            }
            return sender.send(request).map_err(|_| FfiError::Internal {
                description: "analysis worker closed".into(),
            });
        }
        let routes = self.routes.clone();
        spawn(async move {
            for admission in admissions {
                if let Err(error) = admission.await {
                    routes.error(format!("analysis admission closed: {error}"));
                    return;
                }
            }
            if queue.is_closed() {
                routes.error(PlayError::Closed.to_string());
            } else if queue.track(id).is_none() {
                routes.error(format!("track {id:?} is not queued"));
            } else if sender.send(request).is_err() {
                routes.error("analysis worker closed".to_owned());
            }
        });
        Ok(())
    }

    pub(crate) fn notify_interruption(&self, kind: InterruptionKind) {
        self.send(QueueCommand::NotifyInterruption(kind));
    }

    pub(crate) fn pause(&self) {
        self.send(QueueCommand::Pause { at: When::Next });
    }

    pub(crate) fn play(&self) {
        self.send(QueueCommand::Play { at: When::Next });
    }

    delegate::delegate! {
        to self.routes {
            #[call(set_analysis)]
            pub(crate) fn set_analysis_observer(&self, func: Function);
            #[call(set_player)]
            pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>);
        }
        to self.bridge {
            #[call(position_secs)]
            pub(crate) fn current_time(&self) -> f64;
            /// Audio-thread process calls served so far.
            #[call(process_calls)]
            pub(crate) fn rt_process_calls(&self) -> u64;
            /// Underruns the audio thread has recorded so far.
            #[call(underruns)]
            pub(crate) fn rt_underruns(&self) -> u64;
            #[expr($.map_or(0.0, FfiQueueControl::rate))]
            #[call(queue)]
            pub(crate) fn rate(&self) -> f32;
        }
    }

    pub(crate) fn return_to_previous_item(&self) -> Result<(), FfiError> {
        self.submit(QueueCommand::Previous(Transition::None))
    }

    fn seek_command(&self, seconds: f64) -> Result<QueueCommand<FfiPools>, FfiError> {
        let seconds = if seconds == f64::INFINITY {
            self.bridge
                .queue()
                .and_then(FfiQueueControl::duration_seconds)
                .unwrap_or_default()
        } else {
            seconds.max(0.0)
        };
        let to =
            Position::try_from_secs_f64(seconds).map_err(|error| FfiError::InvalidArgument {
                reason: error.to_string(),
            })?;
        Ok(QueueCommand::Seek { to })
    }

    pub(crate) fn seek(
        &self,
        to_seconds: f64,
        tolerance: Option<f64>,
        callback: &Arc<dyn SeekCallback>,
    ) {
        let _ = tolerance;
        let ticket = match self
            .seek_command(to_seconds)
            .and_then(|command| self.post(command))
        {
            Ok(ticket) => ticket,
            Err(error) => {
                callback.on_complete(false);
                self.routes.error(error.to_string());
                return;
            }
        };
        let callback = Arc::clone(callback);
        let routes = self.routes.clone();
        spawn(async move {
            let result = super::core::answer(ticket).await;
            callback.on_complete(result.is_ok());
            if let Err(error) = result {
                routes.error(error.to_string());
            }
        });
    }

    pub(crate) fn seek_ms(&self, position_ms: f64) {
        self.report(
            self.seek_command(position_ms.max(0.0) / Self::MS_PER_SECOND)
                .and_then(|command| self.submit(command)),
        );
    }

    pub(crate) fn snapshot(&self) -> FfiPlayerSnapshot {
        FfiPlayerSnapshot {
            status: self
                .bridge
                .queue()
                .map_or(FfiPlayerStatus::Unknown, |queue| queue.status().into()),
            current_time: self
                .bridge
                .queue()
                .and_then(FfiQueueControl::position_seconds),
            duration: self
                .bridge
                .queue()
                .and_then(FfiQueueControl::duration_seconds),
            rate: self.rate(),
            playing_rate: self.playing_rate(),
            volume: self.volume(),
            is_muted: self.is_muted(),
        }
    }

    pub(crate) fn stop(&self) {
        self.pause();
        self.seek_ms(0.0);
    }
}
