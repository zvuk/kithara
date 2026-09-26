use std::{fs, path::Path, process};

use anyhow::{Context, Result, bail};
use reqwest::{
    blocking::Client,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT},
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::profile::{LinuxHost, LinuxRunner, WindowsGuest};
use crate::consts;

#[derive(Deserialize)]
struct Registration {
    id: u64,
    name: String,
    status: String,
    busy: bool,
}

#[derive(Deserialize)]
struct Listing {
    runners: Vec<Registration>,
}

#[derive(Deserialize)]
struct JitConfig {
    encoded_jit_config: String,
}

#[derive(Deserialize)]
struct RegistrationToken {
    token: String,
}

#[derive(Serialize)]
struct JitRequest<'a> {
    name: String,
    runner_group_id: u32,
    labels: &'a [String],
    work_folder: &'a str,
}

/// Mint one just-in-time configuration and leave it where the runner's service
/// will read it.
///
/// A just-in-time configuration carries its own credentials, is accepted once,
/// and expires with the job it serves. The token that mints it stays on the
/// host: only the generated configuration reaches the container.
pub(super) fn configure(host: &LinuxHost, runner: &LinuxRunner, env_file: &Path) -> Result<()> {
    let credential = host.credential(&runner.repository)?;
    let token = read_token(&credential.token_file)?;
    let client = client(&token)?;
    let endpoint = format!(
        "https://api.github.com/repos/{}/actions/runners",
        credential.name
    );

    prune_offline(&client, &endpoint, &runner.name)?;

    let request = JitRequest {
        // A name is claimed until its runner is removed, and an ephemeral
        // runner is removed only after it has served a job. The process id
        // keeps a restart from colliding with the registration it replaces.
        name: format!("{}-{}", runner.name, process::id()),
        runner_group_id: 1,
        labels: &runner.labels,
        work_folder: "_work",
    };
    let response = client
        .post(format!("{endpoint}/generate-jitconfig"))
        .json(&request)
        .send()
        .context("requesting a just-in-time runner configuration")?;
    if !response.status().is_success() {
        bail!(
            "GitHub refused a runner configuration for {}: {}",
            runner.name,
            response.status()
        );
    }
    let config: JitConfig = response
        .json()
        .context("reading the just-in-time runner configuration")?;

    write_secret(
        env_file,
        &format!("ACTIONS_RUNNER_JITCONFIG={}\n", config.encoded_jit_config),
    )?;
    info!(runner = runner.name, "runner configuration written");
    Ok(())
}

/// Mint the token a machine registers itself with, once.
///
/// A container is handed a configuration that serves one job, because a
/// container is built again for the next one. A guest is installed once and
/// kept, so it enrols the way a physical machine does: it registers itself and
/// holds that registration across the jobs and the restarts that follow. The
/// token minted here is what it registers with, and it expires within the hour
/// whether or not it was used.
pub(super) fn enrolment_token(host: &LinuxHost, guest: &WindowsGuest) -> Result<String> {
    let credential = host.credential(&guest.repository)?;
    let token = read_token(&credential.token_file)?;
    let client = client(&token)?;
    let response = client
        .post(format!(
            "https://api.github.com/repos/{}/actions/runners/registration-token",
            credential.name
        ))
        .send()
        .context("requesting a runner registration token")?;
    if !response.status().is_success() {
        bail!("GitHub refused a registration token: {}", response.status());
    }
    let minted: RegistrationToken = response
        .json()
        .context("reading the runner registration token")?;
    Ok(minted.token)
}

/// Whether a runner of this name is registered and waiting for work. This is
/// the only answer that distinguishes a guest that enrolled from one that
/// booted and did nothing.
pub(super) fn is_online(host: &LinuxHost, guest: &WindowsGuest) -> Result<bool> {
    let credential = host.credential(&guest.repository)?;
    let token = read_token(&credential.token_file)?;
    let client = client(&token)?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{}/actions/runners",
            credential.name
        ))
        .send()
        .context("listing the repository's runners")?;
    if !response.status().is_success() {
        bail!("GitHub refused to list runners: {}", response.status());
    }
    let listing: Listing = response.json().context("reading the runner listing")?;
    Ok(listing
        .runners
        .iter()
        .any(|runner| runner.name == guest.name && runner.status == "online"))
}

/// Drop registrations left behind by runners that never got a job. An ephemeral
/// runner removes itself once it has served one, but a runner that is restarted
/// or killed first leaves its name registered forever.
fn prune_offline(client: &Client, endpoint: &str, prefix: &str) -> Result<()> {
    let response = client
        .get(endpoint)
        .send()
        .context("listing the repository's runners")?;
    if !response.status().is_success() {
        bail!("GitHub refused to list runners: {}", response.status());
    }
    let listing: Listing = response.json().context("reading the runner listing")?;
    for stale in listing
        .runners
        .iter()
        .filter(|runner| is_prunable(runner, prefix))
    {
        let removed = client
            .delete(format!("{endpoint}/{}", stale.id))
            .send()
            .with_context(|| format!("removing the stale runner {}", stale.name))?;
        if !removed.status().is_success() {
            bail!(
                "GitHub refused to remove the stale runner {}: {}",
                stale.name,
                removed.status()
            );
        }
        info!(runner = stale.name, "stale registration removed");
    }
    Ok(())
}

fn is_prunable(runner: &Registration, prefix: &str) -> bool {
    runner.status == "offline" && !runner.busy && runner.name.starts_with(prefix)
}

fn client(token: &str) -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static("kithara-ci"));
    headers.insert(consts::API_VERSION, HeaderValue::from_static("2022-11-28"));
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .context("the runner token is not a usable header value")?;
    authorization.set_sensitive(true);
    headers.insert(AUTHORIZATION, authorization);
    Client::builder()
        .default_headers(headers)
        .build()
        .context("building the GitHub client")
}

fn read_token(path: &Path) -> Result<String> {
    let token = fs::read_to_string(path)
        .with_context(|| format!("reading the runner token {}", path.display()))?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        bail!("the runner token {} is empty", path.display());
    }
    Ok(token)
}

/// Credentials reach disk unreadable to anyone but their owner, and never pass
/// through a command line where every process on the machine could read them.
fn write_secret(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    super::permissions::set_mode(path, consts::OWNER_ONLY)
}

#[cfg(test)]
mod tests {
    use super::{Registration, is_prunable};

    fn runner(status: &str, busy: bool) -> Registration {
        Registration {
            id: 1,
            name: "gerasim13-04-old".to_owned(),
            status: status.to_owned(),
            busy,
        }
    }

    #[test]
    fn a_busy_offline_runner_does_not_block_a_new_jit_registration() {
        assert!(!is_prunable(&runner("offline", true), "gerasim13-04"));
        assert!(is_prunable(&runner("offline", false), "gerasim13-04"));
        assert!(!is_prunable(&runner("online", false), "gerasim13-04"));
    }
}
