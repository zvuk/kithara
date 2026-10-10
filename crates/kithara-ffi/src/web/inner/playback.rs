use js_sys::Function;
use kithara::{platform::sync::Arc, play::InterruptionKind, queue::TrackId};

use super::WasmInner;
use crate::{
    observer::{PlayerObserver, SeekCallback},
    types::{FfiError, FfiPlayerSnapshot, FfiPlayerStatus},
    web::commands::WorkerCmd,
};
impl WasmInner {
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    pub(crate) fn advance_to_next_item(&self) -> Result<(), FfiError> {
        self.try_send(WorkerCmd::Next)
    }

    /// Start (or restart) the analysis pass for a queued track.
    pub(crate) fn analyze(&self, id: TrackId) -> Result<(), FfiError> {
        let request_id = Self::next_request_id();
        self.try_send(WorkerCmd::Analyze { id, request_id })
    }

    pub(super) fn next_request_id() -> u32 {
        crate::web::interop::next_request_id()
    }

    pub(crate) fn notify_interruption(&self, _kind: InterruptionKind) {}

    pub(crate) fn pause(&self) {
        self.send(WorkerCmd::Pause);
    }

    pub(crate) fn play(&self) {
        self.send(WorkerCmd::Play);
    }

    pub(crate) fn rate(&self) -> f32 {
        if self.bridge.is_playing() {
            self.settings.playing_rate()
        } else {
            0.0
        }
    }

    pub(crate) fn return_to_previous_item(&self) -> Result<(), FfiError> {
        self.try_send(WorkerCmd::Previous)
    }

    pub(crate) fn seek(
        &self,
        to_seconds: f64,
        tolerance: Option<f64>,
        callback: &Arc<dyn SeekCallback>,
    ) {
        let _ = tolerance;
        self.send(WorkerCmd::Seek(to_seconds.max(0.0) * Self::MS_PER_SECOND));
        callback.on_complete(true);
    }

    /// Fire-and-forget seek in milliseconds for the JS control surface
    /// (no [`SeekCallback`] round-trip). The shared facade `seek` carries
    /// a callback; this is the wasm-only convenience the JS surface uses.
    pub(crate) fn seek_ms(&self, position_ms: f64) {
        self.send(WorkerCmd::Seek(position_ms.max(0.0)));
    }

    /// Forward commands for infallible facade methods such as play and pause.
    /// Log dropped commands because these methods have no error channel.
    pub(super) fn send(&self, cmd: WorkerCmd) {
        super::core::send(&self.bridge, cmd);
    }

    pub(super) fn try_send(&self, cmd: WorkerCmd) -> Result<(), FfiError> {
        super::core::try_send(&self.bridge, cmd)
    }

    pub(crate) fn snapshot(&self) -> FfiPlayerSnapshot {
        let position = self.bridge.position_secs();
        let duration = self.bridge.duration_secs();
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::ReadyToPlay,
            current_time: (position > 0.0).then_some(position),
            duration: (duration > 0.0).then_some(duration),
            rate: self.rate(),
            playing_rate: self.settings.playing_rate(),
            volume: self.settings.volume(),
            is_muted: self.settings.is_muted(),
        }
    }

    pub(crate) fn stop(&self) {
        self.send(WorkerCmd::Stop);
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
        }
    }
}
