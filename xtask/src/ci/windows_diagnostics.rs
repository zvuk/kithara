use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use kithara_devtools::Ctx;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::process::Process;
use crate::consts;

pub(super) fn run(output: &Path, ctx: &Ctx) -> Result<()> {
    let process = Process::new(&ctx.root, BTreeMap::new());
    process.require_os(&["windows"], "native Windows diagnostics")?;
    let target = ctx.metadata()?.target_directory.as_std_path();
    let files = collect(&env::temp_dir(), target, output)?;
    let mut query = process.command("powershell.exe");
    query.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        consts::WINDOWS_DIAGNOSTIC_SYSTEM,
    ]);
    let system: Value = serde_json::from_str(&process.capture_bounded_command(
        &mut query,
        "read native Windows counters",
        consts::WINDOWS_DIAGNOSTIC_TIMEOUT,
        consts::WINDOWS_DIAGNOSTIC_OUTPUT_BYTES,
    )?)?;
    let retained = match env::var_os("GITHUB_WORKSPACE").map(PathBuf::from) {
        Some(root) if root.join(".git").exists() => {
            let mut git = process.command(ctx.config.tools.program("git"));
            git.current_dir(root)
                .args(["rev-parse", "--verify", "HEAD"])
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE");
            Some(
                process
                    .capture_command(&mut git, "identify retained workspace")?
                    .trim()
                    .to_owned(),
            )
        }
        _ => None,
    };
    let manifest = json!({
        "scope": "diagnostic-only",
        "definition_sha": env::var_os("GITHUB_SHA").map(|value| value.to_string_lossy().into_owned()),
        "retained_workspace_sha": retained,
        "captured_unix_seconds": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        "target": target,
        "system": system,
        "files": files,
    });
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn ordinary(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & consts::WINDOWS_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.file_type().is_symlink()
    }
}

fn entries(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.try_exists()? {
        return Ok(Vec::new());
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || !ordinary(&metadata) {
        return Ok(Vec::new());
    }
    fs::read_dir(root)?.map(|entry| Ok(entry?.path())).collect()
}

fn copy_report(source: &Path, output: &Path, name: &Path) -> Result<Option<Value>> {
    if !source.try_exists()? {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() || !ordinary(&metadata) {
        return Ok(None);
    }
    let destination = output.join(name);
    fs::create_dir_all(destination.parent().context("report has no parent")?)?;
    fs::copy(source, &destination)
        .with_context(|| format!("preserve Windows evidence {}", source.display()))?;
    let bytes = fs::read(&destination)?;
    Ok(Some(json!({
        "source": source,
        "artifact": name,
        "bytes": bytes.len(),
        "modified_unix_seconds": metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs(),
        "sha256": hex::encode(Sha256::digest(&bytes)),
    })))
}

fn collect(temp: &Path, target: &Path, output: &Path) -> Result<Vec<Value>> {
    fs::create_dir_all(output.parent().context("diagnostic output has no parent")?)?;
    fs::create_dir(output).context("diagnostic output must be a new directory")?;
    let mut reports = Vec::new();
    for path in entries(temp)? {
        let metadata = fs::symlink_metadata(&path)?;
        let name = path
            .file_name()
            .context("report path has no name")?
            .to_string_lossy();
        if metadata.is_file()
            && ordinary(&metadata)
            && name.starts_with("kithara-hang-")
            && name.ends_with(".json")
        {
            reports.push((metadata.modified()?, path));
        }
    }
    reports.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut files = Vec::new();
    for (_, path) in reports.into_iter().take(consts::WINDOWS_DIAGNOSTIC_REPORTS) {
        let name = Path::new("previous/hang").join(path.file_name().context("report name")?);
        if let Some(file) = copy_report(&path, output, &name)? {
            files.push(file);
        }
    }
    for profile in entries(&target.join("nextest"))? {
        let metadata = fs::symlink_metadata(&profile)?;
        if !metadata.is_dir() || !ordinary(&metadata) {
            continue;
        }
        let name = Path::new("previous/junit")
            .join(profile.file_name().context("profile name")?)
            .join("junit.xml");
        if let Some(file) = copy_report(&profile.join("junit.xml"), output, &name)? {
            files.push(file);
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_bounded_hashed_and_preserves_its_sources() {
        let root = tempfile::tempdir().unwrap();
        let temp = root.path().join("temp");
        let target = root.path().join("target");
        let output = root.path().join("out");
        fs::create_dir_all(&temp).unwrap();
        for index in 0..consts::WINDOWS_DIAGNOSTIC_REPORTS + 2 {
            fs::write(temp.join(format!("kithara-hang-{index}.json")), b"{}").unwrap();
        }
        fs::write(temp.join("credentials.json"), b"secret").unwrap();
        let junit = target.join("nextest/ci/junit.xml");
        fs::create_dir_all(junit.parent().unwrap()).unwrap();
        fs::write(&junit, b"<testsuites/>").unwrap();
        let files = collect(&temp, &target, &output).unwrap();
        assert_eq!(files.len(), consts::WINDOWS_DIAGNOSTIC_REPORTS + 1);
        assert!(junit.is_file());
        assert!(
            files
                .iter()
                .all(|file| Path::new(file["source"].as_str().unwrap()).is_file())
        );
        assert!(
            files
                .iter()
                .all(|file| file["sha256"].as_str().unwrap().len() == 64)
        );
        assert!(!output.join("credentials.json").exists());
        assert!(!output.join("previous/hang/credentials.json").exists());
        assert!(collect(&temp, &target, &output).is_err());
        assert_eq!(fs::read(&junit).unwrap(), b"<testsuites/>");
    }

    #[cfg(unix)]
    #[test]
    fn capture_does_not_follow_report_or_profile_links() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("private");
        let temp = root.path().join("temp");
        let target = root.path().join("target");
        fs::create_dir_all(&secret).unwrap();
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(target.join("nextest")).unwrap();
        fs::write(secret.join("junit.xml"), b"private").unwrap();
        symlink(
            secret.join("junit.xml"),
            temp.join("kithara-hang-link.json"),
        )
        .unwrap();
        symlink(&secret, target.join("nextest/linked")).unwrap();
        assert!(
            collect(&temp, &target, &root.path().join("out"))
                .unwrap()
                .is_empty()
        );
    }
}
