use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};

use super::{super::config::CiPins, evidence, serve, snapshot, snapshot::SnapshotArgs, verify};
use crate::{ci::host::mac::read_secret, consts};

/// Everything else a client is told, fixed by how a scope is provisioned.
///
/// A host may state these, and what it states is kept; what it leaves out is
/// filled in here. A host file is written once and outlives the code reading
/// it, so a key added later is missing from every file that predates it. When
/// that key was a required one, the prefix, sccache wrote every object to the
/// bucket root, where nothing expires, and the lane's own client refused to
/// start.
fn defaults(endpoint: &str) -> [(&'static str, &'static str); 4] {
    [
        ("SCCACHE_S3_KEY_PREFIX", consts::SCCACHE_PREFIX),
        ("SCCACHE_REGION", consts::CACHE_REGION),
        (
            "SCCACHE_S3_USE_SSL",
            if endpoint.starts_with("https://") {
                "true"
            } else {
                "false"
            },
        ),
        ("AWS_EC2_METADATA_DISABLED", "true"),
    ]
}

/// The whole environment a client is given for one scope's store.
pub(super) fn provisioned_environment(
    bucket: &str,
    endpoint: &str,
    key: &str,
    secret: &str,
) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for (name, value) in consts::CACHE_HOST_KEYS
        .into_iter()
        .zip([bucket, endpoint, key, secret])
    {
        insert_client_environment(&mut environment, name, value)?;
    }
    complete_client_environment(environment)
}

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
    for key in consts::CACHE_HOST_KEYS {
        let value = env::var(key).with_context(|| format!("{key} must be configured"))?;
        insert_client_environment(&mut environment, key, &value)?;
    }
    for (key, _) in defaults("") {
        if let Some(value) = env::var(key).ok().filter(|value| !value.is_empty()) {
            insert_client_environment(&mut environment, key, &value)?;
        }
    }
    complete_client_environment(environment)
}

