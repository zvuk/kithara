fn main() -> kithara_app::AppResult {
    #[cfg(target_os = "macos")]
    // SAFETY: called at program start before any threads are spawned.
    unsafe {
        std::env::set_var("OS_ACTIVITY_MODE", "disable");
    }

    let tracks: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("kithara-worker")
        .build()?;
    kithara_tui::init_tracing(&["info"], false)?;
    rt.block_on(kithara_app::run(kithara_app::Mode::Gui, tracks))
}
