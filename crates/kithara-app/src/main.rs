use std::io::IsTerminal;

use clap::Parser;

/// Kithara — audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player with TUI and GUI modes")]
struct Args {
    /// Force TUI mode.
    #[arg(long, conflicts_with = "gui")]
    tui: bool,

    /// Force GUI mode.
    #[arg(long, conflicts_with = "tui")]
    gui: bool,

    /// Audio files or URLs to play.
    tracks: Vec<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> kithara_app::AppResult {
    let args = Args::parse();

    let mode = if args.tui {
        kithara_app::Mode::Tui
    } else if args.gui {
        kithara_app::Mode::Gui
    } else if std::io::stdin().is_terminal() {
        kithara_app::Mode::Tui
    } else {
        kithara_app::Mode::Gui
    };

    kithara_tui::init_tracing(&["off"], mode == kithara_app::Mode::Tui)?;
    kithara_app::run(mode, args.tracks).await
}
