use std::{
    collections::BTreeMap,
    env, fs,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::common::tools::ToolsConfig;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracing::info;

use super::{client_environment, current_client_environment};
use crate::ci::kache::{self, KacheJob, LocalStore};

struct Probe {
    root: TempDir,
    environment: BTreeMap<String, String>,
    clients: [KacheJob; 2],
}

impl Probe {
    fn new(environment: BTreeMap<String, String>) -> Result<Self> {
        let root = tempfile::Builder::new().prefix("ci-cache-").tempdir()?;
        let clients = ["a", "b"].map(|name| {
            KacheJob::prepare(
                root.path().join(name),
                Some(LocalStore {
                    directory: &root.path().join(format!("store-{name}")),
                    max_size: "1G",
                }),
            )
        });
        let [a, b] = clients;
        fs::write(
            root.path().join("host.toml"),
            kache::host_config(&environment, &root.path().join("unused"), "1G")?,
        )?;
        Ok(Self {
            root,
            environment,
            clients: [a?, b?],
        })
    }

    fn client(&self, index: usize) -> Command {
        let mut command = Command::new("kache");
        command.env_clear();
        for key in [
            "PATH",
            "HOME",
            "USERPROFILE",
            "RUSTUP_HOME",
            "CARGO_HOME",
            "SYSTEMROOT",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .envs(&self.environment)
            .env("KACHE_HOST_CONFIG", self.root.path().join("host.toml"))
            .envs(self.clients[index].environment(&ToolsConfig::default()));
        command
    }

    fn stats(&self, index: usize, full: bool) -> Result<Value> {
        let mut command = self.client(index);
        command.args(["stats", "--json"]);
        if full {
            command.arg("--full");
        }
        let output = command
            .output()
            .context("read independent kache statistics")?;
        ensure!(
            output.status.success(),
            "kache statistics failed: {}",
            output.status
        );
        serde_json::from_slice(&output.stdout).context("read kache statistics JSON")
    }

    fn build(&self, index: usize, name: &str) -> Result<()> {
        let output = self
            .client(index)
            .args([
                "rustc",
                "--crate-name",
                name,
                "--crate-type",
                "lib",
                "--emit=dep-info,metadata,link",
                "--out-dir",
            ])
            .arg(self.root.path().join(format!("out-{index}")))
            .arg(self.root.path().join("probe.rs"))
            .output()
            .context("run independent kache compiler probe")?;
        ensure!(
            output.status.success(),
            "kache compiler probe failed: {}",
            output.status
        );
        Ok(())
    }

    fn stop(&self, index: usize) -> Result<()> {
        let status = self
            .client(index)
            .args(["daemon", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        ensure!(status.success(), "kache probe could not drain its uploads");
        Ok(())
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        for index in 0..2 {
            let _ = self.stop(index);
        }
    }
}

fn require_remote_hit(stats: &Value) -> Result<()> {
    kache::require_remote(&stats.to_string())?;
    ensure!(
        stats["local_hits"] == 0
            && stats["misses"] == 0
            && stats["remote_hits"].as_u64().unwrap_or(0)
                + stats["prefetch_hits"].as_u64().unwrap_or(0)
                == 1,
        "independent kache consumer did not restore exactly one remote entry"
    );
    Ok(())
}

pub(super) fn run(env_file: Option<&Path>) -> Result<()> {
    let environment = match env_file {
        Some(path) => client_environment(path)?,
        None => current_client_environment()?,
    };
    let probe = Probe::new(environment)?;
    let nonce = hex::encode(Sha256::digest(
        probe.root.path().as_os_str().as_encoded_bytes(),
    ));
    let name = format!("rustfs_probe_{}", &nonce[..16]);
    fs::write(
        probe.root.path().join("probe.rs"),
        format!("pub fn value() -> &'static str {{ \"{nonce}\" }}"),
    )?;
    for index in 0..2 {
        fs::create_dir(probe.root.path().join(format!("out-{index}")))?;
        kache::require_remote(&probe.stats(index, false)?.to_string())?;
    }
    probe.build(0, &name)?;
    let producer = probe.stats(0, false)?;
    ensure!(
        producer["misses"] == 1 && producer["local_hits"] == 0 && producer["remote_hits"] == 0,
        "kache producer did not compile one fresh cache entry"
    );
    probe.stop(0)?;
    probe.build(1, &name)?;
    let consumer = probe.stats(1, false)?;
    require_remote_hit(&consumer)?;
    let full = probe.stats(1, true)?;
    ensure!(
        full["network"]["downloads_ok"].as_u64().unwrap_or(0) > 0,
        "kache consumer reported no successful remote download"
    );
    info!(
        producer_misses = 1,
        consumer_remote_hits = 1,
        consumer_local_hits = 0,
        "independent compiler stores shared an entry through RustFS"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_local_hit_cannot_prove_remote_reuse() {
        let remote = json!({"remote":"s3://scope/kache", "daemon_connected":true,
            "daemon_remote_readonly":false,"local_hits":0,"misses":0,"remote_hits":1,"prefetch_hits":0});
        assert!(require_remote_hit(&remote).is_ok());
        for (field, value) in [("local_hits", 1), ("misses", 1), ("remote_hits", 0)] {
            let mut invalid = remote.clone();
            invalid[field] = json!(value);
            assert!(require_remote_hit(&invalid).is_err());
        }
    }
}
