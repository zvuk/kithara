use std::{collections::BTreeSet, fs, path::Path, process::Stdio};

use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::json;

use super::{
    container::{Container, container},
    profile::{LinuxHost, RunnerTarget},
    registration::{write_kache, write_secret},
};
use crate::{
    ci::{cache::client_environment, config::CiPins, process::Process},
    consts,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    url: String,
    project: u64,
    id: u64,
    #[serde(default)]
    token: String,
    labels: BTreeSet<String>,
    #[serde(default)]
    protected: bool,
}

impl Credential {
    fn load(runner: &super::profile::LinuxRunner) -> Result<Self> {
        let RunnerTarget::Gitlab {
            url,
            project,
            token_file,
            ..
        } = &runner.target
        else {
            return Err(anyhow!("native credentials require a GitLab worker"));
        };
        let credential: Self = serde_json::from_slice(
            &fs::read(token_file)
                .with_context(|| format!("reading GitLab credential {}", token_file.display()))?,
        )
        .map_err(|error| {
            anyhow!(
                "invalid project-bound GitLab credential at {}:{}",
                error.line(),
                error.column()
            )
        })?;
        let origin = reqwest::Url::parse(url)?;
        ensure!(
            credential.project == *project && reqwest::Url::parse(&credential.url)? == origin,
            "GitLab credential belongs to another project or server"
        );
        ensure!(
            credential.id > 0,
            "GitLab credential has no runner identity"
        );
        ensure!(
            credential.labels == runner.labels.iter().cloned().collect(),
            "GitLab credential tags differ from the declared worker"
        );
        ensure!(
            !matches!(
                runner.cache_trust,
                crate::ci::environment::CacheTrust::Trusted
            ) || credential.protected,
            "trusted GitLab cache requires a protected native runner"
        );
        Ok(credential)
    }
}

