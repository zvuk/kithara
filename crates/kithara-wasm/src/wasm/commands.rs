//! Command types for main-thread bridge ↔ engine Worker communication.

/// Commands sent from the main-thread bridge to the engine Worker.
#[derive(Clone)]
pub(crate) enum WorkerCmd {
    SelectTrack { url: String, request_id: u32 },
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetVolume(f32),
    SetCrossfade(f32),
    SetEqGain { band: u32, gain_db: f32 },
    ResetEq,
    SetDucking(u32),
}
