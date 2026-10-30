use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::Result;
use tempfile::TempDir;

use super::host_config;

/// Keeps `BuildKit`'s scoped credentials alive until the image build exits.
pub(crate) struct BuildSecrets(TempDir);

impl BuildSecrets {
    pub(crate) fn new(environment: &BTreeMap<String, String>) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let mut contents = String::new();
        for (key, value) in environment {
            contents.push_str(&format!(
                "export {key}='{}'\n",
                value.replace('\'', "'\\''")
            ));
        }
        contents.push_str("export RUSTC_WRAPPER=kache CARGO_INCREMENTAL=0 KACHE_HOST_CONFIG=/run/secrets/kache_host KACHE_RUNTIME_DIR=/tmp/kithara-kache/runtime\n");
        fs::write(directory.path().join("environment"), contents)?;
        fs::write(
            directory.path().join("host.toml"),
            host_config(environment, Path::new("/tmp/kithara-kache/store"), "1G")?,
        )?;
        Ok(Self(directory))
    }

    pub(crate) fn apply(&self, command: &mut Command) {
        for (id, file) in [("cache_env", "environment"), ("kache_host", "host.toml")] {
            command.arg("--secret").arg(format!(
                "id={id},src={}",
                self.0.path().join(file).display()
            ));
        }
    }

    pub(crate) fn directory(&self) -> &Path {
        self.0.path()
    }
}
