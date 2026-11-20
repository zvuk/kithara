use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use serde_json::Value;
use url::{Url, form_urlencoded::byte_serialize};

use super::{
    super::{
        Deployment, Request,
        command::{Preflight, sha256},
    },
    macos,
};
use crate::Ctx;

#[derive(Debug, Args)]
pub(in crate::perf::bench) struct DispatchArgs {
    #[arg(value_enum)]
    host: Host,
    #[arg(long = "bench-preflight", value_name = "FILE")]
    preflight: Option<PathBuf>,
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
        args.preflight.is_none() || matches!(args.host, Host::Linux),
        "benchmark preflight dispatch requires Linux"
    );
    let input = args
        .preflight
        .as_ref()
        .map(fs::read_to_string)
        .transpose()?;
    if let Some(raw) = &input {
        Preflight::parse(&args.request, raw, false)?;
        println!("preflight declaration SHA-256 {}", sha256(raw.as_bytes()));
    }
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
        Host::Linux => {
            let args = linux_arguments(&repository, &controller, &request, input.as_deref());
            let args: Vec<_> = args.iter().map(String::as_str).collect();
            print!("{}", output(ctx, "gh", &args, None)?);
            println!("https://github.com/{repository}/actions/workflows/dispatch.yml");
            Ok(())
        }
        Host::Macos => macos::run(ctx, &config, &request),
    }
}

fn linux_arguments(
    repository: &str,
    controller: &str,
    request: &Request,
    input: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<_> = [
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
    ]
    .map(str::to_owned)
    .into();
    for (key, value) in request.parameters() {
        args.extend(["-f".into(), format!("{}={value}", key.to_ascii_lowercase())]);
    }
    if let Some(raw) = input {
        args.extend(["-f".into(), format!("bench_preflight={raw}")]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_linux_dispatch_preserves_payload_and_declaration_bytes() {
        let request = Request {
            target: "p:b:f".into(),
            head: "head".into(),
            base: "base".into(),
            ..Request::default()
        };
        let default = linux_arguments("owner/repo", "controller", &request, None);
        assert_eq!(
            default,
            [
                "workflow",
                "run",
                "dispatch.yml",
                "--repo",
                "owner/repo",
                "--ref",
                "controller",
                "-f",
                "kind=nightly",
                "-f",
                "only=deep-bench",
                "-f",
                "bench_target=p:b:f",
                "-f",
                "bench_filter=",
                "-f",
                "bench_ref=head",
                "-f",
                "bench_base_ref=base"
            ]
        );
        let raw = "{\"collections\":{}}\n";
        let opts = linux_arguments("owner/repo", "controller", &request, Some(raw));
        assert_eq!(&opts[..default.len()], default);
        assert_eq!(
            &opts[default.len()..],
            ["-f", "bench_preflight={\"collections\":{}}\n"]
        );
    }

    #[test]
    fn bench_mac_preflight_is_refused_before_loading_or_running_anything() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("Cargo.toml");
        fs::write(&manifest, "[workspace]\n").unwrap();
        let ctx = Ctx::load_from_manifest(&manifest).unwrap();
        let args = DispatchArgs {
            host: Host::Macos,
            preflight: Some(root.path().join("absent.json")),
            request: Request::default(),
        };
        let error = run(&args, &ctx).unwrap_err();
        assert_eq!(
            error.to_string(),
            "benchmark preflight dispatch requires Linux"
        );
    }

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
