use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use serde_json::Value;
use url::{Url, form_urlencoded::byte_serialize};

use super::{
    super::{Deployment, Request},
    macos,
};
use crate::Ctx;

#[derive(Debug, Args)]
pub(in crate::perf::bench) struct DispatchArgs {
    #[arg(value_enum)]
    host: Host,
    #[command(flatten)]
    request: Request,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Host {
    Linux,
    Macos,
}

pub(super) fn output(
    ctx: &Ctx,
    program: &str,
    args: &[&str],
    body: Option<&Value>,
) -> Result<String> {
    let mut command = Command::new(ctx.config.tools.program(program));
    command
        .current_dir(&ctx.root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if body.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("start {program}"))?;
    if let Some(body) = body {
        child
            .stdin
            .take()
            .context("CI API input")?
            .write_all(&serde_json::to_vec(body)?)?;
    }
    let result = child.wait_with_output()?;
    ensure!(
        result.status.success(),
        "{program} request failed: {}",
        result.status
    );
    String::from_utf8(result.stdout).context("CI response is not UTF-8")
}

pub(super) fn gh(ctx: &Ctx, path: &str) -> Result<Value> {
    Ok(serde_json::from_str(&output(
        ctx,
        "gh",
        &["api", path],
        None,
    )?)?)
}

pub(super) fn encoded(value: &str) -> String {
    byte_serialize(value.as_bytes()).collect()
}

fn github_repository(remote: &str) -> Result<String> {
    let normalized = remote.strip_prefix("git@github.com:").map_or_else(
        || remote.to_owned(),
        |path| format!("https://github.com/{path}"),
    );
    let url = Url::parse(&normalized)?;
    ensure!(
        matches!(url.scheme(), "https" | "ssh")
            && matches!(url.host_str(), Some("github.com" | "ssh.github.com"))
            && url.password().is_none(),
        "benchmark dispatch needs a GitHub origin remote"
    );
    Ok(url
        .path()
        .trim_start_matches('/')
        .trim_end_matches(".git")
        .to_owned())
}

pub(super) fn git(ctx: &Ctx, args: &[&str]) -> Result<String> {
    Ok(output(ctx, "git", args, None)?.trim().to_owned())
}

fn resolve(ctx: &Ctx, repository: &str, source: &str) -> Result<String> {
    gh(
        ctx,
        &format!("repos/{repository}/commits/{}", encoded(source)),
    )?["sha"]
        .as_str()
        .map(str::to_owned)
        .context("GitHub did not resolve benchmark ref")
}

pub(in crate::perf::bench) fn run(args: &DispatchArgs, ctx: &Ctx) -> Result<()> {
    ensure!(
        !args.request.target.is_empty(),
        "dispatch requires --bench-target"
    );
    let config = Deployment::load(ctx)?;
    let repository = github_repository(&git(ctx, &["remote", "get-url", "origin"])?)?;
    let controller = git(ctx, &["branch", "--show-current"])?;
    ensure!(
        !controller.is_empty(),
        "dispatch requires a published controller branch checkout"
    );
    let head_ref = if args.request.head.is_empty() {
        &controller
    } else {
        &args.request.head
    };
    let head = resolve(ctx, &repository, head_ref)?;
    let base = if args.request.base.is_empty() {
        String::new()
    } else {
        resolve(ctx, &config.github_repo, &args.request.base)?
    };
    println!(
        "Benchmark source {repository}:{head_ref} -> {head}; base {}:{} -> {base}",
        config.github_repo, args.request.base
    );
    let request = Request {
        base,
        head,
        ..args.request.clone()
    };
    match args.host {
        Host::Linux => linux(ctx, &repository, &controller, &request),
        Host::Macos => macos::run(ctx, &config, &request),
    }
}

fn linux(ctx: &Ctx, repository: &str, controller: &str, request: &Request) -> Result<()> {
    let values: Vec<_> = request
        .parameters()
        .into_iter()
        .map(|(key, value)| format!("{}={value}", key.to_ascii_lowercase()))
        .collect();
    let mut args = vec![
        "workflow",
        "run",
        "dispatch.yml",
        "--repo",
        repository,
        "--ref",
        controller,
        "-f",
        "kind=nightly",
        "-f",
        "only=deep-bench",
    ];
    for value in &values {
        args.extend(["-f", value]);
    }
    print!("{}", output(ctx, "gh", &args, None)?);
    println!("https://github.com/{repository}/actions/workflows/dispatch.yml");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_source_repository_comes_from_existing_git_remotes() {
        for remote in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@ssh.github.com:443/owner/repo.git",
        ] {
            assert_eq!(github_repository(remote).unwrap(), "owner/repo");
        }
        assert!(github_repository("https://gitlab.example/owner/repo").is_err());
    }
}
