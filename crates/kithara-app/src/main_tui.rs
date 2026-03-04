use std::env;

#[tokio::main(flavor = "current_thread")]
async fn main() -> kithara_app::AppResult {
    let tracks: Vec<String> = env::args().skip(1).collect();
    kithara_tui::init_tracing(&["off"], true)?;
    kithara_app::run(kithara_app::Mode::Tui, tracks).await
}
