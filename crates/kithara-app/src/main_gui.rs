#[tokio::main(flavor = "current_thread")]
async fn main() -> kithara_app::AppResult {
    let tracks: Vec<String> = std::env::args().skip(1).collect();
    kithara_tui::init_tracing(&["off"], false)?;
    kithara_app::run(kithara_app::Mode::Gui, tracks).await
}
