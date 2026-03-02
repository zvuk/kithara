use clap::Parser;
use kithara_app::{AppResult, Mode, resolve_mode};

/// Kithara — audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player with TUI and GUI modes")]
struct Args {
    /// UI mode: auto, tui, or gui (default: auto-detect from terminal).
    #[arg(long, short, default_value = "auto")]
    mode: Mode,

    /// Audio files or URLs to play.
    tracks: Vec<String>,
}

fn main() -> AppResult {
    // Suppress noisy macOS system logs (OpenGL dlsym, WindowTab, etc.)
    #[cfg(target_os = "macos")]
    // SAFETY: called at program start before any threads are spawned.
    unsafe {
        std::env::set_var("OS_ACTIVITY_MODE", "disable");
    }

    let args = Args::parse();
    let mode = resolve_mode(args.mode);
    let log_directives: &[&str] = match mode {
        Mode::Tui => &["off"],
        _ => &["info"],
    };
    kithara_tui::init_tracing(log_directives, mode == Mode::Tui)?;

    let rt = build_runtime(mode)?;
    rt.block_on(kithara_app::run(mode, args.tracks))
}

fn build_runtime(mode: Mode) -> Result<tokio::runtime::Runtime, std::io::Error> {
    match mode {
        Mode::Gui => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("kithara-worker")
            .build(),
        _ => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
    }
}
