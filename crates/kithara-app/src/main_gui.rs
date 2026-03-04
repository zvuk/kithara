use std::env;

fn main() -> kithara_app::AppResult {
    #[cfg(target_os = "macos")]
    // SAFETY: called at program start before any threads are spawned.
    unsafe {
        env::set_var("OS_ACTIVITY_MODE", "disable");
    }

    let tracks: Vec<String> = env::args().skip(1).collect();
    kithara_tui::init_tracing(&["info"], false)?;

    // iced owns the tokio runtime — do NOT wrap in rt.block_on().
    kithara_app::run_gui_sync(tracks)
}
