use clap::Parser;
use kithara::{
    host::HostConfig,
    platform::{CancelToken, tokio},
};
use kithara_app::{
    config::AppConfig,
    document::Config,
    gui, memory,
    pools::{self, AppHost},
    tracing_init::init_tracing,
};

/// Kithara - audio player application.
#[derive(Parser)]
#[command(name = "kithara", about = "Audio player")]
struct Args {
    /// Which host draws the studio. A build without the `masonry` feature has
    /// only the immediate one.
    #[arg(long, value_enum, default_value_t)]
    host: gui::Host,

    /// Configuration document to read. Defaults to `kithara.yaml` beside the
    /// executable when one is there.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Folder holding the UI package to draw from. An override on top of the
    /// document's `app.ui_package`: a path here wins regardless of what the
    /// document says. With neither, the package a release lays out beside the
    /// executable draws.
    #[arg(long)]
    ui_package: Option<std::path::PathBuf>,

    /// Audio files or URLs to play.
    tracks: Vec<String>,

    /// Print the effective configuration and exit.
    #[arg(long)]
    dump_config: bool,

    /// Accept invalid TLS certificates (self-signed, expired). For test servers only.
    /// An override on top of the document's `net.is_insecure`: `true` here
    /// forces it on regardless of the document.
    #[arg(long)]
    insecure: bool,
}

#[cfg(debug_assertions)]
#[global_allocator]
static HEAP: memory::Ceiling = memory::Ceiling;

fn shipped_ui_package() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("assets/ui"))
}

fn config_beside_binary() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("kithara.yaml"))
}

type AppError = Box<dyn std::error::Error + Send + Sync>;
pub(super) type AppResult<T = ()> = Result<T, AppError>;

#[cfg(target_os = "macos")]
fn suppress_macos_system_logs() {
    // SAFETY: called at program start before any threads are spawned.
    unsafe {
        std::env::set_var("OS_ACTIVITY_MODE", "disable");
    }
}

#[cfg(not(target_os = "macos"))]
fn suppress_macos_system_logs() {}

pub(super) fn main(shutdown: CancelToken) -> AppResult {
    suppress_macos_system_logs();

    let args = Args::parse();
    let document = match Config::load(args.config.as_deref(), config_beside_binary().as_deref()) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    if args.dump_config {
        println!("{}", document.dump());
        return Ok(());
    }

    let directives = document
        .app()
        .log_directives
        .unwrap_or_else(|| vec!["info".to_string()]);
    init_tracing(&directives.iter().map(String::as_str).collect::<Vec<&str>>())?;
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();

    let assembled = AppConfig::assemble()
        .document(&document)
        .pools(pools::build(&document.pools())?)
        .shutdown(shutdown)
        .runtime(runtime.handle().clone())
        .is_insecure(args.insecure)
        .maybe_ui_package(shipped_ui_package())
        .call();
    let mut config = match assembled {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    if !args.tracks.is_empty() {
        config.tracks = args.tracks;
    }
    memory::set_limit(config.memory_limit_bytes);
    if let Some(package) = args.ui_package {
        config.ui_package = Some(package);
    }

    let host = AppHost::new(
        HostConfig::builder()
            .maybe_sample_rate_hint(config.sample_rate)
            .maybe_output_block_frames(config.output_block_frames)
            .build(),
    )?;
    gui::run(config, args.host, host)?;

    Ok(())
}
