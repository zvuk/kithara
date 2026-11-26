use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};

use super::{permissions, profile::LinuxHost};
use crate::{ci::config::CiPins, consts};

/// A matching executable, host profile and tool contract for future admission.
pub(super) struct Deployment {
    pub(super) binary: PathBuf,
    pub(super) profile: PathBuf,
    pub(super) pins: PathBuf,
}

impl Deployment {
    fn at(root: &Path) -> Self {
        Self {
            binary: root.join("kithara-ci"),
            profile: root.join("linux-host.toml"),
            pins: root.join("pins.toml"),
        }
    }
}

pub(super) fn stage(host: &LinuxHost, pins: &CiPins, executable: &Path) -> Result<Deployment> {
    let binary = fs::read(executable).context("read host executable for deployment")?;
    let identity = serde_json::to_vec(&(hex::encode(Sha256::digest(&binary)), host, pins))?;
    let directory = host
        .cache_root
        .join("services")
        .join(hex::encode(Sha256::digest(identity)));
    let profile = toml::to_string(host)?;
    let contract = toml::to_string(pins)?;
    let files = [
        ("kithara-ci", binary.as_slice(), consts::EXECUTABLE),
        ("linux-host.toml", profile.as_bytes(), consts::OWNER_ONLY),
        ("pins.toml", contract.as_bytes(), consts::WORLD_READABLE),
    ];
    if directory.exists() {
        for (name, contents, _) in files {
            ensure!(
                fs::read(directory.join(name))? == contents,
                "installed deployment differs from its content identity"
            );
        }
        return Ok(Deployment::at(&directory));
    }
    let parent = directory.parent().context("deployment has no parent")?;
    fs::create_dir_all(parent)?;
    let staged = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(parent)?;
    for (name, contents, mode) in files {
        let path = staged.path().join(name);
        fs::write(&path, contents)?;
        permissions::set_mode(&path, mode)?;
    }
    fs::rename(staged.path(), &directory).context("publish complete host deployment")?;
    Ok(Deployment::at(&directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{config::fixture, host::linux::profile::tests::host_fixture};

    #[test]
    fn new_admission_keeps_previous_binary_and_profile_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let mut host = host_fixture();
        host.cache_root = directory.path().join("host");
        let source = directory.path().join("source");
        fs::write(&source, b"old binary").unwrap();
        let first = stage(&host, &fixture().pins, &source).unwrap();
        let profile = fs::read(&first.profile).unwrap();
        fs::write(&source, b"new binary").unwrap();
        host.runners[0].cpus += 1;
        let second = stage(&host, &fixture().pins, &source).unwrap();
        assert_ne!(first.binary, second.binary);
        assert_eq!(fs::read(&first.binary).unwrap(), b"old binary");
        assert_eq!(fs::read(first.profile).unwrap(), profile);
        assert_eq!(fs::read(&second.binary).unwrap(), b"new binary");
        assert_eq!(
            stage(&host, &fixture().pins, &source).unwrap().binary,
            second.binary
        );
    }
}
