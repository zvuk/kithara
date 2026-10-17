use kithara::{platform::sync::Arc, play::InterruptionKind, queue::Transition};

use super::NativeInner;
use crate::{
    observer::SeekCallback,
    types::{
        FfiActionAtItemEnd, FfiCrossfadeSettings, FfiError, FfiPlaybackOrder, FfiPlayerSnapshot,
        FfiPlayerStatus, FfiRepeatMode,
    },
};
impl NativeInner {
    pub(crate) fn notify_interruption(&self, kind: InterruptionKind) {
        if let Err(error) = self.queue.notify_interruption(kind) {
            tracing::warn!(%error, "platform interruption was not accepted");
        }
    }

    pub(crate) fn advance_to_next_item(&self) -> Result<(), FfiError> {
        self.queue
            .next(Transition::None)
            .map(|_| ())
            .map_err(|error| FfiError::Internal {
                description: error.to_string(),
            })
    }

    pub(crate) fn return_to_previous_item(&self) -> Result<(), FfiError> {
        self.queue
            .previous(Transition::None)
            .map(|_| ())
            .map_err(|error| FfiError::Internal {
                description: error.to_string(),
            })
    }

    pub(crate) fn seek(
        &self,
        to_seconds: f64,
        tolerance: Option<f64>,
        callback: &Arc<dyn SeekCallback>,
    ) {
        let _ = tolerance;
        match self.queue.seek(to_seconds) {
            Ok(_outcome) => callback.on_complete(true),
            Err(_) => callback.on_complete(false),
        }
    }

    pub(crate) fn snapshot(&self) -> FfiPlayerSnapshot {
        let view = self.queue.playback_view();
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::from(self.queue.status()),
            current_time: view.position,
            duration: view.duration,
            rate: self.queue.rate(),
            playing_rate: self.queue.default_rate(),
            volume: self.queue.volume(),
            is_muted: self.queue.is_muted(),
        }
    }

    pub(crate) fn stop(&self) {
        self.queue.pause();
        let _ = self.queue.seek(0.0);
    }

    delegate::delegate! {
        to self.queue {
            #[expr($.into())]
            pub(crate) fn crossfade_settings(&self) -> FfiCrossfadeSettings;
            #[expr($.into())]
            pub(crate) fn playback_order(&self) -> FfiPlaybackOrder;
            #[expr($.into())]
            pub(crate) fn action_at_item_end(&self) -> FfiActionAtItemEnd;
            #[expr($.unwrap_or(0.0))]
            #[call(position_seconds)]
            pub(crate) fn current_time(&self) -> f64;
            pub(crate) fn is_muted(&self) -> bool;
            pub(crate) fn pause(&self);
            pub(crate) fn play(&self);
            #[call(default_rate)]
            pub(crate) fn playing_rate(&self) -> f32;
            pub(crate) fn rate(&self) -> f32;
            #[expr($.into())]
            pub(crate) fn repeat_mode(&self) -> FfiRepeatMode;
            #[expr($.map_err(FfiError::from))]
            pub(crate) fn reset_eq(&self) -> Result<(), FfiError>;
            #[expr($.map_err(FfiError::from))]
            pub(crate) fn set_muted(&self, muted: bool) -> Result<(), FfiError>;
            #[call(set_default_rate)]
            #[expr($.map_err(FfiError::from))]
            pub(crate) fn set_playing_rate(&self, rate: f32) -> Result<(), FfiError>;
            #[expr($.map_err(FfiError::from))]
            pub(crate) fn set_volume(&self, volume: f32) -> Result<(), FfiError>;
            pub(crate) fn volume(&self) -> f32;
        }
    }
}
