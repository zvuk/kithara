use clap::{Parser, Subcommand};
use kithara_devtools::{CoreCommand, Ctx};

mod agent_hook;
mod android;
mod apple;
mod apple_docgen;
mod ci;
mod config;
mod mutants;
mod parity;
mod publish;
mod release;
mod self_cache;
mod wasm;

use android::AndroidCommand;
use apple::AppleCommand;
use ci::CiArgs;
use mutants::MutantsArgs;
use parity::ParityArgs;
use publish::PublishArgs;
use release::ReleaseArgs;
use self_cache::SelfCacheArgs;
use wasm::WasmCommand;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum BuildProfile {
    Debug,
    Release,
}

impl std::fmt::Display for BuildProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Debug => write!(f, "debug"),
            Self::Release => write!(f, "release"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Workspace automation tasks for kithara")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Android build tasks.
    Android {
        #[command(subcommand)]
        command: AndroidCommand,
    },
    /// Apple build tasks.
    Apple {
        #[command(subcommand)]
        command: AppleCommand,
    },
    /// WASM build and post-build tasks.
    Wasm {
        #[command(subcommand)]
        command: WasmCommand,
    },
    /// Dedicated `GitLab` CI automation.
    Ci(CiArgs),
    /// Publish all public crates to crates.io in dependency order.
    Publish(PublishArgs),
    /// Run explicitly scoped mutation suites.
    Mutants(MutantsArgs),
    /// Photograph the gallery and the shipped studio pages through both hosts
    /// and compare the sets.
    Parity(ParityArgs),
    /// Apple release flow: prepare (stamp manifests) and publish
    /// (GitHub release + `GitLab` mirror).
    Release(ReleaseArgs),
    /// Agent editor/shell hooks for tool-specific adapters.
    AgentHook,
    #[command(hide = true)]
    SelfCache(SelfCacheArgs),
    #[command(flatten)]
    Core(CoreCommand),
}

fn main() -> std::process::ExitCode {
    match work() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // A check that ran and found something is not a crash. Printing it as
        // one — `Error:` and a backtrace through the Rust runtime — reads as a
        // broken tool, which sends the reader looking for a defect that is not
        // there. See `kithara_devtools::verdict`.
        Err(error) => std::process::ExitCode::from(
            u8::try_from(kithara_devtools::verdict::NotClean::report(&error)).unwrap_or(1),
        ),
    }
}

fn work() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .compact()
        .try_init();
    let _lease: Option<self_cache::GenerationLease> = self_cache::lease_current()?;
    let cli = Cli::parse();
    match &cli.command {
        Command::AgentHook => return agent_hook::run(),
        Command::SelfCache(args) => return self_cache::run(args),
        Command::Ci(args) if ci::is_standalone(args) => return ci::run_standalone(args),
        _ => {}
    }
    // Held for the life of the process so the host's build-cache budget
    // leaves the shared target directory alone while work runs in it.
    let _target_lease = ci::hold_target_lease();
    let ctx = Ctx::load()?;

    match cli.command {
        Command::Android { command } => android::run(command, &ctx),
        Command::Apple { command } => apple::run(command, &ctx),
        Command::Wasm { command } => wasm::run(command, &ctx),
        Command::Ci(ref args) => ci::run(args, &ctx),
        Command::Publish(ref args) => publish::run(args, &ctx),
        Command::Mutants(ref args) => mutants::run(args, &ctx),
        Command::Parity(ref args) => parity::run(args, &ctx),
        Command::Release(ref args) => release::run(args, &ctx),
        Command::AgentHook => agent_hook::run(),
        Command::SelfCache(ref args) => self_cache::run(args),
        Command::Core(cmd) => kithara_devtools::run(&cmd, &ctx),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use kithara_devtools::CoreCommand;

    use super::{Cli, Command};

    #[test]
    fn agent_hook_uses_payload_without_cli_discriminator() {
        assert!(Cli::try_parse_from(["xtask", "agent-hook"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "agent-hook", "pre-bash"]).is_err());
    }

    #[test]
    fn self_cache_command_is_available_to_just_transport() {
        assert!(Cli::try_parse_from(["xtask", "self-cache", "probe"]).is_ok());
    }

    /// `.cargo/config.toml` aliases `cargo xtask` to `cargo run --package xtask`,
    /// which refuses to pick when the package carries several binaries.
    /// `src/bin/fake-tool.rs` is the second one, so the manifest has to name this
    /// file's binary as the default or every `cargo xtask` invocation fails.
    #[test]
    fn the_cargo_xtask_alias_resolves_to_this_binary() {
        let manifest: toml::Table = include_str!("../Cargo.toml").parse().unwrap();

        assert_eq!(
            manifest["package"]
                .get("default-run")
                .and_then(toml::Value::as_str),
            Some("xtask")
        );
    }

    #[test]
    fn quality_lab_commands_are_nested_under_quality() {
        assert!(Cli::try_parse_from(["xtask", "quality", "lab", "list"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "quality", "lab", "run", "scheduled"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "quality-lab", "list"]).is_err());
    }

    #[test]
    fn stress_run_commands_are_nested_under_stress() {
        assert!(
            Cli::try_parse_from([
                "xtask",
                "stress",
                "run",
                "--subject-root",
                "/subject",
                "--output",
                "/raw",
                "--count",
                "2",
                "--mode",
                "reproduction",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "xtask",
                "stress",
                "report",
                "--raw",
                "/raw",
                "--expected-controller-sha",
                "0123456789abcdef0123456789abcdef01234567",
                "--expected-subject-sha",
                "0123456789abcdef0123456789abcdef01234567",
                "--filter",
                "all()",
                "--count",
                "2",
                "--mode",
                "reproduction",
                "--execute-result",
                "failure",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["xtask", "stress-run"]).is_err());
    }

    #[test]
    fn audit_clippy_fix_accepts_dirty_override_and_scope() {
        let cli = Cli::try_parse_from([
            "xtask",
            "audit-clippy",
            "--fix",
            "--allow-dirty",
            "-p",
            "kithara-ui",
        ])
        .expect("audit-clippy fix CLI");
        let Command::Core(CoreCommand::AuditClippy(args)) = cli.command else {
            panic!("expected audit-clippy command");
        };

        assert!(args.fix);
        assert!(args.allow_dirty);
        assert_eq!(args.paths, ["-p", "kithara-ui"]);
    }
}
