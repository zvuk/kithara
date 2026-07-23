use kithara_platform::{sync::mpsc, thread::assert_main_thread};

use crate::session::{self as play_session, CmdMsg, SessionHandle};

/// Ensure the main-thread session client exists in `Local` mode.
///
/// Must be called on the main thread before [`worker_session_channel`] so that
/// the main thread gets a `Local` session and Workers get `Remote`.
pub fn ensure_main_session() {
    assert_main_thread("ensure_main_session");
    play_session::local_session();
}

/// Create a Worker -> main-thread session channel owned by the caller.
///
/// The receiver must be passed to [`tick_and_poll`] on the main thread.
#[must_use]
pub fn worker_session_channel() -> (mpsc::Sender<CmdMsg>, mpsc::Receiver<CmdMsg>) {
    assert_main_thread("worker_session_channel");
    play_session::worker_channel()
}

/// Build a Worker-side remote session handle for a caller-owned channel.
#[must_use]
pub fn remote_session(tx: mpsc::Sender<CmdMsg>) -> SessionHandle {
    play_session::remote_session(tx)
}

/// Pre-initialise the audio context and AudioWorklet eagerly.
///
/// Call on the main thread after [`ensure_main_session`]. This creates the
/// AudioContext in suspended state and starts the async AudioWorklet module
/// load. Once complete, `firewheel-web-audio` registers auto-resume listeners
/// so that the very first user click resumes the context.
pub fn warm_up_audio() {
    assert_main_thread("warm_up_audio");
    play_session::warm_up_audio();
}

/// Poll pending session commands from Workers and update the audio graph.
///
/// Call this on the main thread from `requestAnimationFrame`.
pub fn tick_and_poll(rx: &mpsc::Receiver<CmdMsg>) {
    assert_main_thread("tick_and_poll");
    play_session::tick_and_poll_remote(rx);
}
