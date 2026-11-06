use clap::Parser;
use kithara::{
    host::{HostConfig, HostSettings},
    platform::{CancelToken, tokio},
};
use kithara_app::{
    config::AppConfig, document::Config, gui, logging::init_tracing, memory, plugins, pools,
};
use kithara_app_library::{Environment, Secrets};

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

/// The value `result` holds; its error is printed and ends the process.
fn or_exit<T>(result: Result<T, impl std::fmt::Display>) -> T {
    result.unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1)
    })
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
    let document = or_exit(Config::load(
        args.config.as_deref(),
        config_beside_binary().as_deref(),
    ));
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

    let pools = pools::build(&document.pools())?;
    let net = AppConfig::client(&document, &pools, &shutdown, args.insecure);
    let environment = Environment::new(
        runtime.handle().clone(),
        net.clone(),
        Secrets::native(document.overlay()),
    );
    let registered = or_exit(plugins::mount(&document, &environment, &shutdown));
    let mut config = or_exit(
        AppConfig::assemble()
            .document(&document)
            .pools(pools)
            .net(net)
            .grants(&plugins::grants(&registered))
            .shutdown(shutdown)
            .runtime(runtime.handle().clone())
            .maybe_ui_package(shipped_ui_package())
            .call(),
    );
    if !args.tracks.is_empty() {
        config.tracks = args.tracks;
    }
    memory::set_limit(config.memory_limit_bytes);
    if let Some(package) = args.ui_package {
        config.ui_package = Some(package);
    }

    let host = pools::host(
        HostConfig::builder()
            .settings(
                HostSettings::builder()
                    .maybe_sample_rate(config.sample_rate)
                    .build(),
            )
            .maybe_output_block_frames(config.output_block_frames)
            .build(),
    )?;
    gui::run(
        config,
        document.overlay(),
        registered,
        args.host,
        host,
        runtime.handle(),
    )?;

    Ok(())
}