/// Stage native configuration without changing the active manager.
pub(super) fn prepare(
    process: &Process,
    host: &LinuxHost,
    pins: &CiPins,
    cpusets: &[String],
    out: &Path,
) -> Result<()> {
    let manager = host
        .gitlab
        .as_ref()
        .context("profile has no GitLab manager")?;
    ensure!(
        out.is_absolute() && out != manager.config_file,
        "stage GitLab configuration at an absolute path outside its active path"
    );
    match fs::symlink_metadata(&manager.config_file) {
        Ok(metadata) => ensure!(
            !metadata.file_type().is_symlink() || manager.config_file.try_exists()?,
            "active GitLab configuration is a dangling symbolic link"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("read active configuration metadata"),
    }
    let parent = out.parent().context("staged configuration has no parent")?;
    fs::create_dir_all(parent).context("create staged configuration directory")?;
    let destination = fs::canonicalize(parent)?.join(
        out.file_name()
            .context("staged configuration has no file name")?,
    );
    let active_parent = manager
        .config_file
        .parent()
        .context("active configuration has no parent")?;
    if active_parent.try_exists()? {
        ensure!(
            destination
                != fs::canonicalize(active_parent)?.join(
                    manager
                        .config_file
                        .file_name()
                        .context("active configuration has no file name")?
                ),
            "staged configuration aliases the active directory entry"
        );
    }
    if out.try_exists()? && manager.config_file.try_exists()? {
        ensure!(
            fs::canonicalize(out)? != fs::canonicalize(&manager.config_file)?,
            "staged configuration aliases the active file"
        );
    }
    let binary = manager
        .binary
        .to_str()
        .context("GitLab runner path is not UTF-8")?;
    process.require_tools(&[binary])?;
    let version = process.capture(binary, &["--version"], "read GitLab Runner version")?;
    ensure!(
        version.lines().any(|line| line
            .strip_prefix("Version:")
            .is_some_and(|value| value.trim() == pins.gitlab_runner_version)),
        "installed GitLab Runner differs from the reviewed pin"
    );
    write_secret(out, &configuration(host, pins, cpusets)?)?;
    let mut command = process.command(binary);
    command
        .args(["list", "--config"])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    process.run_command(&mut command, "validate native GitLab configuration")
}

/// Change future native admission through config reload, never by restarting jobs.
pub(super) fn install(
    process: &Process,
    host: &LinuxHost,
    pins: &CiPins,
    cpusets: &[String],
) -> Result<Vec<String>> {
    let manager = host
        .gitlab
        .as_ref()
        .context("profile has no GitLab manager")?;
    let before = read_config(&manager.config_file)?;
    let state = process.capture(
        "systemctl",
        &["show", &manager.service, "--property=LoadState", "--value"],
        "read native manager unit",
    )?;
    ensure!(
        matches!(state.as_str(), "loaded" | "not-found"),
        "native manager unit is not in an installable state"
    );
    if state == "loaded" {
        let command = process.capture(
            "systemctl",
            &["show", &manager.service, "--property=ExecStart", "--value"],
            "read native manager binding",
        )?;
        let argv = command
            .split_once("argv[]=")
            .and_then(|(_, value)| value.split(" ;").next())
            .context("native manager has no explicit command binding")?;
        let words = argv.split_whitespace().collect::<Vec<_>>();
        ensure!(
            words.first().copied() == manager.binary.to_str()
                && words
                    .windows(2)
                    .any(|pair| pair[0] == "--config"
                        && Some(pair[1]) == manager.config_file.to_str()),
            "active native manager uses a different executable or configuration"
        );
    }
    let candidate = manager.config_file.with_extension("candidate.toml");
    prepare(process, host, pins, cpusets, &candidate)?;
    let backup = manager.config_file.with_extension("toml.previous");
    super::services::install_worker_slices(process, host)?;
    let mut keep = Vec::new();
    for path in [&manager.config_file, &backup] {
        match fs::read_to_string(path) {
            Ok(text) => {
                let value: toml::Value = toml::from_str(&text)
                    .map_err(|_| anyhow!("invalid native rollback configuration"))?;
                if let Some(runners) = value.get("runners").and_then(toml::Value::as_array) {
                    for runner in runners {
                        if let Some(docker) = runner.get("docker") {
                            for name in ["image", "helper_image"] {
                                if let Some(image) = docker.get(name).and_then(toml::Value::as_str)
                                {
                                    keep.push(image.to_owned());
                                }
                            }
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read native rollback metadata"),
        }
    }
    for runner in host
        .runners
        .iter()
        .filter(|runner| matches!(runner.target, RunnerTarget::Gitlab { .. }))
    {
        write_kache(host, runner)?;
    }
    if state == "not-found" {
        fs::create_dir_all(host.cache_root.join("gitlab-manager"))?;
        fs::write(
            Path::new(consts::SERVICE_SYSTEMD_ROOT).join(&manager.service),
            unit(host)?,
        )?;
    }
    process.run(
        "systemctl",
        &["daemon-reload"],
        "load native manager resource prerequisites",
    )?;
    for runner in host
        .runners
        .iter()
        .filter(|r| matches!(r.target, RunnerTarget::Gitlab { .. }))
    {
        let parent = Container::cgroup_parent(runner);
        let max = process.capture(
            "systemctl",
            &["show", &parent, "--property=TasksMax", "--value"],
            "verify native worker PID budget",
        )?;
        ensure!(
            max == Container::PIDS_LIMIT.to_string(),
            "native worker aggregate PID budget is not installed"
        );
    }
    ensure!(
        read_config(&manager.config_file)? == before,
        "native configuration changed during preparation; candidate was not applied"
    );
    if let Some(previous) = before
        && previous != fs::read_to_string(&candidate)?
    {
        write_secret(&backup, &previous)?;
    }
    fs::rename(&candidate, &manager.config_file).context("publish native manager configuration")?;
    Ok(keep)
}

fn unit(host: &LinuxHost) -> Result<String> {
    let manager = host
        .gitlab
        .as_ref()
        .context("profile has no GitLab manager")?;
    let work = host.cache_root.join("gitlab-manager");
    Ok(format!(
        "[Unit]\nDescription=Kithara GitLab Docker workers\nAfter=docker.service network-online.target\nRequires=docker.service\nWants=network-online.target\n\n[Service]\nExecStart={} run --config {} --working-directory {}\nRestart=always\nRestartSec=10\nKillSignal=SIGQUIT\n\n[Install]\nWantedBy=multi-user.target\n",
        manager.binary.display(),
        manager.config_file.display(),
        work.display()
    ))
}

fn read_config(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("reading native manager configuration"),
    }
}

fn configuration(host: &LinuxHost, pins: &CiPins, cpusets: &[String]) -> Result<String> {
    let manager = host
        .gitlab
        .as_ref()
        .context("profile has no GitLab manager")?;
    ensure!(
        host.runners.len() == cpusets.len(),
        "CPU assignments do not cover the profile"
    );
    let mut ids = BTreeSet::new();
    let mut runners = Vec::new();
    for (runner, cpuset) in host.runners.iter().zip(cpusets) {
        let RunnerTarget::Gitlab { url, ca_file, .. } = &runner.target else {
            continue;
        };
        let credential = Credential::load(runner)?;
        let origin = reqwest::Url::parse(url)?;
        ensure!(
            ids.insert((origin.to_string(), credential.id)),
            "GitLab manager cannot configure one native runner twice"
        );
        let job = container(host, runner, cpuset.clone(), pins)?;
        let mut environment = Container::environment(host, runner);
        environment.push(format!(
            "KITHARA_CACHE_TRUST={}",
            runner.cache_trust.as_str()
        ));
        environment.extend(
            client_environment(&runner.sccache_s3_env_file)?
                .into_iter()
                .map(|(name, value)| format!("{name}={value}")),
        );
        let tmpfs = Container::tmpfs();
        let (temporary, options) = tmpfs
            .split_once(':')
            .context("invalid canonical tmpfs declaration")?;
        let mut config = json!({
            "id":credential.id,"name":runner.name,"url":url,"token":credential.token,
            "executor":"docker","limit":1,"request_concurrency":1,"use_slot_cgroups":false,
            "builds_dir":Container::WORKSPACE,"cache_dir":Container::CACHE,
            "environment":environment,
            "feature_flags":{"FF_DISABLE_UMASK_FOR_DOCKER_EXECUTOR":true},
            "docker":{
                "image":job.image,"helper_image":manager.helper_image,
                "user":format!("{id}:{id}",id=consts::JOB_USER),
                "cpuset_cpus":job.cpuset,"memory":job.memory,"memory_swap":job.memory,
                "cgroup_parent":job.cgroup_parent,"service_cgroup_parent":job.cgroup_parent,
                "network_mode":job.network,"privileged":false,
                "security_opt":["no-new-privileges"],
                "pull_policy":["never"],"allowed_pull_policies":["never"],
                "devices":job.devices.iter().map(|path|path.to_string_lossy()).collect::<Vec<_>>(),
                "group_add":job.groups.iter().map(u32::to_string).collect::<Vec<_>>(),
                "volumes":job.mounts.iter().map(|(source,target)|format!("{source}:{target}:rw")).collect::<Vec<_>>(),
                "tmpfs":{temporary:options},
                "ulimit":{"nice":format!("{limit}:{limit}",limit=Container::NICE_LIMIT)}
            }
        });
        if let Some(ca_file) = ca_file {
            config["tls-ca-file"] = json!(ca_file);
        }
        runners.push(toml::Value::try_from(config)?);
    }
    ensure!(!runners.is_empty(), "profile has no GitLab workers");
    let mut document: toml::Table = match read_config(&manager.config_file)? {
        Some(text) => toml::from_str(&text).map_err(|error: toml::de::Error| {
            anyhow!(
                "invalid installed GitLab configuration at {:?}",
                error.span()
            )
        })?,
        None => toml::Table::new(),
    };
    if let Some(existing) = document.get("runners") {
        for existing in existing
            .as_array()
            .context("installed runners must be an array")?
        {
            let url = existing
                .get("url")
                .and_then(toml::Value::as_str)
                .context("installed runner has no server")?;
            let id = existing
                .get("id")
                .and_then(toml::Value::as_integer)
                .context("installed runner has no identity")?;
            ensure!(
                id > 0
                    && ids.contains(&(reqwest::Url::parse(url)?.to_string(), u64::try_from(id)?)),
                "installed GitLab configuration contains workers outside this profile"
            );
            let current = existing
                .get("token")
                .and_then(toml::Value::as_str)
                .context("installed native runner has no authentication token")?;
            ensure!(
                !current.trim().is_empty(),
                "installed native runner token is empty"
            );
            let replacement = runners
                .iter_mut()
                .find(|candidate| {
                    candidate.get("id").and_then(toml::Value::as_integer) == Some(id)
                        && candidate
                            .get("url")
                            .and_then(toml::Value::as_str)
                            .is_some_and(|value| {
                                reqwest::Url::parse(value).ok() == reqwest::Url::parse(url).ok()
                            })
                })
                .context("installed native runner has no declared replacement")?
                .as_table_mut()
                .context("native runner must be a table")?;
            // The issued credential enrolls a worker; the native manager owns token rotation.
            for field in ["token", "token_obtained_at", "token_expires_at"] {
                if let Some(value) = existing.get(field) {
                    replacement.insert(field.to_owned(), value.clone());
                }
            }
        }
    }
    ensure!(
        runners.iter().all(|runner| runner
            .get("token")
            .and_then(toml::Value::as_str)
            .is_some_and(|token| !token.trim().is_empty())),
        "new native workers require an issued authentication token"
    );
    document.insert(
        "concurrent".to_owned(),
        toml::Value::Integer(i64::try_from(runners.len())?),
    );
    document.insert("runners".to_owned(), toml::Value::Array(runners));
    toml::to_string(&document).context("serialize native GitLab configuration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{
        config::fixture, host::linux::profile::tests::gitlab_fixture, process::Recording,
    };

    fn setup(directory: &Path) -> LinuxHost {
        let mut host = gitlab_fixture();
        host.cache_root = directory.join("cache");
        let manager = host.gitlab.as_mut().unwrap();
        manager.config_file = directory.join("installed.toml");
        let store = directory.join("review.env");
        write_secret(&store, "SCCACHE_BUCKET=kithara-review\nSCCACHE_ENDPOINT=http://cache:9000\nSCCACHE_REGION=us-east-1\nSCCACHE_S3_USE_SSL=false\nAWS_ACCESS_KEY_ID=key\nAWS_SECRET_ACCESS_KEY=secret\nAWS_EC2_METADATA_DISABLED=true\n").unwrap();
        for (index, runner) in host.runners.iter_mut().enumerate() {
            runner.sccache_s3_env_file = store.clone();
            let RunnerTarget::Gitlab {
                url,
                project,
                token_file,
                ..
            } = &mut runner.target
            else {
                panic!("GitLab fixture");
            };
            *token_file = directory.join(format!("runner-{index}.json"));
            write_secret(token_file, &json!({"url":url,"project":project,"id":index+1,"token":format!("glrt-fixture-{index}"),"labels":runner.labels,"protected":false}).to_string()).unwrap();
        }
        host
    }

    fn manager_process(directory: &Path, host: &LinuxHost, pins: &CiPins) -> Process {
        let manager = host.gitlab.as_ref().unwrap();
        let binary = manager.binary.to_str().unwrap();
        let command = format!(
            "{{ path={binary} ; argv[]={binary} run --config {} ; ignore_errors=no }}",
            manager.config_file.display()
        );
        Process::recording(
            directory,
            Recording::default()
                .with_reply(&["systemctl", "show"], "8192")
                .with_reply(
                    &[binary, "--version"],
                    &format!("Version: {}", pins.gitlab_runner_version),
                )
                .with_reply(
                    &[
                        "systemctl",
                        "show",
                        &manager.service,
                        "--property=LoadState",
                    ],
                    "loaded",
                )
                .with_reply(
                    &[
                        "systemctl",
                        "show",
                        &manager.service,
                        "--property=ExecStart",
                    ],
                    &command,
                ),
        )
    }

    #[test]
    fn staging_rejects_physical_active_aliases_and_preserves_installed_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let mut host = setup(directory.path());
        let pins = fixture().pins;
        let cpusets = ["0,1".into(), "2,3".into()];
        let installed = host.gitlab.as_ref().unwrap().config_file.clone();
        let before = configuration(&host, &pins, &cpusets).unwrap();
        write_secret(&installed, &before).unwrap();
        let metadata = fs::metadata(&installed).unwrap();
        host.runners[0].memory = "8g".to_owned();
        fs::create_dir(directory.path().join("subdir")).unwrap();
        let mut aliases = vec![directory.path().join("subdir/../installed.toml")];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(directory.path(), directory.path().join("alias")).unwrap();
            aliases.push(directory.path().join("alias/installed.toml"));
            std::os::unix::fs::symlink(&installed, directory.path().join("linked.toml")).unwrap();
            aliases.push(directory.path().join("linked.toml"));
        }
        for alias in aliases {
            let process = manager_process(directory.path(), &host, &pins);
            let result = prepare(&process, &host, &pins, &cpusets, &alias);
            assert_eq!(fs::read_to_string(&installed).unwrap(), before);
            assert!(
                result.is_err(),
                "active alias was accepted: {}",
                alias.display()
            );
            let after = fs::metadata(&installed).unwrap();
            assert_eq!(metadata.modified().unwrap(), after.modified().unwrap());
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(
                    (
                        metadata.ino(),
                        metadata.mode(),
                        metadata.ctime(),
                        metadata.ctime_nsec()
                    ),
                    (after.ino(), after.mode(), after.ctime(), after.ctime_nsec())
                );
            }
        }
        let candidate = directory.path().join("new/candidate.toml");
        let process = manager_process(directory.path(), &host, &pins);
        prepare(&process, &host, &pins, &cpusets, &candidate).unwrap();
        assert_ne!(fs::read_to_string(candidate).unwrap(), before);
        assert_eq!(fs::read_to_string(installed).unwrap(), before);
    }

    #[test]
    fn missing_worker_pid_budget_cannot_publish_native_admission() {
        let directory = tempfile::tempdir().unwrap();
        let host = setup(directory.path());
        let pins = fixture().pins;
        let active = &host.gitlab.as_ref().unwrap().config_file;
        fs::write(
            active,
            configuration(&host, &pins, &["0,1".into(), "2,3".into()]).unwrap(),
        )
        .unwrap();
        let before = fs::read(active).unwrap();
        let backup = active.with_extension("toml.previous");
        let sentinel = "concurrent = 19\n";
        fs::write(&backup, sentinel).unwrap();
        let parent = Container::cgroup_parent(&host.runners[0]);
        let recording = manager_process(directory.path(), &host, &pins)
            .recorded()
            .unwrap()
            .with_reply(&["systemctl", "show", &parent], "0");
        let error = install(
            &Process::recording(directory.path(), recording),
            &host,
            &pins,
            &["2,3".into(), "0,1".into()],
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "native worker aggregate PID budget is not installed"
        );
        assert_eq!(fs::read(active).unwrap(), before);
        assert_eq!(fs::read(backup).unwrap(), sentinel.as_bytes());
        assert_ne!(
            fs::read(active.with_extension("candidate.toml")).unwrap(),
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_rejects_a_dangling_active_link_without_materializing_its_target() {
        use std::os::unix::fs::MetadataExt as _;

        let directory = tempfile::tempdir().unwrap();
        let host = setup(directory.path());
        let pins = fixture().pins;
        let installed = &host.gitlab.as_ref().unwrap().config_file;
        let target = directory.path().join("future/active.toml");
        std::os::unix::fs::symlink(&target, installed).unwrap();
        assert!(!target.exists());
        let before = fs::symlink_metadata(installed).unwrap();
        let process = manager_process(directory.path(), &host, &pins);
        let result = prepare(
            &process,
            &host,
            &pins,
            &["0,1".into(), "2,3".into()],
            &target,
        );
        assert!(
            !target.exists(),
            "staging materialized the active link's target"
        );
        assert!(result.is_err());
        assert_eq!(fs::read_link(installed).unwrap(), target);
        let after = fs::symlink_metadata(installed).unwrap();
        assert_eq!(
            (
                before.ino(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.ino(),
                after.mode(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
    }

    #[test]
    fn cache_preparation_failure_preserves_active_configuration_and_backup() {
        let directory = tempfile::tempdir().unwrap();
        let mut host = setup(directory.path());
        let pins = fixture().pins;
        let cpusets = ["0,1".into(), "2,3".into()];
        let installed = host.gitlab.as_ref().unwrap().config_file.clone();
        let backup = installed.with_extension("toml.previous");
        let before = configuration(&host, &pins, &cpusets).unwrap();
        write_secret(&installed, &before).unwrap();
        write_secret(&backup, &before).unwrap();
        host.runners[0].memory = "8g".to_owned();
        let blocked = Container::kache(&host, &host.runners[0]);
        fs::create_dir_all(blocked.parent().unwrap()).unwrap();
        fs::write(&blocked, "not a directory").unwrap();
        let process = manager_process(directory.path(), &host, &pins);
        let result = install(&process, &host, &pins, &cpusets);
        assert_eq!(fs::read_to_string(installed).unwrap(), before);
        assert_eq!(fs::read_to_string(backup).unwrap(), before);
        assert!(result.is_err());
        assert!(
            !process
                .recorded()
                .unwrap()
                .steps()
                .iter()
                .any(|step| step.args == ["daemon-reload"])
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_reload_observes_ready_cache_before_configuration_publication() {
        use std::ffi::OsString;

        use crate::testing::{install_script, install_scripted_double};

        let directory = tempfile::tempdir().unwrap();
        let mut host = setup(directory.path());
        let pins = fixture().pins;
        let cpusets = ["0,1".into(), "2,3".into()];
        let bin = directory.path().join("bin");
        let binary = install_scripted_double(
            &bin,
            "gitlab-runner",
            &format!(
                "case \"$1\" in --version) printf 'Version: {}\\n';; list) exit 0;; *) exit 21;; esac\n",
                pins.gitlab_runner_version
            ),
        );
        host.gitlab.as_mut().unwrap().binary = binary;
        install_script(
            &bin.join("systemctl"),
            r#"#!/bin/sh
test "$3" != --property=TasksMax || { printf '8192\n'; exit 0; }
case "$1 $2" in
  'show gitlab-runner.service')
    case "$3" in
      --property=LoadState) printf 'loaded\n';;
      --property=ExecStart) printf '{ argv[]=%s run --config %s ; }\n' "$RUNNER_BINARY" "$ACTIVE_CONFIG";;
      *) exit 22;;
    esac;;
  'daemon-reload ')
    test -f "$CACHE_CONFIG" || exit 23
    cmp "$ACTIVE_CONFIG" "$BEFORE_CONFIG" || exit 24
    printf 'ready\n' > "$RELOAD_PROOF";;
  *) exit 25;;
esac
"#,
        );
        install_script(&bin.join("install"), "#!/bin/sh\nexit 0\n");
        let manager = host.gitlab.as_ref().unwrap();
        assert!(
            !Container::kache(&host, &host.runners[0])
                .join(consts::KACHE_HOST_FILE)
                .exists()
        );
        let before = configuration(&host, &pins, &cpusets).unwrap();
        let snapshot = directory.path().join("before.toml");
        let proof = directory.path().join("reload-proof");
        write_secret(&snapshot, &before).unwrap();
        write_secret(&manager.config_file, &before).unwrap();
        let vars = std::collections::BTreeMap::from([
            (
                OsString::from("PATH"),
                std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                    .unwrap(),
            ),
            (
                OsString::from("RUNNER_BINARY"),
                manager.binary.as_os_str().to_owned(),
            ),
            (
                OsString::from("ACTIVE_CONFIG"),
                manager.config_file.as_os_str().to_owned(),
            ),
            (OsString::from("BEFORE_CONFIG"), snapshot.into_os_string()),
            (
                OsString::from("CACHE_CONFIG"),
                Container::kache(&host, &host.runners[0])
                    .join(consts::KACHE_HOST_FILE)
                    .into_os_string(),
            ),
            (
                OsString::from("RELOAD_PROOF"),
                proof.clone().into_os_string(),
            ),
        ]);
        host.runners[0].memory = "8g".to_owned();
        install(
            &Process::new(directory.path(), vars),
            &host,
            &pins,
            &cpusets,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(proof).unwrap(), "ready\n");
        assert_ne!(
            fs::read_to_string(&host.gitlab.as_ref().unwrap().config_file).unwrap(),
            before
        );
    }

    #[test]
    fn native_config_preserves_resources_scopes_and_correct_nice_serialization() {
        let directory = tempfile::tempdir().unwrap();
        let host = setup(directory.path());
        let pins = fixture().pins;
        let config: toml::Value =
            toml::from_str(&configuration(&host, &pins, &["0,1".into(), "2,3".into()]).unwrap())
                .unwrap();
        assert_eq!(config["concurrent"].as_integer(), Some(2));
        for (index, runner) in config["runners"].as_array().unwrap().iter().enumerate() {
            let docker = &runner["docker"];
            assert!(
                docker.get("pids_limit").is_none(),
                "native Runner has no pids_limit mapping"
            );
            assert_eq!(docker["cgroup_parent"], docker["service_cgroup_parent"]);
            assert_eq!(
                docker["cgroup_parent"].as_str(),
                Some(Container::cgroup_parent(&host.runners[index]).as_str())
            );
            assert_eq!(runner["use_slot_cgroups"].as_bool(), Some(false));
            assert_eq!(docker["ulimit"]["nice"].as_str(), Some("40:40"));
            assert!(docker["ulimit"].get("nice=40").is_none());
            assert_eq!(docker["memory"].as_str(), Some("4g"));
            assert_eq!(
                docker["cpuset_cpus"].as_str(),
                Some(if index == 0 { "0,1" } else { "2,3" })
            );
            assert_eq!(docker["privileged"].as_bool(), Some(false));
            assert_eq!(docker["user"].as_str(), Some("1000:1000"));
            assert_eq!(
                runner["feature_flags"]["FF_DISABLE_UMASK_FOR_DOCKER_EXECUTOR"].as_bool(),
                Some(true)
            );
            assert_eq!(
                docker["image"].as_str(),
                Some(
                    super::super::container::image(&host.runners[index], &pins)
                        .unwrap()
                        .as_str()
                )
            );
            let mounts = docker["volumes"].as_array().unwrap();
            assert!(
                mounts
                    .iter()
                    .all(|mount| !mount.as_str().unwrap().contains("docker.sock"))
            );
            assert!(
                mounts
                    .iter()
                    .any(|mount| mount.as_str().unwrap().ends_with(":/cache/target:rw"))
            );
            let env = runner["environment"].as_array().unwrap();
            assert!(
                env.iter()
                    .any(|value| value.as_str() == Some("KITHARA_CACHE_TRUST=review"))
            );
            assert!(
                env.iter()
                    .all(|value| !value.as_str().unwrap().contains("ACTIONS_RUNNER_JITCONFIG"))
            );
        }
        assert_ne!(
            Container::build_root(&host, &host.runners[0]),
            Container::build_root(&host, &host.runners[1])
        );
    }

    #[test]
    fn native_preparation_requires_no_github_credentials_or_api_commands() {
        let directory = tempfile::tempdir().unwrap();
        let host = setup(directory.path());
        let pins = fixture().pins;
        let binary = host.gitlab.as_ref().unwrap().binary.to_str().unwrap();
        let process = Process::recording(
            directory.path(),
            Recording::default().with_reply(
                &[binary, "--version"],
                &format!("Version: {}", pins.gitlab_runner_version),
            ),
        );
        let out = directory.path().join("candidate.toml");
        prepare(&process, &host, &pins, &["0,1".into(), "2,3".into()], &out).unwrap();
        let steps = process.recorded().unwrap();
        assert!(
            steps
                .steps()
                .iter()
                .all(|step| step.program == binary || step.program.starts_with("<require "))
        );
        assert!(!out.to_string_lossy().contains("github"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(out).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn mismatched_project_and_foreign_installed_workers_fail_before_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let host = setup(directory.path());
        let RunnerTarget::Gitlab {
            token_file: token, ..
        } = &host.runners[0].target
        else {
            panic!("GitLab fixture");
        };
        write_secret(
            token,
            &json!({"url":"https://gitlab.example","project":99,"id":1,"token":"glrt-fixture","labels":["kithara-linux"],"protected":false})
                .to_string(),
        )
        .unwrap();
        assert!(configuration(&host, &fixture().pins, &["0,1".into(), "2,3".into()]).is_err());
        let host = setup(directory.path());
        let installed = &host.gitlab.as_ref().unwrap().config_file;
        fs::write(
            installed,
            "[[runners]]\nurl='https://gitlab.example'\nid=99\n",
        )
        .unwrap();
        assert!(configuration(&host, &fixture().pins, &["0,1".into(), "2,3".into()]).is_err());
        assert_eq!(
            fs::read_to_string(installed).unwrap(),
            "[[runners]]\nurl='https://gitlab.example'\nid=99\n"
        );
    }

    #[test]
    fn native_rotation_and_manager_globals_survive_reconfiguration_without_restart() {
        let directory = tempfile::tempdir().unwrap();
        let host = setup(directory.path());
        let pins = fixture().pins;
        let cpusets = ["0,1".into(), "2,3".into()];
        let manager = host.gitlab.as_ref().unwrap();
        let mut initial: toml::Value =
            toml::from_str(&configuration(&host, &pins, &cpusets).unwrap()).unwrap();
        initial["runners"][0]["token"] = "glrt-rotated".into();
        initial
            .as_table_mut()
            .unwrap()
            .insert("shutdown_timeout".to_owned(), 7.into());
        write_secret(&manager.config_file, &toml::to_string(&initial).unwrap()).unwrap();
        let process = manager_process(directory.path(), &host, &pins);
        install(&process, &host, &pins, &cpusets).unwrap();
        let result: toml::Value =
            toml::from_str(&fs::read_to_string(&manager.config_file).unwrap()).unwrap();
        assert_eq!(result["runners"][0]["token"].as_str(), Some("glrt-rotated"));
        assert_eq!(result["shutdown_timeout"].as_integer(), Some(7));
        for runner in &host.runners {
            assert!(
                Container::kache(&host, runner)
                    .join(consts::KACHE_HOST_FILE)
                    .is_file()
            );
        }
        let steps = process.recorded().unwrap();
        assert!(
            steps
                .steps()
                .iter()
                .any(|step| step.program == "systemctl" && step.args == ["daemon-reload"])
        );
        assert!(steps.steps().iter().all(|step| {
            step.args
                .iter()
                .all(|arg| !matches!(arg.as_str(), "restart" | "stop" | "kill"))
        }));
    }

    #[test]
    fn unprotected_native_credentials_cannot_publish_to_trusted_cache() {
        let directory = tempfile::tempdir().unwrap();
        let mut host = setup(directory.path());
        for runner in &mut host.runners {
            runner.cache_trust = crate::ci::environment::CacheTrust::Trusted;
        }
        assert!(configuration(&host, &fixture().pins, &["0,1".into(), "2,3".into()]).is_err());
    }
}
