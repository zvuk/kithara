use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};

use super::{super::config::CiPins, provision, snapshot, snapshot::SnapshotArgs, verify};
use crate::{ci::host::mac::read_secret, consts};

/// Read the restricted environment a cache client may inherit.
pub(crate) fn client_environment(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for line in read_secret(path)?.lines() {
        let (key, value) = line
            .split_once('=')
            .context("invalid cache environment entry")?;
        insert_client_environment(&mut environment, key, value)?;
    }
    complete_client_environment(environment)
}

/// Read the restricted cache credentials injected into a CI job.
pub(crate) fn current_client_environment() -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for key in consts::CLIENT_KEYS {
        let value = env::var(key).with_context(|| format!("{key} must be configured"))?;
        insert_client_environment(&mut environment, key, &value)?;
    }
    complete_client_environment(environment)
}

fn insert_client_environment(
    environment: &mut BTreeMap<String, String>,
    key: &str,
    value: &str,
) -> Result<()> {
    ensure!(
        consts::CLIENT_KEYS.contains(&key),
        "unexpected cache environment key"
    );
    ensure!(!value.is_empty(), "empty cache environment value");
    ensure!(
        !value
            .chars()
            .any(|character| character.is_control() || matches!(character, '"' | '\\')),
        "unsafe cache environment value"
    );
    ensure!(
        environment
            .insert(key.to_owned(), value.to_owned())
            .is_none(),
        "duplicate cache environment key"
    );
    Ok(())
}

fn complete_client_environment(
    environment: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    ensure!(
        environment.len() == consts::CLIENT_KEYS.len(),
        "incomplete cache environment"
    );
    Ok(environment)
}

#[derive(Debug, Args)]
pub(crate) struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Operate the shared compiler cache through Docker Compose.
    Compose {
        env_file: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    /// Create persistent administrator credentials inside the Compose volume.
    Credentials,
    /// Initialize isolated buckets and client credentials inside Compose.
    Initialize,
    /// Verify cache reuse between two independent compiler daemons.
    Verify { env_file: PathBuf },
    /// Restore and publish immutable trusted Cargo target snapshots.
    Snapshot(SnapshotArgs),
}

pub(crate) fn run(args: &CacheArgs) -> Result<()> {
    match &args.command {
        CacheCommand::Compose {
            env_file,
            arguments,
        } => {
            let pins = CiPins::load(Path::new(consts::PINS_PATH))?;
            let status = Command::new("docker")
                .args(["compose", "--env-file"])
                .arg(env_file)
                .args(["-f", "docker/ci-cache.compose.yml"])
                .args(arguments)
                .env("KITHARA_CACHE_IMAGE", &pins.sccache_s3_image)
                .env("KITHARA_RUST_VERSION", &pins.stable_toolchain)
                .env("KITHARA_RUST_DIGEST", &pins.linux_base_digest)
                .status()
                .context("run cache Compose")?;
            ensure!(status.success(), "cache Compose exited with {status}");
            Ok(())
        }
        CacheCommand::Credentials => provision::credentials(),
        CacheCommand::Initialize => provision::initialize(),
        CacheCommand::Verify { env_file } => verify::run(env_file),
        CacheCommand::Snapshot(args) => snapshot::run(args),
    }
}

pub(super) fn required(name: &str) -> Result<String> {
    let value = env::var(name).with_context(|| format!("{name} must be configured"))?;
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(value)
}