/// The defaults a job's own processes lack, once the job was given a store.
///
/// sccache reads its configuration from the environment it starts in, so a
/// key the host left out has to be put into the lane's environment rather than
/// only filled in where this crate reads it. A job with no store keeps its
/// local cache and is told nothing. An empty value is a missing one: a runner
/// that forwards an unset variable hands over an empty string, and sccache
/// reads an empty prefix as the bucket root.
pub(crate) fn missing_defaults(
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(&'static str, &'static str)> {
    let lookup = |key: &str| lookup(key).filter(|value| !value.is_empty());
    let (Some(_), Some(endpoint)) = (lookup("SCCACHE_BUCKET"), lookup("SCCACHE_ENDPOINT")) else {
        return Vec::new();
    };
    defaults(&endpoint)
        .into_iter()
        .filter(|(key, _)| lookup(key).is_none())
        .collect()
}

fn insert_client_environment(
    environment: &mut BTreeMap<String, String>,
    key: &str,
    value: &str,
) -> Result<()> {
    ensure!(
        consts::CACHE_HOST_KEYS.contains(&key) || defaults("").iter().any(|(name, _)| *name == key),
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
    mut environment: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    for key in consts::CACHE_HOST_KEYS {
        ensure!(
            environment.contains_key(key),
            "the cache environment has no {key}"
        );
    }
    let endpoint = environment
        .get("SCCACHE_ENDPOINT")
        .cloned()
        .unwrap_or_default();
    for (key, value) in defaults(&endpoint) {
        environment
            .entry(key.to_owned())
            .or_insert_with(|| value.to_owned());
    }
    Ok(environment)
}

#[derive(Debug, Args)]
pub(crate) struct CacheArgs {
    /// Scoped credentials for compiling the cache service image.
    #[arg(long, env = "KITHARA_CI_CACHE_ENV_FILE")]
    cache_env_file: Option<PathBuf>,
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
    /// Run the cache stack inside its container: the store, its buckets and
    /// client credentials, and the evictor keeping each scope's compiler cache
    /// under its budget by last use.
    Serve,
    /// Verify cache reuse between two independent compiler daemons.
    Verify { env_file: Option<PathBuf> },
    /// Retain bounded observations for a compilation phase outside a lane.
    Record { phase: String },
    /// Run a compilation phase with a verified, isolated `RustFS` daemon.
    Exec {
        phase: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    /// Restore and publish immutable trusted Cargo target snapshots.
    Snapshot(SnapshotArgs),
}

/// Compose against the host's environment file, which also goes to the stack
/// whole as `CACHE_ENV_FILE`: the evictor reads a quota under a name built
/// from each scope the host serves, which the Compose file cannot list.
/// Compose resolves that path beside the Compose file, so it is made absolute.
fn compose(env_file: &Path) -> Result<Command> {
    let handed = std::path::absolute(env_file)
        .with_context(|| format!("resolving {}", env_file.display()))?;
    let mut command = Command::new("docker");
    command
        .args(["compose", "--env-file"])
        .arg(env_file)
        .args(["-f", consts::CACHE_COMPOSE_FILE])
        .env("CACHE_ENV_FILE", handed);
    Ok(command)
}

pub(crate) fn run(args: &CacheArgs) -> Result<()> {
    match &args.command {
        CacheCommand::Compose {
            env_file,
            arguments,
        } => {
            let pins = CiPins::load(Path::new(consts::PINS_PATH))?;
            let secrets = if arguments
                .iter()
                .any(|arg| matches!(arg.as_str(), "build" | "up" | "create" | "run"))
            {
                let environment = match args.cache_env_file.as_deref() {
                    Some(path) => client_environment(path)?,
                    None => current_client_environment()?,
                };
                Some(crate::ci::kache::BuildSecrets::new(&environment)?)
            } else {
                None
            };
            let mut command = compose(env_file)?;
            if let Some(secrets) = &secrets {
                command
                    .env(
                        "KITHARA_BUILD_CACHE_ENV",
                        secrets.directory().join("environment"),
                    )
                    .env(
                        "KITHARA_BUILD_KACHE_HOST",
                        secrets.directory().join("host.toml"),
                    );
            }
            let status = command
                .args(arguments)
                .env("KITHARA_CACHE_SERVER_IMAGE", &pins.cache_server_image)
                .env("KITHARA_CACHE_CLIENT_IMAGE", &pins.cache_client_image)
                .env("KITHARA_RUST_VERSION", &pins.stable_toolchain)
                .env("KITHARA_RUST_DIGEST", &pins.linux_base_digest)
                .env("KITHARA_CI_IMAGE", &pins.linux_image)
                .status()
                .context("run cache Compose")?;
            ensure!(status.success(), "cache Compose exited with {status}");
            Ok(())
        }
        CacheCommand::Serve => serve::run(),
        CacheCommand::Verify { env_file } => verify::run(env_file.as_deref()),
        CacheCommand::Record { phase } => evidence::record(phase),
        CacheCommand::Exec { phase, arguments } => {
            let status = kithara_ci_bootstrap::run(
                [OsString::from("cache"), phase.into()]
                    .into_iter()
                    .chain(arguments.iter().cloned()),
            )?;
            ensure!(status.success(), "compilation phase failed: {status}");
            Ok(())
        }
        CacheCommand::Snapshot(args) => snapshot::run(args),
    }
}

/// Fails with what the client said on stderr when it exited unsuccessfully.
pub(super) fn require_success(output: &Output, what: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(super) fn required(name: &str) -> Result<String> {
    let value = env::var(name).with_context(|| format!("{name} must be configured"))?;
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use clap::Parser;

    use super::*;
    use crate::ci::config::workspace_root;

    fn host_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.env");
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (directory, path)
    }

    /// The Linux fleet's host files were written before the prefix existed.
    /// Refusing them left the lane without its source layer, and sccache,
    /// which is not asked, wrote to the bucket root.
    #[test]
    fn a_host_file_written_before_the_prefix_still_yields_it() {
        let (_directory, path) = host_file(
            "SCCACHE_BUCKET=kithara-review\nSCCACHE_ENDPOINT=http://kithara-ci-cache:9000\n\
             SCCACHE_REGION=us-east-1\nSCCACHE_S3_USE_SSL=false\nAWS_ACCESS_KEY_ID=key\n\
             AWS_SECRET_ACCESS_KEY=secret\nAWS_EC2_METADATA_DISABLED=true\n",
        );

        let environment = client_environment(&path).expect("an older host file is complete");

        assert_eq!(
            environment.get("SCCACHE_S3_KEY_PREFIX").map(String::as_str),
            Some(consts::SCCACHE_PREFIX)
        );
    }

    #[test]
    fn a_host_names_only_its_store_and_the_credentials_for_it() {
        let (_directory, path) = host_file(
            "SCCACHE_BUCKET=kithara-trusted\nSCCACHE_ENDPOINT=https://cache\n\
             AWS_ACCESS_KEY_ID=key\nAWS_SECRET_ACCESS_KEY=secret\n",
        );

        let environment = client_environment(&path).expect("the host said all it has to");

        for (key, value) in [
            ("SCCACHE_S3_KEY_PREFIX", consts::SCCACHE_PREFIX),
            ("SCCACHE_REGION", consts::CACHE_REGION),
            ("SCCACHE_S3_USE_SSL", "true"),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ] {
            assert_eq!(
                environment.get(key).map(String::as_str),
                Some(value),
                "{key}"
            );
        }
    }

    #[test]
    fn what_a_host_states_is_kept() {
        let (_directory, path) = host_file(
            "SCCACHE_BUCKET=kithara-review\nSCCACHE_ENDPOINT=http://cache\n\
             SCCACHE_REGION=eu-central-1\nAWS_ACCESS_KEY_ID=key\nAWS_SECRET_ACCESS_KEY=secret\n",
        );

        let environment = client_environment(&path).unwrap();

        assert_eq!(
            environment.get("SCCACHE_REGION").map(String::as_str),
            Some("eu-central-1")
        );
    }

    #[test]
    fn a_host_file_without_credentials_is_refused_by_name() {
        let (_directory, path) = host_file(
            "SCCACHE_BUCKET=kithara-review\nSCCACHE_ENDPOINT=http://cache\nAWS_ACCESS_KEY_ID=key\n",
        );

        let error = client_environment(&path).expect_err("no secret, no client");

        assert!(
            error.to_string().contains("AWS_SECRET_ACCESS_KEY"),
            "{error}"
        );
    }

    #[test]
    fn a_job_given_a_store_is_told_what_its_host_left_out() {
        let environment = BTreeMap::from([
            ("SCCACHE_BUCKET", "kithara-review"),
            ("SCCACHE_ENDPOINT", "http://kithara-ci-cache:9000"),
            ("SCCACHE_REGION", "us-east-1"),
        ]);

        let missing = missing_defaults(|key| environment.get(key).map(|value| (*value).to_owned()));

        assert_eq!(
            missing,
            [
                ("SCCACHE_S3_KEY_PREFIX", consts::SCCACHE_PREFIX),
                ("SCCACHE_S3_USE_SSL", "false"),
                ("AWS_EC2_METADATA_DISABLED", "true"),
            ]
        );
    }

    #[test]
    fn a_job_handed_an_empty_prefix_is_given_the_default() {
        let environment = BTreeMap::from([
            ("SCCACHE_BUCKET", "kithara-review"),
            ("SCCACHE_ENDPOINT", "http://kithara-ci-cache:9000"),
            ("SCCACHE_S3_KEY_PREFIX", ""),
        ]);

        let missing = missing_defaults(|key| environment.get(key).map(|value| (*value).to_owned()));

        assert!(
            missing.contains(&("SCCACHE_S3_KEY_PREFIX", consts::SCCACHE_PREFIX)),
            "{missing:?}"
        );
    }

    #[test]
    fn a_job_without_a_store_is_told_nothing() {
        assert!(missing_defaults(|_| None).is_empty());
    }

    fn stack() -> serde_yaml_ng::Value {
        serde_yaml_ng::from_str(
            &fs::read_to_string(workspace_root().join(consts::CACHE_COMPOSE_FILE)).unwrap(),
        )
        .unwrap()
    }

    /// The stack reads a quota under a name built from each scope the host
    /// serves. Compose hands a container only the variables its file names,
    /// so the quota a host named for one scope never reached it, and every
    /// start flattened that scope to the shared quota.
    #[test]
    fn the_stack_is_handed_the_environment_file_it_is_started_with() {
        let host = Path::new("docker/ci-cache/linux.env");

        let command = compose(host).unwrap();

        let handed = command
            .get_envs()
            .find(|(key, _)| *key == "CACHE_ENV_FILE")
            .and_then(|(_, value)| value)
            .map(PathBuf::from);
        assert_eq!(handed, Some(env::current_dir().unwrap().join(host)));
        assert!(
            stack()["services"]["cache"]["env_file"]
                .as_str()
                .is_some_and(|file| file.starts_with("${CACHE_ENV_FILE")),
            "the stack must read the file CACHE_ENV_FILE names"
        );
    }

    /// A named volume lives wherever the Docker daemon keeps it - inside
    /// colima's virtual machine on the Mac - so a stack brought up after that
    /// daemon was reset starts with an empty store and a new admin password,
    /// and every client key the runners hold stops working. Everything the
    /// stack keeps has to sit in a directory the host names on its own disk.
    #[test]
    fn the_stack_keeps_its_state_only_in_directories_the_host_names() {
        let stack: serde_yaml_ng::Value = serde_yaml_ng::from_str(
            &fs::read_to_string(workspace_root().join(consts::CACHE_COMPOSE_FILE)).unwrap(),
        )
        .unwrap();

        assert!(
            stack.get("volumes").is_none(),
            "the stack declares named volumes"
        );
        let services = stack["services"].as_mapping().unwrap();
        let mut mounts = 0;
        for (name, service) in services {
            for mount in service["volumes"].as_sequence().into_iter().flatten() {
                let mount = mount.as_str().unwrap();
                let (source, _) = mount.split_once("}:").unwrap_or((mount, ""));
                assert!(
                    source.starts_with("${CACHE_") && source.contains("_VOLUME:?"),
                    "{name:?} mounts {mount}, which is not a host directory the environment must name"
                );
                mounts += 1;
            }
        }
        assert!(mounts > 0, "the stack mounts nothing");
    }

    /// The store, what sets it up and the evictor start and stop together,
    /// so the setup always runs the code the store is served with. The image
    /// runs the whole stack, and Docker's init stands first in the container
    /// to hand the stack the signal that stops it.
    #[test]
    fn the_stack_is_one_container_the_image_runs_whole() {
        let stack = stack();
        let services = stack["services"].as_mapping().unwrap();
        let cache = &stack["services"]["cache"];

        let dockerfile = fs::read_to_string(
            workspace_root().join(cache["build"]["dockerfile"].as_str().unwrap()),
        )
        .unwrap();
        let entrypoint: Vec<String> = dockerfile
            .lines()
            .find_map(|line| line.strip_prefix("ENTRYPOINT "))
            .map(|words| serde_json::from_str(words).unwrap())
            .expect("the image names what it runs");

        assert_eq!(services.len(), 1, "the stack runs apart");
        assert!(
            cache.get("entrypoint").is_none(),
            "the stack overrides what the image runs"
        );
        assert!(
            crate::Cli::try_parse_from(&entrypoint).is_ok(),
            "{entrypoint:?}"
        );
        assert_eq!(entrypoint.last().map(String::as_str), Some("serve"));
        assert_eq!(cache["init"].as_bool(), Some(true));
    }

    /// The store posts its audit log to the evictor beside it, and its egress
    /// filter refuses even loopback unless the origin is allowed. An endpoint
    /// that drifted from where the evictor listens would leave every read
    /// unseen, and the evictor would age entries by their writes alone. The
    /// evictor takes every delivery on trust, so it listens where no job
    /// reaches.
    #[test]
    fn the_store_sends_its_audit_log_to_the_evictor_beside_it() {
        let stack = stack();
        let cache = &stack["services"]["cache"];
        let environment = &cache["environment"];
        let setting = |name: &str| {
            environment[name]
                .as_str()
                .unwrap_or_else(|| panic!("the store sets no {name}"))
        };

        let endpoint =
            reqwest::Url::parse(setting("RUSTFS_AUDIT_WEBHOOK_ENDPOINT_RECENCY")).unwrap();

        assert_eq!(setting("RUSTFS_AUDIT_ENABLE"), "true");
        assert_eq!(setting("RUSTFS_AUDIT_WEBHOOK_ENABLE_RECENCY"), "on");
        assert!(consts::EVICT_LISTEN.ip().is_loopback());
        assert_eq!(
            endpoint.host_str(),
            Some(consts::EVICT_LISTEN.ip().to_string().as_str())
        );
        assert_eq!(endpoint.port(), Some(consts::EVICT_LISTEN.port()));
        let origin = endpoint.origin().ascii_serialization();
        assert!(
            setting("RUSTFS_OUTBOUND_ALLOW_ORIGINS")
                .split(',')
                .any(|allowed| allowed == origin),
            "the store's egress filter refuses {origin}"
        );
        let queue = setting("RUSTFS_AUDIT_WEBHOOK_QUEUE_DIR_RECENCY");
        assert!(
            cache["tmpfs"]
                .as_sequence()
                .into_iter()
                .flatten()
                .filter_map(serde_yaml_ng::Value::as_str)
                .any(|mount| mount.split(':').next() == Some(queue)),
            "the audit queue {queue} is not in memory"
        );
    }

    #[test]
    fn provisioning_writes_what_a_client_reads() {
        let written = provisioned_environment("kithara-review", "http://cache", "key", "secret")
            .expect("provisioning names every host key");

        assert_eq!(
            written.len(),
            consts::CACHE_HOST_KEYS.len() + defaults("").len()
        );
        assert_eq!(
            written.get("SCCACHE_S3_KEY_PREFIX").map(String::as_str),
            Some(consts::SCCACHE_PREFIX)
        );
    }
}
