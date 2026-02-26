//! Command types for main-thread bridge ↔ engine Worker communication.

/// Commands sent from the main-thread bridge to the engine Worker.
pub(crate) enum WorkerCmd {
    SelectTrack {
        url: String,
        reply_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetVolume(f32),
    SetCrossfade(f32),
    SetEqGain {
        band: u32,
        gain_db: f32,
    },
    ResetEq,
    SetDucking(u32),
    TestHlsRead,
}
