use std::{
    fs,
    io::Write as _,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{runner::Session, sha256};
use crate::child;

struct Manifest {
    root: PathBuf,
    files: Vec<File>,
}

struct File {
    path: PathBuf,
    sha256: String,
    bytes: u64,
}

pub(super) fn directory(
    session: &Session,
    root: &Path,
    name: &str,
    cancel: &child::Cancel,
) -> Result<()> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    transfer(
        session,
        &Manifest {
            root: root.to_owned(),
            files,
        },
        name,
        cancel,
    )
}

fn collect(root: &Path, directory: &Path, files: &mut Vec<File>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, files)?;
        } else if kind.is_file() {
            files.push(File {
                path: path.strip_prefix(root)?.to_owned(),
                sha256: sha256(&path)?,
                bytes: entry.metadata()?.len(),
            });
        } else {
            bail!(
                "fixture staging requires ordinary files: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn transfer(
    session: &Session,
    manifest: &Manifest,
    name: &str,
    cancel: &child::Cancel,
) -> Result<()> {
    if manifest.files.is_empty() {
        bail!("{name} manifest is empty");
    }
    if !manifest.root.is_absolute() {
        bail!("fixture manifest root must be absolute");
    }
    let evidence = session
        .evidence
        .parent()
        .context("native evidence directory")?;
    let list = evidence.join(format!("{name}.files"));
    let mut paths = fs::File::create(&list)?;
    let mut checksums = String::new();
    let destination = format!("{}/{name}", session.directory);
    for file in &manifest.files {
        validate(&manifest.root, file)?;
        let path = file.path.to_str().context("fixture path must be UTF-8")?;
        if path.contains(['\n', '\r', '\\']) {
            bail!("fixture path cannot contain line separators or backslashes");
        }
        paths.write_all(path.as_bytes())?;
        paths.write_all(&[0])?;
        checksums.push_str(&format!("{}  {destination}/{path}\n", file.sha256));
    }
    drop(paths);
    let archive = evidence.join(format!("{name}.tar"));
    let status = child::run(
        Command::new("tar")
            .arg("-C")
            .arg(&manifest.root)
            .arg("-cf")
            .arg(&archive)
            .arg("--null")
            .arg("-T")
            .arg(&list),
        Some(cancel),
    )?;
    if !status.success() {
        bail!("archiving {name} failed");
    }
    let remote = format!(
        "/data/local/tmp/kithara-native-{}-{name}.tar",
        evidence
            .parent()
            .and_then(Path::file_name)
            .context("run name")?
            .to_string_lossy()
    );
    let uploaded = (|| {
        let pushed = child::output(
            session.adb().arg("push").arg(&archive).arg(&remote),
            Some(cancel),
            Duration::from_secs(120),
        )?;
        if !pushed.status.success() {
            bail!(
                "pushing {name} failed: {}",
                String::from_utf8_lossy(&pushed.stderr)
            );
        }
        session.control(
            &["run-as", &session.package, "mkdir", "-p", &destination],
            Some(cancel),
        )?;
        session.control(
            &[
                "run-as",
                &session.package,
                "tar",
                "-xf",
                &remote,
                "-C",
                &destination,
            ],
            Some(cancel),
        )?;
        verify_device(session, &checksums, name, cancel)
    })();
    let removed = session.control(&["rm", "-f", &remote], None);
    uploaded?;
    removed?;
    Ok(())
}

fn verify_device(
    session: &Session,
    checksums: &str,
    name: &str,
    cancel: &child::Cancel,
) -> Result<()> {
    let evidence = session
        .evidence
        .parent()
        .context("native evidence directory")?;
    let local = evidence.join(format!("{name}.sha256"));
    fs::write(&local, checksums)?;
    let remote = format!(
        "/data/local/tmp/kithara-native-{}-{name}.sha256",
        evidence
            .parent()
            .and_then(Path::file_name)
            .context("run name")?
            .to_string_lossy()
    );
    let result = (|| {
        let pushed = child::output(
            session.adb().arg("push").arg(&local).arg(&remote),
            Some(cancel),
            Duration::from_secs(30),
        )?;
        if !pushed.status.success() {
            bail!("uploading fixture checksums failed");
        }
        let checked = session.control(
            &["run-as", &session.package, "sha256sum", "-c", &remote],
            Some(cancel),
        )?;
        fs::write(
            evidence.join(format!("{name}.verification.log")),
            checked.stdout,
        )?;
        Ok(())
    })();
    let removed = session.control(&["rm", "-f", &remote], None);
    result?;
    removed?;
    Ok(())
}

fn validate(root: &Path, file: &File) -> Result<()> {
    if file
        .path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
        || file.path.as_os_str().is_empty()
    {
        bail!("fixture path must remain under its namespace");
    }
    let mut path = root.to_owned();
    for part in file.path.components() {
        path.push(part);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            bail!("fixture path contains a symlink");
        }
    }
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() || metadata.len() != file.bytes || sha256(&path)? != file.sha256 {
        bail!("fixture bytes changed: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_manifest_rejects_changed_bytes_and_namespace_escape() {
        let root = tempfile::tempdir().expect("directory");
        fs::write(root.path().join("fixture"), b"abc").expect("file");
        let mut file = File {
            path: "fixture".into(),
            sha256: sha256(&root.path().join("fixture")).expect("digest"),
            bytes: 3,
        };
        validate(root.path(), &file).expect("unchanged");
        fs::write(root.path().join("fixture"), b"xyz").expect("change");
        assert!(validate(root.path(), &file).is_err());
        file.path = "../fixture".into();
        assert!(validate(root.path(), &file).is_err());
    }
}
